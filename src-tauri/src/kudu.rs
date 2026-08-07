use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Client for one App Service's Kudu VFS API (https://<app>.scm.azurewebsites.net/api/vfs/).
pub struct KuduClient {
    base: String,
    token: String,
    client: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfsEntry {
    pub name: String,
    pub size: u64,
    pub mtime: String,
    pub mime: String,
}

impl VfsEntry {
    pub fn is_dir(&self) -> bool {
        self.mime == "inode/directory"
    }
}

/// Maps a non-success Kudu status to a message. 401/403 almost always mean the
/// az token lacks access to this app or was issued for the wrong tenant, which a
/// bare status code does not convey.
fn status_error(status: reqwest::StatusCode, url: &str) -> String {
    match status {
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => format!(
            "Access denied ({status}) for {url}. Your Azure account may lack access to this app, \
             or you are signed in to the wrong tenant/subscription. Check with \
             'az account show', switch via 'az login --tenant <id>' or \
             'az account set --subscription <name-or-id>', then retry."
        ),
        _ => format!("Kudu returned {status} for {url}"),
    }
}

pub struct DownloadResult {
    pub bytes_written: u64,
    /// false when a ranged request was answered with 200 (server sent the whole file)
    pub was_partial: bool,
    /// The body was cut short mid-stream and what arrived was salvaged. Those
    /// bytes are still a valid prefix of the remote file, so the next ranged sync
    /// resumes from where the transfer was interrupted.
    pub truncated: bool,
}

/// Why a single download attempt failed, deciding whether the caller retries.
enum AttemptError {
    /// The connection or request send failed before any body arrived, or the
    /// server answered 5xx - retryable, nothing was written to `dest`.
    Send(String),
    /// A non-retryable HTTP status like 401/404 (or a local I/O error) - a fresh
    /// attempt would fail the same way, so it is surfaced immediately.
    Status(String),
    /// The body was cut off mid-stream. `bytes` are flushed to `dest` and form a
    /// valid prefix of the file, so the attempt is retryable and the partial can
    /// be salvaged once retries are exhausted.
    Truncated {
        message: String,
        bytes: u64,
        was_partial: bool,
    },
    /// A resume request was answered with 200: the server ignores Range and
    /// would resend the whole file. Continuing is pointless - the salvaged
    /// prefix is kept and the next sync picks up from it.
    ResumeUnsupported,
}

/// How many times a transient (send or truncated-body) failure is retried before
/// the partial body, if any, is salvaged.
const DOWNLOAD_ATTEMPTS: usize = 3;

/// How long a download waits for the next chunk before treating the connection
/// as stalled. Kudu can hold the connection of a live file open without sending
/// anything; without a timeout that waits forever.
const STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

impl KuduClient {
    pub fn new(kudu_url: &str, token: String) -> Result<Self, String> {
        let base = kudu_url.trim_end_matches('/').to_string();
        if !base.starts_with("https://") {
            return Err(format!("Kudu URL must be https: {base}"));
        }
        // gzip: Kudu compresses log text ~15x and its VFS API cannot serve byte
        // ranges, so live files are always transferred whole - compression is
        // what keeps that transfer (and the data's staleness) short. reqwest
        // decompresses transparently; all sizes stay in decompressed bytes.
        let client = reqwest::Client::builder()
            .gzip(true)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(KuduClient { base, token, client })
    }

    fn vfs_url(&self, path: &str) -> String {
        format!("{}/api/vfs/{}", self.base, path.trim_start_matches('/'))
    }

    /// Lists a directory. `path` is VFS-relative, e.g. "applogs" or "site/wwwroot/App_Data".
    pub async fn list_dir(&self, path: &str) -> Result<Vec<VfsEntry>, String> {
        let url = format!("{}/", self.vfs_url(path).trim_end_matches('/'));
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("Kudu request failed: {e}"))?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(format!("Not found: {url}"));
        }
        if !status.is_success() {
            return Err(status_error(status, &url));
        }
        response
            .json::<Vec<VfsEntry>>()
            .await
            .map_err(|e| format!("Failed to parse VFS listing: {e}"))
    }

    /// Downloads a file to `dest`, streaming to disk. With `range_from`, requests only
    /// bytes from that offset (Kudu VFS answers 206) - used to sync growing files.
    /// A 416 on a ranged request means the file has nothing past the offset and
    /// yields an empty partial result rather than an error.
    /// `expected_size` is the file size from the directory listing: the stream is
    /// cut off at that offset, because a live file that is being appended while it
    /// is read would otherwise keep the transfer running indefinitely. Bytes past
    /// the listed size are left for the next sync's ranged probe.
    /// `on_chunk` receives the running byte count for progress reporting.
    /// `cancel` is polled per chunk: a set flag stops the transfer mid-stream and
    /// what already arrived is returned as a truncated (resumable) result.
    pub async fn download_file(
        &self,
        path: &str,
        dest: &Path,
        range_from: Option<u64>,
        expected_size: Option<u64>,
        cancel: &AtomicBool,
        mut on_chunk: impl FnMut(u64),
    ) -> Result<DownloadResult, String> {
        // The current day's log is written and rotated under the read, so a large
        // transfer is often cut short with a body-decode error. Retry the transient
        // failures (a dropped connection, a truncated body) before giving up.
        //
        // Retries resume rather than restart: bytes salvaged from a truncated
        // attempt stay on disk and the next attempt fetches only the missing tail
        // (Range from `range_from + salvaged`, appending). Without this a live file
        // cut near the end would be re-downloaded in full on every attempt.
        let mut salvaged: u64 = 0;
        // was_partial of the caller's original request (206 vs 200) - it decides
        // whether the caller appends to the .zst. The internal resume ranges used
        // after a truncation do not change that answer.
        let mut original_partial: Option<bool> = None;
        let mut last_error: Option<String> = None;
        for attempt in 0..DOWNLOAD_ATTEMPTS {
            // Cancelled between attempts: do not retry, salvage what arrived
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if attempt > 0 {
                // Back off 0.5s, 1s, 2s, ... between attempts.
                let delay = std::time::Duration::from_millis(500u64 << (attempt - 1));
                tokio::time::sleep(delay).await;
            }
            // Resume offset: the caller's range plus whatever is already salvaged.
            // A full download (range_from None) that was cut becomes ranged on retry.
            let offset = match range_from {
                Some(from) => Some(from + salvaged),
                None if salvaged > 0 => Some(salvaged),
                None => None,
            };
            let append = salvaged > 0;
            let base = salvaged;
            match self
                .try_download(path, dest, offset, expected_size, append, cancel, &mut |n| {
                    on_chunk(base + n)
                })
                .await
            {
                Ok(mut result) => {
                    // The salvaged prefix is already on disk; add it to this
                    // attempt's bytes and report the caller's original partial-ness.
                    result.bytes_written += salvaged;
                    if let Some(partial) = original_partial {
                        result.was_partial = partial;
                    }
                    return Ok(result);
                }
                // A bad status (or local I/O error) will not change on retry.
                Err(AttemptError::Status(message)) => return Err(message),
                // The server cannot resume - stop retrying and keep the salvage.
                Err(AttemptError::ResumeUnsupported) => break,
                Err(AttemptError::Send(message)) => last_error = Some(message),
                Err(AttemptError::Truncated {
                    message,
                    bytes,
                    was_partial,
                }) => {
                    if original_partial.is_none() {
                        original_partial = Some(was_partial);
                    }
                    salvaged += bytes;
                    last_error = Some(message);
                }
            }
        }

        // Retries exhausted. If anything was salvaged across the attempts it is a
        // valid prefix of the remote file, so the next ranged sync resumes past it
        // instead of restarting from zero.
        if salvaged > 0 {
            Ok(DownloadResult {
                bytes_written: salvaged,
                was_partial: original_partial.unwrap_or_else(|| range_from.is_some()),
                truncated: true,
            })
        } else if cancel.load(Ordering::Relaxed) {
            Err("cancelled before any data arrived".to_string())
        } else {
            Err(last_error.unwrap_or_else(|| "Download failed with no attempts".to_string()))
        }
    }

    /// A single download attempt. Streams the body to `dest`; on a mid-stream cut
    /// it flushes and reports the salvageable byte count via `AttemptError::Truncated`.
    async fn try_download(
        &self,
        path: &str,
        dest: &Path,
        range_from: Option<u64>,
        expected_size: Option<u64>,
        append: bool,
        cancel: &AtomicBool,
        on_chunk: &mut impl FnMut(u64),
    ) -> Result<DownloadResult, AttemptError> {
        let url = self.vfs_url(path);
        let mut request = self.client.get(&url).bearer_auth(&self.token);
        if let Some(from) = range_from {
            request = request.header(reqwest::header::RANGE, format!("bytes={from}-"));
        }

        let response = request
            .send()
            .await
            .map_err(|e| AttemptError::Send(format!("Kudu download failed: {e}")))?;

        let status = response.status();
        // Nothing past the requested offset - the remote file has not grown. When
        // resuming (append) the salvaged prefix already on disk is what we keep, so
        // do not recreate `dest`; only a fresh probe starts an empty file.
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && range_from.is_some() {
            if !append {
                std::fs::File::create(dest).map_err(|e| {
                    AttemptError::Status(format!("Cannot create {}: {e}", dest.display()))
                })?;
            }
            return Ok(DownloadResult {
                bytes_written: 0,
                was_partial: true,
                truncated: false,
            });
        }
        // Kudu answers 5xx while the scm site restarts or scales; a retry
        // moments later usually succeeds. No body arrived, so like a failed
        // send there is nothing to salvage.
        if status.is_server_error() {
            return Err(AttemptError::Send(status_error(status, &url)));
        }
        if !status.is_success() {
            return Err(AttemptError::Status(status_error(status, &url)));
        }
        let was_partial = status == reqwest::StatusCode::PARTIAL_CONTENT;

        // A resume answered with 200 restarts at byte zero: the server ignores
        // Range and re-sending everything before the salvaged prefix can take
        // minutes with nothing to show for it. Keep the salvage instead - the
        // next sync continues from it.
        if append && !was_partial {
            return Err(AttemptError::ResumeUnsupported);
        }

        // How many bytes of this response to keep: up to the listed file size,
        // counted from where the response starts in the remote file (a 206 honors
        // the requested offset, a 200 restarts from zero). Without the cap a live
        // file appended while it is read keeps the stream open indefinitely.
        let cap = expected_size.map(|size| {
            let start = if was_partial { range_from.unwrap_or(0) } else { 0 };
            size.saturating_sub(start)
        });

        // Resuming appends the missing tail to the salvaged prefix; a fresh attempt
        // starts (or replaces) the file.
        let file = if append {
            std::fs::OpenOptions::new()
                .append(true)
                .open(dest)
                .map_err(|e| AttemptError::Status(format!("Cannot open {}: {e}", dest.display())))?
        } else {
            std::fs::File::create(dest)
                .map_err(|e| AttemptError::Status(format!("Cannot create {}: {e}", dest.display())))?
        };
        let mut writer = std::io::BufWriter::new(file);
        let mut bytes_written: u64 = 0;

        let mut stream = response.bytes_stream();
        loop {
            // A stalled connection (no data at all for the timeout) is treated
            // like a cut body: what arrived is salvaged and the retry resumes.
            let next = match tokio::time::timeout(STALL_TIMEOUT, stream.next()).await {
                Ok(next) => next,
                Err(_) => {
                    let _ = writer.flush();
                    return Err(AttemptError::Truncated {
                        message: format!(
                            "Download stalled - no data for {}s",
                            STALL_TIMEOUT.as_secs()
                        ),
                        bytes: bytes_written,
                        was_partial,
                    });
                }
            };
            let Some(chunk) = next else { break };
            // The user cancelled: stop mid-stream. The bytes already on disk are
            // a valid prefix, so they are kept and the next sync resumes past them.
            if cancel.load(Ordering::Relaxed) {
                writer
                    .flush()
                    .map_err(|e| AttemptError::Status(e.to_string()))?;
                return Ok(DownloadResult {
                    bytes_written,
                    was_partial,
                    truncated: true,
                });
            }
            let mut chunk = match chunk {
                Ok(chunk) => chunk,
                // The body was cut short. Flush what arrived so the caller can keep
                // it as a resumable prefix rather than discarding the whole transfer.
                Err(e) => {
                    let _ = writer.flush();
                    return Err(AttemptError::Truncated {
                        message: format!("Download stream error: {e}"),
                        bytes: bytes_written,
                        was_partial,
                    });
                }
            };
            if let Some(cap) = cap {
                let remaining = cap.saturating_sub(bytes_written);
                if (chunk.len() as u64) > remaining {
                    chunk = chunk.slice(..remaining as usize);
                }
            }
            writer
                .write_all(&chunk)
                .map_err(|e| AttemptError::Status(format!("Write to {} failed: {e}", dest.display())))?;
            bytes_written += chunk.len() as u64;
            on_chunk(bytes_written);
            // Everything the listing promised has arrived - stop reading; dropping
            // the stream closes the connection even if the file has grown since.
            if cap.is_some_and(|cap| bytes_written >= cap) {
                break;
            }
        }
        writer
            .flush()
            .map_err(|e| AttemptError::Status(e.to_string()))?;

        Ok(DownloadResult {
            bytes_written,
            was_partial,
            truncated: false,
        })
    }
}

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
        // Keep full transfers small when the endpoint ignores Range. reqwest
        // decompresses gzip transparently; all stored sizes are decoded bytes.
        let client = reqwest::Client::builder()
            .gzip(true)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(KuduClient {
            base,
            token,
            client,
        })
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
    /// The directory size is only a progress estimate. Read the complete HTTP
    /// response, whose framing defines its end; an older listing must never cut
    /// off data the server is already sending, including decoded gzip content.
    /// `on_chunk` receives the running byte count for progress reporting.
    /// `cancel` is polled per chunk: a set flag stops the transfer mid-stream and
    /// what already arrived is returned as a truncated (resumable) result.
    pub async fn download_file(
        &self,
        path: &str,
        dest: &Path,
        range_from: Option<u64>,
        _expected_size: Option<u64>,
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
            let offset = if salvaged > 0 {
                let start = if original_partial == Some(true) {
                    range_from.unwrap_or(0)
                } else {
                    0
                };
                Some(start + salvaged)
            } else {
                range_from
            };
            match self
                .try_download(path, dest, offset, salvaged, cancel, &mut on_chunk)
                .await
            {
                Ok(mut result) => {
                    // The salvaged prefix is already on disk; add it to this
                    // attempt's bytes and report the caller's original partial-ness.
                    if salvaged > 0 && result.was_partial {
                        result.bytes_written += salvaged;
                        result.was_partial = original_partial.unwrap_or(result.was_partial);
                    }
                    return Ok(result);
                }
                // A bad status (or local I/O error) will not change on retry.
                Err(AttemptError::Status(message)) => return Err(message),
                Err(AttemptError::Send(message)) => last_error = Some(message),
                Err(AttemptError::Truncated {
                    message,
                    bytes,
                    was_partial,
                }) => {
                    if salvaged > 0 && was_partial {
                        salvaged += bytes;
                    } else {
                        // A 200 replaces the prefix and restarts from byte zero.
                        original_partial = Some(was_partial);
                        salvaged = bytes;
                    }
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
        resume_bytes: u64,
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
        let append = resume_bytes > 0;
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

        // A server that ignores Range sends a fresh full response. Finish it in
        // this sync, replacing the salvaged prefix rather than deferring to the user.
        let append = append && was_partial;

        // Resuming appends the missing tail to the salvaged prefix; a fresh attempt
        // starts (or replaces) the file.
        let file = if append {
            std::fs::OpenOptions::new()
                .append(true)
                .open(dest)
                .map_err(|e| AttemptError::Status(format!("Cannot open {}: {e}", dest.display())))?
        } else {
            std::fs::File::create(dest).map_err(|e| {
                AttemptError::Status(format!("Cannot create {}: {e}", dest.display()))
            })?
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
            let chunk = match chunk {
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
            writer.write_all(&chunk).map_err(|e| {
                AttemptError::Status(format!("Write to {} failed: {e}", dest.display()))
            })?;
            bytes_written += chunk.len() as u64;
            on_chunk(bytes_written + if append { resume_bytes } else { 0 });
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut bytes =
            format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n").into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    async fn server(responses: Vec<Vec<u8>>) -> (KuduClient, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                }
                requests.push(String::from_utf8(request).unwrap().to_lowercase());
                socket.write_all(&response).await.unwrap();
                socket.shutdown().await.unwrap();
            }
            requests
        });
        // Only the local test server bypasses the production HTTPS requirement.
        let client = KuduClient {
            base,
            token: "test-token".into(),
            client: reqwest::Client::builder()
                .gzip(true)
                .no_proxy()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        (client, task)
    }

    fn destination(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("loglooker-kudu-{}-{name}.log", std::process::id()))
    }

    #[tokio::test]
    async fn downloads_the_complete_response_despite_a_stale_listing() {
        for (name, status, headers, body, offset, expected) in [
            (
                "full",
                "200 OK",
                "Content-Length: 16\r\n",
                "0123456789ABCDEF",
                None,
                "0123456789ABCDEF",
            ),
            (
                "tail",
                "206 Partial Content",
                "Content-Length: 6\r\nContent-Range: bytes 10-15/16\r\n",
                "ABCDEF",
                Some(10),
                "ABCDEF",
            ),
            (
                "chunked",
                "200 OK",
                "Transfer-Encoding: chunked\r\n",
                "10\r\n0123456789ABCDEF\r\n0\r\n\r\n",
                None,
                "0123456789ABCDEF",
            ),
        ] {
            let (client, task) = server(vec![response(status, headers, body.as_bytes())]).await;
            let dest = destination(name);
            let result = client
                .download_file(
                    "today.log",
                    &dest,
                    offset,
                    Some(10),
                    &AtomicBool::new(false),
                    |_| {},
                )
                .await
                .unwrap();
            assert_eq!(std::fs::read_to_string(&dest).unwrap(), expected, "{name}");
            assert_eq!(result.bytes_written, expected.len() as u64);
            assert!(!result.truncated);
            task.await.unwrap();
            std::fs::remove_file(dest).unwrap();
        }
    }

    #[tokio::test]
    async fn retries_a_cut_download_when_the_server_ignores_range() {
        let (client, task) = server(vec![
            response("200 OK", "Content-Length: 16\r\n", b"01234"),
            response("200 OK", "Content-Length: 16\r\n", b"0123456789ABCDEF"),
        ])
        .await;
        let dest = destination("retry-full");
        let result = client
            .download_file(
                "today.log",
                &dest,
                None,
                Some(16),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();
        assert!(!result.truncated, "the same sync must finish the retry");
        assert!(!result.was_partial);
        assert_eq!(result.bytes_written, 16);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "0123456789ABCDEF");
        let requests = task.await.unwrap();
        assert!(requests[1].contains("range: bytes=5-"));
        std::fs::remove_file(dest).unwrap();
    }

    #[tokio::test]
    async fn downloads_all_decoded_gzip_bytes_despite_a_stale_listing() {
        // gzip encoding of "0123456789ABCDEF", including its checksum and trailer.
        let gzip = [
            31, 139, 8, 0, 0, 0, 0, 0, 0, 10, 51, 48, 52, 50, 54, 49, 53, 51, 183, 176, 116, 116,
            114, 118, 113, 117, 3, 0, 181, 55, 60, 152, 16, 0, 0, 0,
        ];
        let (client, task) = server(vec![response(
            "200 OK",
            "Content-Encoding: gzip\r\nContent-Length: 36\r\n",
            &gzip,
        )])
        .await;
        let dest = destination("gzip");
        let result = client
            .download_file(
                "today.log",
                &dest,
                None,
                Some(10),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "0123456789ABCDEF");
        assert_eq!(result.bytes_written, 16);
        assert!(!result.truncated);
        task.await.unwrap();
        std::fs::remove_file(dest).unwrap();
    }

    #[tokio::test]
    async fn repeated_full_retries_do_not_duplicate_the_salvaged_prefix() {
        let (client, task) = server(vec![
            response("200 OK", "Content-Length: 16\r\n", b"01234"),
            response("200 OK", "Content-Length: 16\r\n", b"01234567"),
            response("200 OK", "Content-Length: 16\r\n", b"0123456789ABCDEF"),
        ])
        .await;
        let dest = destination("retry-twice");
        let result = client
            .download_file(
                "today.log",
                &dest,
                Some(3),
                Some(16),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();
        assert!(!result.truncated);
        assert!(!result.was_partial);
        assert_eq!(result.bytes_written, 16);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "0123456789ABCDEF");
        let requests = task.await.unwrap();
        assert!(requests[2].contains("range: bytes=8-"));
        std::fs::remove_file(dest).unwrap();
    }

    #[tokio::test]
    async fn a_growing_log_reports_a_transfer_that_remains_incomplete() {
        let prefix = b"2026-09-07 10:00:00 INFO first entry\n";
        let (client, task) = server(vec![
            response("200 OK", "Content-Length: 1000\r\n", prefix);
            DOWNLOAD_ATTEMPTS
        ])
        .await;
        let dir = destination("growing-incomplete");
        std::fs::create_dir_all(&dir).unwrap();
        let service = serde_json::from_value(serde_json::json!({
            "id": "test/app", "name": "app", "environment": "test"
        }))
        .unwrap();
        let file = crate::locations::RemoteLogFile {
            vfs_path: "Log.txt".into(),
            name: "Log.txt".into(),
            size: 1000,
            mtime: "2026-09-07T10:00:00Z".into(),
            date: None,
            instance: None,
        };
        let mut manifest = crate::cache::Manifest::default();
        let summary = crate::growing::sync_growing(
            &crate::source::LogSource::Kudu(client),
            &dir,
            &service,
            &mut manifest,
            vec![file],
            &AtomicBool::new(false),
            |_| {},
        )
        .await
        .unwrap();
        assert!(
            summary
                .warnings
                .iter()
                .any(|warning| warning.contains("transfer interrupted")),
            "partial logs must not be silently reported as complete"
        );
        assert_eq!(manifest.growing["Log.txt"].remote_size, prefix.len() as u64);
        task.await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn resumes_from_zero_based_bytes_after_a_range_was_ignored() {
        let (client, task) = server(vec![
            response("200 OK", "Content-Length: 16\r\n", b"01234"),
            response(
                "206 Partial Content",
                "Content-Length: 11\r\nContent-Range: bytes 5-15/16\r\n",
                b"56789ABCDEF",
            ),
        ])
        .await;
        let dest = destination("retry-offset");
        let result = client
            .download_file(
                "today.log",
                &dest,
                Some(3),
                Some(16),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();
        assert!(!result.truncated);
        assert!(!result.was_partial);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "0123456789ABCDEF");
        let requests = task.await.unwrap();
        assert!(requests[1].contains("range: bytes=5-"), "{}", requests[1]);
        std::fs::remove_file(dest).unwrap();
    }

    #[tokio::test]
    async fn a_resumed_tail_stays_partial_for_the_cache_append() {
        let (client, task) = server(vec![
            response(
                "206 Partial Content",
                "Content-Length: 13\r\nContent-Range: bytes 3-15/16\r\n",
                b"34",
            ),
            response(
                "206 Partial Content",
                "Content-Length: 11\r\nContent-Range: bytes 5-15/16\r\n",
                b"56789ABCDEF",
            ),
        ])
        .await;
        let dest = destination("retry-partial");
        let result = client
            .download_file(
                "today.log",
                &dest,
                Some(3),
                Some(10),
                &AtomicBool::new(false),
                |_| {},
            )
            .await
            .unwrap();
        assert!(!result.truncated);
        assert!(result.was_partial);
        assert_eq!(result.bytes_written, 13);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "3456789ABCDEF");
        let requests = task.await.unwrap();
        assert!(requests[1].contains("range: bytes=5-"));
        std::fs::remove_file(dest).unwrap();
    }
}

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

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
}

impl KuduClient {
    pub fn new(kudu_url: &str, token: String) -> Result<Self, String> {
        let base = kudu_url.trim_end_matches('/').to_string();
        if !base.starts_with("https://") {
            return Err(format!("Kudu URL must be https: {base}"));
        }
        let client = reqwest::Client::builder()
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
    /// bytes from that offset (Kudu VFS answers 206) — used to sync growing files.
    /// A 416 on a ranged request means the file has nothing past the offset and
    /// yields an empty partial result rather than an error.
    /// `on_chunk` receives the running byte count for progress reporting.
    pub async fn download_file(
        &self,
        path: &str,
        dest: &Path,
        range_from: Option<u64>,
        mut on_chunk: impl FnMut(u64),
    ) -> Result<DownloadResult, String> {
        let url = self.vfs_url(path);
        let mut request = self.client.get(&url).bearer_auth(&self.token);
        if let Some(from) = range_from {
            request = request.header(reqwest::header::RANGE, format!("bytes={from}-"));
        }

        let response = request
            .send()
            .await
            .map_err(|e| format!("Kudu download failed: {e}"))?;

        let status = response.status();
        // Nothing past the requested offset — the remote file has not grown
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && range_from.is_some() {
            std::fs::File::create(dest)
                .map_err(|e| format!("Cannot create {}: {e}", dest.display()))?;
            return Ok(DownloadResult {
                bytes_written: 0,
                was_partial: true,
            });
        }
        if !status.is_success() {
            return Err(status_error(status, &url));
        }
        let was_partial = status == reqwest::StatusCode::PARTIAL_CONTENT;

        let file = std::fs::File::create(dest)
            .map_err(|e| format!("Cannot create {}: {e}", dest.display()))?;
        let mut writer = std::io::BufWriter::new(file);
        let mut bytes_written: u64 = 0;

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("Download stream error: {e}"))?;
            writer
                .write_all(&chunk)
                .map_err(|e| format!("Write to {} failed: {e}", dest.display()))?;
            bytes_written += chunk.len() as u64;
            on_chunk(bytes_written);
        }
        writer.flush().map_err(|e| e.to_string())?;

        Ok(DownloadResult {
            bytes_written,
            was_partial,
        })
    }
}

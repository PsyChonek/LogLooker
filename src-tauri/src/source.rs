//! Transports - the ways the app can reach a service's log files.
//!
//! The list is closed on purpose: a pack picks a transport and configures it, it
//! cannot supply code. Dispatch is a plain enum rather than a trait object, so
//! the async methods need no extra machinery and each transport keeps its own
//! quirks (Kudu's Range handling in particular) where they belong.

use crate::kudu::{DownloadResult, KuduClient};
use crate::plugin::SourceDef;
use chrono::{DateTime, Utc};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// One entry of a listed log directory, however the transport found it.
#[derive(Debug, Clone)]
pub struct SourceEntry {
    pub name: String,
    pub size: u64,
    /// Last-write time as text. Only ever compared for equality against the
    /// value a previous sync recorded, so the format matters less than that it
    /// is stable per transport.
    pub mtime: String,
    pub is_dir: bool,
}

pub enum LogSource {
    Kudu(KuduClient),
    LocalFolder(LocalFolder),
}

impl LogSource {
    /// Builds the transport a pack declares against one service's endpoint.
    pub fn new(source: &SourceDef, endpoint: &str, token: Option<String>) -> Result<Self, String> {
        match source {
            SourceDef::Kudu { .. } => {
                let token = token.ok_or("A Kudu source needs an Azure access token")?;
                Ok(LogSource::Kudu(KuduClient::new(endpoint, token)?))
            }
            SourceDef::LocalFolder { root } => Ok(LogSource::LocalFolder(LocalFolder::new(
                root.as_deref(),
                endpoint,
            ))),
        }
    }

    /// Whether this transport needs an Azure token before it can be built, so
    /// the caller only pays for `az account get-access-token` when it is used.
    pub fn needs_token(source: &SourceDef) -> bool {
        matches!(source, SourceDef::Kudu { .. })
    }

    /// Lists a directory relative to the service's root. `.` is the root itself.
    pub async fn list_dir(&self, path: &str) -> Result<Vec<SourceEntry>, String> {
        match self {
            LogSource::Kudu(client) => Ok(client
                .list_dir(path)
                .await?
                .into_iter()
                .map(|entry| SourceEntry {
                    is_dir: entry.is_dir(),
                    name: entry.name,
                    size: entry.size,
                    mtime: entry.mtime,
                })
                .collect()),
            LogSource::LocalFolder(folder) => folder.list_dir(path),
        }
    }

    /// Copies a file to `dest`. With `range_from`, fetches only the bytes from
    /// that offset - what keeps a growing file from being re-read in full.
    /// `expected_size` is a listing estimate, never a cutoff for newer content.
    /// HTTP transfers consume the response; local copies snapshot the opened file.
    pub async fn download_file(
        &self,
        path: &str,
        dest: &Path,
        range_from: Option<u64>,
        expected_size: Option<u64>,
        cancel: &AtomicBool,
        on_chunk: impl FnMut(u64),
    ) -> Result<DownloadResult, String> {
        match self {
            LogSource::Kudu(client) => {
                client
                    .download_file(path, dest, range_from, expected_size, cancel, on_chunk)
                    .await
            }
            LogSource::LocalFolder(folder) => {
                folder.copy_file(path, dest, range_from, expected_size, cancel, on_chunk)
            }
        }
    }
}

/// A directory on this machine or a mounted share. Reads only - the app never
/// writes into a source.
pub struct LocalFolder {
    base: PathBuf,
}

/// Copy buffer. Large enough that a multi-GB log is not read a page at a time,
/// small enough that cancellation and progress stay responsive.
const COPY_CHUNK: usize = 256 * 1024;

impl LocalFolder {
    pub fn new(root: Option<&str>, endpoint: &str) -> Self {
        let path = PathBuf::from(endpoint);
        // A relative endpoint hangs off the pack's root; an absolute one stands
        // alone, so a pack root cannot silently redirect an explicit path.
        let base = match root {
            Some(root) if path.is_relative() => PathBuf::from(root).join(path),
            _ => path,
        };
        LocalFolder { base }
    }

    /// Resolves a pack-declared directory under the service root, refusing
    /// anything that climbs out of it - a location's `dir` comes from a pack
    /// file, which is not something to trust with `..`.
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let relative = path.trim().trim_start_matches(['/', '\\']);
        if relative == "." || relative.is_empty() {
            return Ok(self.base.clone());
        }
        let mut resolved = self.base.clone();
        for part in relative.split(['/', '\\']).filter(|p| !p.is_empty()) {
            if part == ".." {
                return Err(format!(
                    "Log location \"{path}\" climbs out of the source folder"
                ));
            }
            if part == "." {
                continue;
            }
            resolved.push(part);
        }
        Ok(resolved)
    }

    fn list_dir(&self, path: &str) -> Result<Vec<SourceEntry>, String> {
        let dir = self.resolve(path)?;
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;

        let mut out = Vec::new();
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .map(|time| DateTime::<Utc>::from(time).to_rfc3339())
                .unwrap_or_default();
            out.push(SourceEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                size: meta.len(),
                mtime,
                is_dir: meta.is_dir(),
            });
        }
        Ok(out)
    }

    fn copy_file(
        &self,
        path: &str,
        dest: &Path,
        range_from: Option<u64>,
        _expected_size: Option<u64>,
        cancel: &AtomicBool,
        mut on_chunk: impl FnMut(u64),
    ) -> Result<DownloadResult, String> {
        let source = self.resolve(path)?;
        let mut file = std::fs::File::open(&source)
            .map_err(|e| format!("Cannot open {}: {e}", source.display()))?;
        let len = file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| format!("Cannot stat {}: {e}", source.display()))?;

        if let Some(from) = range_from {
            // Nothing past the offset: the file has not grown. An empty partial
            // is the same answer Kudu gives with a 416, so the caller's
            // append-or-replace logic needs no special case here.
            if from >= len {
                std::fs::File::create(dest)
                    .map_err(|e| format!("Cannot create {}: {e}", dest.display()))?;
                return Ok(DownloadResult {
                    bytes_written: 0,
                    was_partial: true,
                    truncated: false,
                });
            }
            file.seek(SeekFrom::Start(from))
                .map_err(|e| format!("Cannot seek {}: {e}", source.display()))?;
        }

        // Snapshot the actual file at open time, so an old listing does not omit
        // existing data and continuous writes cannot prolong the copy indefinitely.
        let cap = len.saturating_sub(range_from.unwrap_or(0));
        let out = std::fs::File::create(dest)
            .map_err(|e| format!("Cannot create {}: {e}", dest.display()))?;
        let mut writer = std::io::BufWriter::new(out);
        let mut buffer = vec![0u8; COPY_CHUNK];
        let mut written: u64 = 0;

        loop {
            if cancel.load(Ordering::Relaxed) {
                writer.flush().map_err(|e| e.to_string())?;
                // What is on disk is a valid prefix, so the next sync resumes past it
                return Ok(DownloadResult {
                    bytes_written: written,
                    was_partial: range_from.is_some(),
                    truncated: true,
                });
            }
            let want = cap.saturating_sub(written).min(COPY_CHUNK as u64) as usize;
            if want == 0 {
                break;
            }
            let read = file
                .read(&mut buffer[..want])
                .map_err(|e| format!("Cannot read {}: {e}", source.display()))?;
            if read == 0 {
                break;
            }
            writer
                .write_all(&buffer[..read])
                .map_err(|e| format!("Write to {} failed: {e}", dest.display()))?;
            written += read as u64;
            on_chunk(written);
        }
        writer.flush().map_err(|e| e.to_string())?;

        Ok(DownloadResult {
            bytes_written: written,
            was_partial: range_from.is_some(),
            truncated: written < cap,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("loglooker-source-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_a_local_folder_with_sizes() {
        let dir = temp_dir("list");
        std::fs::write(dir.join("a.log"), "hello").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();

        let folder = LocalFolder::new(None, dir.to_str().unwrap());
        let mut entries = folder.list_dir(".").unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.log");
        assert_eq!(entries[0].size, 5);
        assert!(!entries[0].is_dir);
        assert!(entries[1].is_dir);
    }

    #[test]
    fn resolves_a_relative_endpoint_under_the_pack_root() {
        let folder = LocalFolder::new(Some("C:\\logs"), "app");
        assert_eq!(folder.base, PathBuf::from("C:\\logs").join("app"));
    }

    #[test]
    fn an_absolute_endpoint_ignores_the_pack_root() {
        let folder = LocalFolder::new(Some("C:\\logs"), "D:\\elsewhere");
        assert_eq!(folder.base, PathBuf::from("D:\\elsewhere"));
    }

    #[test]
    fn refuses_a_location_dir_that_climbs_out_of_the_source() {
        let folder = LocalFolder::new(None, "C:\\logs\\app");
        assert!(folder.resolve("../../windows").is_err());
        assert_eq!(folder.resolve(".").unwrap(), PathBuf::from("C:\\logs\\app"));
        assert_eq!(
            folder.resolve("sub/dir").unwrap(),
            PathBuf::from("C:\\logs\\app\\sub\\dir")
        );
    }

    #[test]
    fn copies_a_whole_file() {
        let dir = temp_dir("copy");
        std::fs::write(dir.join("a.log"), "0123456789").unwrap();
        let dest = dir.join("out.bin");

        let folder = LocalFolder::new(None, dir.to_str().unwrap());
        let cancel = AtomicBool::new(false);
        let result = folder
            .copy_file("a.log", &dest, None, Some(10), &cancel, |_| {})
            .unwrap();

        assert_eq!(result.bytes_written, 10);
        assert!(!result.was_partial);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "0123456789");
    }

    /// A growing file syncs by fetching only the tail, the same contract the
    /// remote transport offers with an HTTP Range.
    #[test]
    fn copies_only_the_tail_of_a_grown_file() {
        let dir = temp_dir("range");
        std::fs::write(dir.join("a.log"), "0123456789").unwrap();
        let dest = dir.join("out.bin");

        let folder = LocalFolder::new(None, dir.to_str().unwrap());
        let cancel = AtomicBool::new(false);
        let result = folder
            .copy_file("a.log", &dest, Some(4), Some(10), &cancel, |_| {})
            .unwrap();

        assert_eq!(result.bytes_written, 6);
        assert!(result.was_partial);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "456789");
    }

    /// An earlier listing must not hide data already present when copying starts.
    #[test]
    fn copies_the_current_file_despite_a_stale_listing() {
        let dir = temp_dir("cap");
        std::fs::write(dir.join("a.log"), "0123456789ABCDEF").unwrap();
        let dest = dir.join("out.bin");

        let folder = LocalFolder::new(None, dir.to_str().unwrap());
        let cancel = AtomicBool::new(false);
        let result = folder
            .copy_file("a.log", &dest, None, Some(10), &cancel, |_| {})
            .unwrap();

        assert_eq!(result.bytes_written, 16);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "0123456789ABCDEF");
    }

    #[test]
    fn stops_at_the_snapshot_size_even_if_the_local_file_keeps_growing() {
        let dir = temp_dir("snapshot");
        let original = vec![b'a'; COPY_CHUNK * 2];
        std::fs::write(dir.join("a.log"), &original).unwrap();
        let dest = dir.join("out.bin");
        let folder = LocalFolder::new(None, dir.to_str().unwrap());
        let result = folder
            .copy_file(
                "a.log",
                &dest,
                None,
                Some(10),
                &AtomicBool::new(false),
                |_| {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(dir.join("a.log"))
                        .unwrap()
                        .write_all(&[b'b'; COPY_CHUNK])
                        .unwrap();
                },
            )
            .unwrap();
        assert_eq!(result.bytes_written, original.len() as u64);
        assert_eq!(std::fs::read(dest).unwrap(), original);
    }

    #[test]
    fn a_file_that_has_not_grown_yields_an_empty_partial() {
        let dir = temp_dir("nogrowth");
        std::fs::write(dir.join("a.log"), "0123456789").unwrap();
        let dest = dir.join("out.bin");

        let folder = LocalFolder::new(None, dir.to_str().unwrap());
        let cancel = AtomicBool::new(false);
        let result = folder
            .copy_file("a.log", &dest, Some(10), Some(10), &cancel, |_| {})
            .unwrap();

        assert_eq!(result.bytes_written, 0);
        assert!(result.was_partial);
        assert!(!result.truncated);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "");
    }
}

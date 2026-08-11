use crate::cache;
use crate::config::ServiceConfig;
use crate::memcache::MemCache;
use crate::search::Matcher;
use serde::Serialize;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Where an open raw file reads its lines from.
enum Backing {
    /// The memory cache already holds the decompressed text - the viewer indexes
    /// it in place, so opening costs neither a decompression nor a temp file.
    Memory(Arc<[u8]>),
    /// The zstd file has no random access, so it is decompressed once into a
    /// temp file that the viewer can seek in.
    TempFile(PathBuf),
}

/// A cached log file opened for raw viewing, indexed by line start so the
/// viewer can jump to any line without reading the ones before it.
pub struct OpenFile {
    pub service_id: String,
    pub file: String,
    backing: Backing,
    line_offsets: Vec<u64>,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawFileInfo {
    pub service_id: String,
    pub file: String,
    pub total_lines: usize,
    pub size_bytes: u64,
}

impl OpenFile {
    pub fn info(&self) -> RawFileInfo {
        RawFileInfo {
            service_id: self.service_id.clone(),
            file: self.file.clone(),
            total_lines: self.line_offsets.len(),
            size_bytes: self.size_bytes,
        }
    }

    /// Lines `[offset, offset + limit)`, 0-based. A range past the end yields
    /// fewer (or no) lines rather than an error - the viewer may overscan.
    pub fn lines(&self, offset: usize, limit: usize) -> Result<Vec<String>, String> {
        let end = offset.saturating_add(limit).min(self.line_offsets.len());
        if offset >= end {
            return Ok(Vec::new());
        }

        match &self.backing {
            Backing::Memory(text) => {
                let mut lines = Vec::with_capacity(end - offset);
                for index in offset..end {
                    let from = self.line_offsets[index] as usize;
                    let to = self
                        .line_offsets
                        .get(index + 1)
                        .map_or(text.len(), |next| *next as usize);
                    let line = String::from_utf8_lossy(&text[from..to]);
                    lines.push(line.trim_end_matches(['\r', '\n']).to_string());
                }
                Ok(lines)
            }
            Backing::TempFile(path) => {
                let file = std::fs::File::open(path)
                    .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
                let mut reader = BufReader::new(file);
                reader
                    .seek(SeekFrom::Start(self.line_offsets[offset]))
                    .map_err(|e| format!("Seek failed in {}: {e}", self.file))?;

                let mut lines = Vec::with_capacity(end - offset);
                let mut buffer = Vec::new();
                for _ in offset..end {
                    buffer.clear();
                    let read = reader
                        .read_until(b'\n', &mut buffer)
                        .map_err(|e| format!("Read failed in {}: {e}", self.file))?;
                    if read == 0 {
                        break;
                    }
                    let line = String::from_utf8_lossy(&buffer);
                    lines.push(line.trim_end_matches(['\r', '\n']).to_string());
                }
                Ok(lines)
            }
        }
    }

    /// Scans the whole decompressed file for lines matching `matcher`. At most
    /// `limit` matches are returned (with their text truncated for display);
    /// `total` counts them all so the UI can say "showing N of M".
    pub fn search(&self, matcher: &Matcher, limit: usize) -> Result<RawSearchResult, String> {
        let mut result = RawSearchResult::default();
        let mut line_number: u64 = 0;

        match &self.backing {
            Backing::Memory(text) => {
                for raw in text.split_inclusive(|byte| *byte == b'\n') {
                    line_number += 1;
                    let line = String::from_utf8_lossy(raw);
                    collect_match(&mut result, matcher, limit, line_number, &line);
                }
            }
            Backing::TempFile(path) => {
                let file = std::fs::File::open(path)
                    .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
                let mut reader = BufReader::new(file);
                let mut buffer = Vec::new();
                loop {
                    buffer.clear();
                    let read = reader
                        .read_until(b'\n', &mut buffer)
                        .map_err(|e| format!("Read failed in {}: {e}", self.file))?;
                    if read == 0 {
                        break;
                    }
                    line_number += 1;
                    let line = String::from_utf8_lossy(&buffer);
                    collect_match(&mut result, matcher, limit, line_number, &line);
                }
            }
        }
        Ok(result)
    }

    pub fn cleanup(&self) {
        if let Backing::TempFile(path) = &self.backing {
            std::fs::remove_file(path).ok();
        }
    }
}

fn collect_match(
    result: &mut RawSearchResult,
    matcher: &Matcher,
    limit: usize,
    line_number: u64,
    line: &str,
) {
    let line = line.trim_end_matches(['\r', '\n']);
    if !matcher.matches(line) {
        return;
    }
    result.total += 1;
    if result.matches.len() < limit {
        result.matches.push(RawSearchMatch {
            line_number,
            text: line.chars().take(MATCH_TEXT_CHARS).collect(),
        });
    }
}

/// Matched lines are only previews for the match list - the viewer shows the
/// full line - so their text is capped to keep huge SQL/HTML lines off IPC.
const MATCH_TEXT_CHARS: usize = 500;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSearchMatch {
    pub line_number: u64,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSearchResult {
    pub matches: Vec<RawSearchMatch>,
    pub total: usize,
}

/// Opens the service's cached copy of `file` for raw viewing and indexes its
/// lines. When the memory cache holds the text, it is indexed in place; only
/// otherwise is it decompressed into a temp file.
/// Blocking and CPU-bound; call via spawn_blocking.
pub fn open(service: &ServiceConfig, file: &str, cache: &MemCache) -> Result<OpenFile, String> {
    let dir = cache::service_dir_path(&service.environment, &service.name)?;
    let local_name = format!("{file}.zst");
    let manifest = cache::load_manifest(&dir);
    let entry = manifest
        .entries
        .values()
        .find(|entry| entry.local_name == local_name)
        .ok_or_else(|| format!("{file} is not cached for {}", service.name))?;

    let path = dir.join(&local_name);
    // Held in memory, or small enough to hold: index it there. The temp file is
    // only for logs too big for the budget - the one case where writing the text
    // out beats keeping it.
    let (backing, line_offsets, size_bytes) = match cache.load(&path, entry.remote_size)? {
        Some(text) => {
            let line_offsets = index_memory(&text);
            let size_bytes = text.len() as u64;
            (Backing::Memory(text), line_offsets, size_bytes)
        }
        None => {
            let target = temp_path(&service.id, file)?;
            decompress(&path, &target)?;
            let line_offsets = index_lines(&target)?;
            let size_bytes = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
            (Backing::TempFile(target), line_offsets, size_bytes)
        }
    };

    Ok(OpenFile {
        service_id: service.id.clone(),
        file: file.to_string(),
        backing,
        line_offsets,
        size_bytes,
    })
}

fn index_memory(text: &[u8]) -> Vec<u64> {
    let mut offsets = Vec::new();
    let mut offset: u64 = 0;
    for line in text.split_inclusive(|byte| *byte == b'\n') {
        offsets.push(offset);
        offset += line.len() as u64;
    }
    offsets
}

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("LogLooker").join("raw")
}

/// Removes leftover decompressed files (e.g. from a raw-view window that was
/// killed before it could clean up). Call once at startup.
pub fn clear_temp_dir() {
    std::fs::remove_dir_all(temp_dir()).ok();
}

/// Service ids and file names carry `/`, `\` and `:`; flatten them into one
/// safe file name. A per-process counter keeps two opens of the same file
/// (e.g. in separate windows) from sharing a temp file.
fn temp_path(service_id: &str, file: &str) -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create temp dir: {e}"))?;
    let safe: String = format!("{service_id}-{file}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!("{safe}.{n}.log")))
}

fn decompress(source: &Path, target: &Path) -> Result<(), String> {
    let raw = std::fs::File::open(source)
        .map_err(|e| format!("Cannot open {}: {e}", source.display()))?;
    let output = std::fs::File::create(target)
        .map_err(|e| format!("Cannot create {}: {e}", target.display()))?;
    zstd::stream::copy_decode(BufReader::new(raw), output)
        .map_err(|e| format!("zstd decompression failed for {}: {e}", source.display()))
}

fn index_lines(path: &Path) -> Result<Vec<u64>, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut offsets = Vec::new();
    let mut offset: u64 = 0;
    let mut buffer = Vec::new();

    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|e| format!("Read failed in {}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        offsets.push(offset);
        offset += read as u64;
    }
    Ok(offsets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_plain(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    fn open_file(path: PathBuf) -> OpenFile {
        let line_offsets = index_lines(&path).unwrap();
        let size_bytes = std::fs::metadata(&path).unwrap().len();
        OpenFile {
            service_id: "test/Svc".into(),
            file: "Log.txt".into(),
            backing: Backing::TempFile(path),
            line_offsets,
            size_bytes,
        }
    }

    /// The same file backed by the memory cache instead of a temp file - the
    /// viewer must see exactly the same lines either way.
    fn open_in_memory(path: &Path) -> OpenFile {
        let text: Arc<[u8]> = Arc::from(std::fs::read(path).unwrap());
        let line_offsets = index_memory(&text);
        let size_bytes = text.len() as u64;
        OpenFile {
            service_id: "test/Svc".into(),
            file: "Log.txt".into(),
            backing: Backing::Memory(text),
            line_offsets,
            size_bytes,
        }
    }

    #[test]
    fn serves_a_window_of_lines() {
        let dir = std::env::temp_dir().join("loglooker-rawfile-window");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_plain(&dir, "window.log", "one\r\ntwo\nthree\nfour\n");
        let open = open_file(path);

        assert_eq!(open.info().total_lines, 4);
        assert_eq!(open.lines(1, 2).unwrap(), vec!["two", "three"]);
        // Line endings are stripped, including CRLF
        assert_eq!(open.lines(0, 1).unwrap(), vec!["one"]);
        open.cleanup();
    }

    #[test]
    fn a_file_read_from_memory_serves_the_same_lines_and_matches() {
        let dir = std::env::temp_dir().join("loglooker-rawfile-memory");
        std::fs::create_dir_all(&dir).unwrap();
        // A trailing line without a newline, and a CRLF one, are the awkward cases
        let path = write_plain(&dir, "memory.log", "alpha\r\nBETA\nalpha beta\nlast");
        let from_disk = open_file(path.clone());
        let from_memory = open_in_memory(&path);

        assert_eq!(from_memory.info().total_lines, from_disk.info().total_lines);
        assert_eq!(from_memory.info().size_bytes, from_disk.info().size_bytes);
        assert_eq!(from_memory.lines(0, 10).unwrap(), from_disk.lines(0, 10).unwrap());
        assert_eq!(from_memory.lines(2, 1).unwrap(), vec!["alpha beta"]);
        assert_eq!(from_memory.lines(3, 1).unwrap(), vec!["last"]);
        assert!(from_memory.lines(9, 5).unwrap().is_empty());

        let matcher = Matcher::new("beta", false, false).unwrap();
        let memory_hits = from_memory.search(&matcher, 10).unwrap();
        let disk_hits = from_disk.search(&matcher, 10).unwrap();
        assert_eq!(memory_hits.total, disk_hits.total);
        assert_eq!(memory_hits.matches[0].line_number, 2);
        assert_eq!(memory_hits.matches[1].text, "alpha beta");

        // Nothing to clean up for an in-memory file; the temp one still goes
        from_memory.cleanup();
        assert!(path.exists(), "cleanup of a memory-backed file touches no file");
        from_disk.cleanup();
    }

    #[test]
    fn reads_past_the_end_without_erroring() {
        let dir = std::env::temp_dir().join("loglooker-rawfile-end");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_plain(&dir, "end.log", "one\ntwo\n");
        let open = open_file(path);

        assert_eq!(open.lines(1, 100).unwrap(), vec!["two"]);
        assert!(open.lines(50, 10).unwrap().is_empty());
        open.cleanup();
    }

    #[test]
    fn searches_lines_with_limit_and_total() {
        let dir = std::env::temp_dir().join("loglooker-rawfile-search");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_plain(&dir, "search.log", "alpha\nBETA\nalpha beta\ngamma\n");
        let open = open_file(path);

        let matcher = Matcher::new("beta", false, false).unwrap();
        let result = open.search(&matcher, 10).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.matches[0].line_number, 2);
        assert_eq!(result.matches[1].text, "alpha beta");

        let matcher = Matcher::new("^alpha", true, true).unwrap();
        let result = open.search(&matcher, 1).unwrap();
        assert_eq!(result.total, 2);
        assert_eq!(result.matches.len(), 1);
        open.cleanup();
    }

    #[test]
    fn handles_a_file_without_a_trailing_newline() {
        let dir = std::env::temp_dir().join("loglooker-rawfile-noeol");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_plain(&dir, "noeol.log", "one\nlast line");
        let open = open_file(path);

        assert_eq!(open.info().total_lines, 2);
        assert_eq!(open.lines(1, 1).unwrap(), vec!["last line"]);
        open.cleanup();
    }
}

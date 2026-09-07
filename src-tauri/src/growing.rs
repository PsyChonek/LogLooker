//! Per-day accumulation of a single growing log file (FrameworkAppData's
//! App_Data/Log.txt).
//!
//! The remote file is rotated by the app that writes it: every so often the
//! oldest day is dropped and the file starts over shorter. A cache mirroring
//! the file one-to-one loses those days on the next sync. Instead, downloaded
//! content is split by the day of each line into dated segments
//! (Log-YYYY-MM-DD.txt.zst) and a sync only ever appends lines newer than what
//! is already cached - days the remote file has since dropped stay cached.
//!
//! `Manifest::growing` tracks the remote file itself: how many bytes were
//! consumed into segments, the last consumed timestamp, and the tail of the
//! consumed bytes. A sync re-downloads that tail (HTTP Range) and compares:
//! a match proves the remote file still continues the cached content and only
//! the new bytes are consumed; a mismatch (or a shrunken file) means it was
//! rotated, so the whole file is downloaded and merged by timestamp - lines at
//! or before the last cached timestamp are dropped as already cached.

use crate::cache::{self, CachedFile, Manifest, SyncProgress, SyncSummary};
use crate::config::ServiceConfig;
use crate::locations::RemoteLogFile;
use crate::source::LogSource;
use crate::search::parse_timestamp;
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Bytes of consumed content kept for the continuity check. Long enough that a
/// match cannot be a coincidence, short enough to live in the manifest.
const TAIL_LEN: usize = 2048;

/// Sync state of one growing remote file whose lines live in per-day segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrowingState {
    /// Bytes of the remote file already consumed into day segments - the offset
    /// the next ranged sync continues from
    pub remote_size: u64,
    pub remote_mtime: String,
    /// Timestamp and day of the last consumed timestamped line. The day dates
    /// continuation lines at the start of the next appended chunk.
    pub last_ts: Option<NaiveDateTime>,
    pub last_day: Option<NaiveDate>,
    /// Hex-encoded last bytes of the consumed content; a ranged sync re-downloads
    /// this span and a mismatch reveals the file was rotated
    pub tail_hex: String,
}

fn segment_key(vfs_path: &str, day: NaiveDate) -> String {
    format!("{vfs_path}@{day}")
}

/// Splits a `{vfs_path}@{day}` segment key back into its parts. Keys of plain
/// per-file entries (dated files mirrored one-to-one) yield None.
pub(crate) fn split_segment_key(key: &str) -> Option<(&str, NaiveDate)> {
    let (path, day) = key.rsplit_once('@')?;
    Some((path, day.parse().ok()?))
}

/// Log-2026-07-15.txt.zst - dated like the per-day profiles, .txt like the source
fn segment_file_name(day: NaiveDate) -> String {
    format!("Log-{day}.txt.zst")
}

/// Syncs a single growing remote file into per-day segments. Replaces the
/// generic per-file sync for the FrameworkAppData profile.
pub async fn sync_growing(
    source: &LogSource,
    dir: &Path,
    service: &ServiceConfig,
    manifest: &mut Manifest,
    files: Vec<RemoteLogFile>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(SyncProgress),
) -> Result<SyncSummary, String> {
    let mut summary = SyncSummary {
        service_id: service.id.clone(),
        files_total: files.len(),
        files_downloaded: 0,
        files_skipped: 0,
        files_failed: 0,
        bytes_downloaded: 0,
        warnings: Vec::new(),
    };
    let Some(file) = files.into_iter().next() else {
        return Ok(summary);
    };

    migrate_legacy(dir, manifest, &file)?;

    let state = manifest.growing.get(&file.vfs_path).cloned();
    let temp = dir.join(format!("{}.download", file.name));

    let mut emit = |bytes: u64, phase: &str| {
        on_progress(SyncProgress {
            service_id: service.id.clone(),
            file_name: file.name.clone(),
            file_index: 1,
            file_count: 1,
            bytes_downloaded: bytes,
            // The listing can lag behind the content actually transferred.
            total_bytes: file.size.max(bytes),
            state: phase.into(),
        })
    };

    // First try to continue past the cached bytes: re-download the known tail
    // and the rest. `None` here means the remote file no longer continues the
    // cached content and the whole file must be taken and merged by timestamp.
    let fetch = match &state {
        Some(s) if file.size >= s.remote_size => {
            let tail = hex_decode(&s.tail_hex);
            let overlap = tail.len() as u64;
            let range_from = s.remote_size - overlap;
            let result = match source
                .download_file(&file.vfs_path, &temp, Some(range_from), Some(file.size), cancel, |d| {
                    emit(d, "downloading")
                })
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    summary.files_failed = 1;
                    summary.warnings.push(format!("{}: download failed - {e}", file.name));
                    emit(0, "failed");
                    return Ok(summary);
                }
            };
            if result.was_partial {
                if result.bytes_written >= overlap && file_matches_at(&temp, 0, &tail)? {
                    if result.bytes_written == overlap && !result.truncated {
                        // Nothing past the cached bytes - the cache is current
                        std::fs::remove_file(&temp).ok();
                        summary.files_skipped = 1;
                        emit(0, "skipped");
                        return Ok(summary);
                    }
                    Some(Fetch {
                        skip: overlap,
                        cut: None,
                        start_day: s.last_day,
                        tail_seed: tail,
                        remote_size: range_from + result.bytes_written,
                        bytes_downloaded: result.bytes_written,
                        truncated: result.truncated,
                    })
                } else {
                    // Rotated (or shrank since the listing) - take the whole file
                    None
                }
            } else {
                // The server ignored the range and sent the whole file - decide
                // append vs merge from the bytes where the cached content ends
                if result.bytes_written >= s.remote_size
                    && file_matches_at(&temp, range_from, &tail)?
                {
                    if result.bytes_written == s.remote_size && !result.truncated {
                        std::fs::remove_file(&temp).ok();
                        summary.files_skipped = 1;
                        emit(0, "skipped");
                        return Ok(summary);
                    }
                    Some(Fetch {
                        skip: s.remote_size,
                        cut: None,
                        start_day: s.last_day,
                        tail_seed: tail,
                        remote_size: result.bytes_written,
                        bytes_downloaded: result.bytes_written,
                        truncated: result.truncated,
                    })
                } else {
                    Some(Fetch {
                        skip: 0,
                        cut: s.last_ts,
                        start_day: None,
                        tail_seed: Vec::new(),
                        remote_size: result.bytes_written,
                        bytes_downloaded: result.bytes_written,
                        truncated: result.truncated,
                    })
                }
            }
        }
        _ => None,
    };

    let fetch = match fetch {
        Some(fetch) => fetch,
        None => {
            // A cancel during the ranged attempt must not fall through into a
            // full re-download of the whole file
            if cancel.load(Ordering::Relaxed) {
                std::fs::remove_file(&temp).ok();
                summary.files_skipped = 1;
                emit(0, "skipped");
                return Ok(summary);
            }
            let result = match source
                .download_file(&file.vfs_path, &temp, None, Some(file.size), cancel, |d| {
                    emit(d, "downloading")
                })
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    summary.files_failed = 1;
                    summary.warnings.push(format!("{}: download failed - {e}", file.name));
                    emit(0, "failed");
                    return Ok(summary);
                }
            };
            Fetch {
                skip: 0,
                cut: state.as_ref().and_then(|s| s.last_ts),
                start_day: None,
                tail_seed: Vec::new(),
                remote_size: result.bytes_written,
                bytes_downloaded: result.bytes_written,
                truncated: result.truncated,
            }
        }
    };

    // Splitting a large download into day segments takes a while - tell the UI
    // the download itself is finished so it does not look stuck at 100%
    emit(fetch.bytes_downloaded, "processing");

    let mut raw = std::fs::File::open(&temp)
        .map_err(|e| format!("Cannot open {}: {e}", temp.display()))?;
    raw.seek(std::io::SeekFrom::Start(fetch.skip))
        .map_err(|e| e.to_string())?;
    let outcome = split_into_days(
        std::io::BufReader::new(raw),
        fetch.cut,
        fetch.start_day,
        fetch.tail_seed,
        dir,
        manifest,
        &file,
    )?;
    std::fs::remove_file(&temp).ok();

    // A rotation can replace content with older lines that are all cut away -
    // max() keeps the state's watermark from moving backwards then
    manifest.growing.insert(
        file.vfs_path.clone(),
        GrowingState {
            remote_size: fetch.remote_size,
            remote_mtime: file.mtime.clone(),
            last_ts: outcome.last_ts.max(state.as_ref().and_then(|s| s.last_ts)),
            last_day: outcome.last_day.max(state.as_ref().and_then(|s| s.last_day)),
            tail_hex: hex_encode(&outcome.tail),
        },
    );
    cache::save_manifest(dir, manifest)?;

    if fetch.truncated {
        summary.warnings.push(format!(
            "{}: transfer interrupted - cached logs are incomplete; sync again to retry",
            file.name
        ));
    }
    summary.files_downloaded = 1;
    summary.bytes_downloaded = fetch.bytes_downloaded;
    emit(fetch.bytes_downloaded, "done");
    Ok(summary)
}

/// One resolved download: where the new content starts in the temp file and how
/// to merge it with what is cached.
struct Fetch {
    /// Bytes of the temp file that are already cached (the verified overlap)
    skip: u64,
    /// Drop lines at or before this timestamp - used after a rotation, when the
    /// download overlaps cached content but not byte-for-byte
    cut: Option<NaiveDateTime>,
    /// Day for continuation lines at the start of an appended chunk
    start_day: Option<NaiveDate>,
    /// Rolling-tail seed: the consumed content directly before the chunk
    tail_seed: Vec<u8>,
    /// Remote bytes consumed after this sync
    remote_size: u64,
    bytes_downloaded: u64,
    truncated: bool,
}

/// Splits the legacy single-file cache (Log.txt.zst) into day segments once,
/// deriving the growing state from its manifest entry, and removes it.
fn migrate_legacy(dir: &Path, manifest: &mut Manifest, file: &RemoteLogFile) -> Result<(), String> {
    let Some(entry) = manifest.entries.get(&file.vfs_path) else {
        return Ok(());
    };
    if entry.date.is_some() {
        return Ok(());
    }
    let entry = entry.clone();
    let path = dir.join(&entry.local_name);
    if path.exists() {
        let raw = std::fs::File::open(&path)
            .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
        let decoder = zstd::stream::read::Decoder::new(raw)
            .map_err(|e| format!("zstd open failed for {}: {e}", path.display()))?;
        let outcome = split_into_days(
            std::io::BufReader::new(decoder),
            None,
            None,
            Vec::new(),
            dir,
            manifest,
            file,
        )?;
        manifest.growing.insert(
            file.vfs_path.clone(),
            GrowingState {
                remote_size: entry.remote_size,
                remote_mtime: entry.remote_mtime.clone(),
                last_ts: outcome.last_ts,
                last_day: outcome.last_day,
                tail_hex: hex_encode(&outcome.tail),
            },
        );
        std::fs::remove_file(&path).ok();
    }
    manifest.entries.remove(&file.vfs_path);
    cache::save_manifest(dir, manifest)
}

struct SplitOutcome {
    last_ts: Option<NaiveDateTime>,
    last_day: Option<NaiveDate>,
    /// Last TAIL_LEN bytes of everything read, cut lines included - the tail of
    /// the remote content consumed so far
    tail: Vec<u8>,
}

/// Streams log lines into per-day zstd segments, appending a frame per touched
/// day and folding each day's span and size into the manifest.
///
/// Lines dated by `parse_timestamp` pick the segment; continuation lines (SQL
/// bodies, stack traces) follow the entry that opened them - `start_day` seeds
/// that for a chunk beginning mid-entry. With `cut`, lines at or before the
/// timestamp are dropped as already cached; lines sharing the exact cut second
/// are considered cached too, which loses nothing in practice since a rotation
/// happens between syncs, not mid-second.
fn split_into_days(
    mut reader: impl BufRead,
    cut: Option<NaiveDateTime>,
    start_day: Option<NaiveDate>,
    tail_seed: Vec<u8>,
    dir: &Path,
    manifest: &mut Manifest,
    file: &RemoteLogFile,
) -> Result<SplitOutcome, String> {
    let mut tail = tail_seed;
    let mut cutting = cut;
    let mut pending: Vec<u8> = Vec::new();
    let mut writer: Option<DayWriter> = None;
    let mut current_day = start_day;
    let mut last_ts: Option<NaiveDateTime> = None;
    let mut buffer = Vec::new();

    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("Read failed while splitting log: {e}")),
        }
        push_tail(&mut tail, &buffer);
        let text = String::from_utf8_lossy(&buffer);
        let ts = parse_timestamp(text.trim_end_matches(['\r', '\n']));
        if let Some(ts) = ts {
            last_ts = Some(ts);
        }
        if let Some(cut_ts) = cutting {
            match ts {
                Some(ts) if ts > cut_ts => cutting = None,
                _ => continue,
            }
        }
        let Some(day) = ts.map(|ts| ts.date()).or(current_day) else {
            // Continuation lines before the first timestamp - held until the
            // first dated line tells which day they belong to
            pending.extend_from_slice(&buffer);
            continue;
        };
        current_day = Some(day);
        if writer.as_ref().is_some_and(|w| w.day != day) {
            writer.take().expect("just checked").close(manifest, file)?;
        }
        if writer.is_none() {
            writer = Some(DayWriter::open(dir, day)?);
        }
        let w = writer.as_mut().expect("just set");
        if !pending.is_empty() {
            w.write(&pending, None)?;
            pending.clear();
        }
        w.write(&buffer, ts)?;
    }
    if let Some(w) = writer.take() {
        w.close(manifest, file)?;
    }

    Ok(SplitOutcome {
        last_day: last_ts.map(|ts| ts.date()).or(start_day),
        last_ts,
        tail,
    })
}

/// Open zstd frame appended to one day's segment during a split.
struct DayWriter {
    day: NaiveDate,
    encoder: zstd::stream::write::Encoder<'static, std::fs::File>,
    bytes: u64,
    first_ts: Option<NaiveDateTime>,
    last_ts: Option<NaiveDateTime>,
}

impl DayWriter {
    fn open(dir: &Path, day: NaiveDate) -> Result<Self, String> {
        let path = dir.join(segment_file_name(day));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
        let encoder = zstd::stream::write::Encoder::new(file, 3)
            .map_err(|e| format!("zstd encoder failed: {e}"))?;
        Ok(DayWriter {
            day,
            encoder,
            bytes: 0,
            first_ts: None,
            last_ts: None,
        })
    }

    fn write(&mut self, bytes: &[u8], ts: Option<NaiveDateTime>) -> Result<(), String> {
        self.encoder
            .write_all(bytes)
            .map_err(|e| format!("Write to day segment failed: {e}"))?;
        self.bytes += bytes.len() as u64;
        if let Some(ts) = ts {
            self.first_ts.get_or_insert(ts);
            self.last_ts = Some(ts);
        }
        Ok(())
    }

    /// Finishes the zstd frame and folds this stint into the day's manifest entry.
    fn close(self, manifest: &mut Manifest, file: &RemoteLogFile) -> Result<(), String> {
        self.encoder
            .finish()
            .map_err(|e| format!("zstd finish failed: {e}"))?;
        let entry = manifest
            .entries
            .entry(segment_key(&file.vfs_path, self.day))
            .or_insert_with(|| CachedFile {
                // For a segment this counts the uncompressed bytes stored, not a
                // remote file size - the same thing the status and the memory
                // cache read it as
                remote_size: 0,
                remote_mtime: file.mtime.clone(),
                local_name: segment_file_name(self.day),
                date: Some(self.day),
                instance: None,
                first_ts: None,
                last_ts: None,
            });
        entry.remote_size += self.bytes;
        entry.remote_mtime = file.mtime.clone();
        entry.first_ts = match (entry.first_ts, self.first_ts) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        entry.last_ts = match (entry.last_ts, self.last_ts) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        Ok(())
    }
}

/// Whether the file's bytes at `offset` equal `expected`. Anything unreadable
/// (file shorter than the span) is a mismatch, not an error.
fn file_matches_at(path: &Path, offset: u64, expected: &[u8]) -> Result<bool, String> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
    file.seek(std::io::SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; expected.len()];
    match file.read_exact(&mut buffer) {
        Ok(()) => Ok(buffer == expected),
        Err(_) => Ok(false),
    }
}

fn push_tail(tail: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.len() >= TAIL_LEN {
        tail.clear();
        tail.extend_from_slice(&bytes[bytes.len() - TAIL_LEN..]);
        return;
    }
    tail.extend_from_slice(bytes);
    if tail.len() > TAIL_LEN {
        tail.drain(..tail.len() - TAIL_LEN);
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut text, byte| {
            write!(text, "{byte:02x}").expect("writing to a String cannot fail");
            text
        },
    )
}

fn hex_decode(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .filter_map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const VFS: &str = "site/wwwroot/App_Data/Log.txt";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("loglooker-growing").join(name);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn remote() -> RemoteLogFile {
        RemoteLogFile {
            vfs_path: VFS.into(),
            name: "Log.txt".into(),
            size: 0,
            mtime: "2026-07-17T10:00:00Z".into(),
            date: None,
            instance: None,
        }
    }

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn split(
        dir: &Path,
        manifest: &mut Manifest,
        text: &str,
        cut: Option<&str>,
        start_day: Option<NaiveDate>,
    ) -> SplitOutcome {
        let cut =
            cut.map(|c| NaiveDateTime::parse_from_str(c, "%Y-%m-%d %H:%M:%S").unwrap());
        split_into_days(text.as_bytes(), cut, start_day, Vec::new(), dir, manifest, &remote())
            .unwrap()
    }

    fn read_day(dir: &Path, date: &str) -> String {
        let path = dir.join(format!("Log-{date}.txt.zst"));
        let mut out = Vec::new();
        zstd::stream::copy_decode(std::fs::File::open(path).unwrap(), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn splits_by_day_and_keeps_continuation_lines_with_their_entry() {
        let dir = temp("split");
        let mut manifest = Manifest::default();
        let text = "15.07.2026 10:00:00 debug (sql) - exec SF_X @Body='<html>\n\
                    </html>\n\
                    16.07.2026 09:00:00 debug (other) - two\n";

        let outcome = split(&dir, &mut manifest, text, None, None);

        assert_eq!(
            read_day(&dir, "2026-07-15"),
            "15.07.2026 10:00:00 debug (sql) - exec SF_X @Body='<html>\n</html>\n"
        );
        assert_eq!(read_day(&dir, "2026-07-16"), "16.07.2026 09:00:00 debug (other) - two\n");
        assert_eq!(outcome.last_ts.unwrap().to_string(), "2026-07-16 09:00:00");
        assert_eq!(outcome.last_day, Some(day("2026-07-16")));
        assert_eq!(String::from_utf8(outcome.tail).unwrap(), text);

        let entry = &manifest.entries[&format!("{VFS}@2026-07-15")];
        assert_eq!(entry.date, Some(day("2026-07-15")));
        assert_eq!(entry.local_name, "Log-2026-07-15.txt.zst");
        assert_eq!(entry.first_ts.unwrap().to_string(), "2026-07-15 10:00:00");
        assert_eq!(entry.last_ts.unwrap().to_string(), "2026-07-15 10:00:00");
        assert_eq!(entry.remote_size, read_day(&dir, "2026-07-15").len() as u64);
    }

    #[test]
    fn leading_continuation_lines_join_the_first_dated_day() {
        let dir = temp("pending");
        let mut manifest = Manifest::default();
        let text = "tail of an entry the download cut in half\n\
                    15.07.2026 08:00:00 debug (other) - first dated line\n";

        split(&dir, &mut manifest, text, None, None);

        assert_eq!(read_day(&dir, "2026-07-15"), text);
    }

    #[test]
    fn an_appended_chunk_of_continuation_lines_extends_the_previous_day() {
        let dir = temp("append");
        let mut manifest = Manifest::default();
        split(
            &dir,
            &mut manifest,
            "15.07.2026 10:00:00 debug (sql) - exec SF_X @Body='<html>\n",
            None,
            None,
        );

        // The next sync starts mid-entry: no timestamp anywhere in the chunk
        let outcome = split(&dir, &mut manifest, "</html>\n", None, Some(day("2026-07-15")));

        assert_eq!(
            read_day(&dir, "2026-07-15"),
            "15.07.2026 10:00:00 debug (sql) - exec SF_X @Body='<html>\n</html>\n"
        );
        assert_eq!(outcome.last_ts, None);
        assert_eq!(outcome.last_day, Some(day("2026-07-15")));
        let entry = &manifest.entries[&format!("{VFS}@2026-07-15")];
        assert_eq!(entry.remote_size, read_day(&dir, "2026-07-15").len() as u64);
    }

    #[test]
    fn a_cut_drops_lines_already_cached_and_keeps_the_rest() {
        let dir = temp("cut");
        let mut manifest = Manifest::default();
        let text = "15.07.2026 11:00:00 debug (other) - cached\n\
                    15.07.2026 12:00:00 debug (other) - exactly the cut\n\
                    15.07.2026 13:00:00 debug (sql) - kept @Body='<html>\n\
                    </html>\n";

        let outcome = split(&dir, &mut manifest, text, Some("2026-07-15 12:00:00"), None);

        assert_eq!(
            read_day(&dir, "2026-07-15"),
            "15.07.2026 13:00:00 debug (sql) - kept @Body='<html>\n</html>\n"
        );
        // The tail still covers everything read - it mirrors the remote bytes
        assert_eq!(String::from_utf8(outcome.tail).unwrap(), text);
    }

    #[test]
    fn a_fully_cut_chunk_writes_nothing() {
        let dir = temp("allcut");
        let mut manifest = Manifest::default();
        let outcome = split(
            &dir,
            &mut manifest,
            "15.07.2026 11:00:00 debug (other) - old\n",
            Some("2026-07-15 12:00:00"),
            None,
        );

        assert!(manifest.entries.is_empty());
        assert!(!dir.join("Log-2026-07-15.txt.zst").exists());
        assert_eq!(outcome.last_ts.unwrap().to_string(), "2026-07-15 11:00:00");
    }

    #[test]
    fn migrating_the_legacy_single_file_splits_it_into_days() {
        let dir = temp("migrate");
        let raw = "15.07.2026 10:00:00 debug (other) - one\n\
                   16.07.2026 09:00:00 debug (other) - two\n";
        std::fs::write(
            dir.join("Log.txt.zst"),
            zstd::stream::encode_all(raw.as_bytes(), 3).unwrap(),
        )
        .unwrap();
        let mut manifest = Manifest::default();
        manifest.entries.insert(
            VFS.into(),
            CachedFile {
                remote_size: raw.len() as u64,
                remote_mtime: "2026-07-16T09:00:01Z".into(),
                local_name: "Log.txt.zst".into(),
                date: None,
                instance: None,
                first_ts: None,
                last_ts: None,
            },
        );

        migrate_legacy(&dir, &mut manifest, &remote()).unwrap();

        assert!(!manifest.entries.contains_key(VFS), "legacy entry is gone");
        assert!(!dir.join("Log.txt.zst").exists(), "legacy file is gone");
        assert_eq!(read_day(&dir, "2026-07-15"), "15.07.2026 10:00:00 debug (other) - one\n");
        assert_eq!(read_day(&dir, "2026-07-16"), "16.07.2026 09:00:00 debug (other) - two\n");

        let state = &manifest.growing[VFS];
        assert_eq!(state.remote_size, raw.len() as u64);
        assert_eq!(state.last_ts.unwrap().to_string(), "2026-07-16 09:00:00");
        assert_eq!(hex_decode(&state.tail_hex), raw.as_bytes());
    }

    #[test]
    fn migration_is_a_no_op_without_a_legacy_entry() {
        let dir = temp("nomigrate");
        let mut manifest = Manifest::default();
        migrate_legacy(&dir, &mut manifest, &remote()).unwrap();
        assert!(manifest.entries.is_empty() && manifest.growing.is_empty());
    }

    #[test]
    fn the_tail_is_capped_and_survives_a_hex_round_trip() {
        let mut tail = Vec::new();
        push_tail(&mut tail, &[1u8; 100]);
        push_tail(&mut tail, &vec![2u8; TAIL_LEN * 2]);
        assert_eq!(tail.len(), TAIL_LEN);
        assert!(tail.iter().all(|&b| b == 2));

        push_tail(&mut tail, b"end");
        assert_eq!(tail.len(), TAIL_LEN);
        assert!(tail.ends_with(b"end"));
        assert_eq!(hex_decode(&hex_encode(&tail)), tail);
    }

    #[test]
    fn file_bytes_are_compared_at_an_offset() {
        let dir = temp("matches");
        let path = dir.join("probe.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        assert!(file_matches_at(&path, 2, b"2345").unwrap());
        assert!(!file_matches_at(&path, 3, b"2345").unwrap());
        assert!(
            !file_matches_at(&path, 8, b"890").unwrap(),
            "a span past the end is a mismatch, not an error"
        );
        assert!(file_matches_at(&path, 10, b"").unwrap(), "an empty span always matches");
    }
}

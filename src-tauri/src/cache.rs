use crate::config::ServiceConfig;
use crate::kudu::KuduClient;
use crate::profiles::{self, ProfileKind, RemoteLogFile};
use crate::scraper::Environment;
use crate::search::parse_timestamp;
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;

/// Cached files are stored zstd-compressed. Growing remote files are synced
/// incrementally: only bytes past the last known size are fetched (HTTP Range)
/// and appended as a new zstd frame — decoders handle concatenated frames.
/// Files that can still grow (dated today, or undated like Log.txt) are never
/// skipped on listing equality alone — the listing may lag behind writes — so
/// they are probed with a ranged request past the cached size on every sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: HashMap<String, CachedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedFile {
    pub remote_size: u64,
    pub remote_mtime: String,
    pub local_name: String,
    pub date: Option<NaiveDate>,
    pub instance: Option<String>,
    /// Real span of the log lines inside the file. The filename only dates the
    /// per-day files; Log.txt is one growing file whose span is content-only.
    /// Absent in manifests written before this was recorded — backfilled on sync.
    #[serde(default)]
    pub first_ts: Option<NaiveDateTime>,
    #[serde(default)]
    pub last_ts: Option<NaiveDateTime>,
}

/// Span of the timestamped lines in a log file or in one downloaded chunk of it.
#[derive(Debug, Clone, Copy, Default)]
pub struct TimeRange {
    pub first: Option<NaiveDateTime>,
    pub last: Option<NaiveDateTime>,
}

impl TimeRange {
    /// Extends a known range with a chunk appended after it.
    fn extended_with(self, chunk: TimeRange) -> TimeRange {
        TimeRange {
            first: self.first.or(chunk.first),
            last: chunk.last.or(self.last),
        }
    }
}

/// Scans a raw log stream for its first and last line-start timestamp. Lines that
/// continue an entry (SQL bodies, embedded HTML) carry none and are skipped, so a
/// multi-line entry is dated by the line that opened it.
fn scan_time_range(mut reader: impl BufRead) -> TimeRange {
    let mut range = TimeRange::default();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buffer);
        if let Some(ts) = parse_timestamp(line.trim_end_matches(['\r', '\n'])) {
            range.first.get_or_insert(ts);
            range.last = Some(ts);
        }
    }
    range
}

fn scan_cached_file(path: &std::path::Path) -> TimeRange {
    let Ok(raw) = std::fs::File::open(path) else {
        return TimeRange::default();
    };
    match zstd::stream::read::Decoder::new(raw) {
        Ok(decoder) => scan_time_range(std::io::BufReader::new(decoder)),
        Err(_) => TimeRange::default(),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub service_id: String,
    pub file_name: String,
    pub file_index: usize,
    pub file_count: usize,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSummary {
    pub service_id: String,
    pub files_total: usize,
    pub files_downloaded: usize,
    pub files_skipped: usize,
    pub bytes_downloaded: u64,
    pub warnings: Vec<String>,
}

pub fn cache_root() -> Result<PathBuf, String> {
    // Override used by tests to keep them out of the real app cache
    if let Ok(dir) = std::env::var("LOGLOOKER_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let dir = dirs::data_dir()
        .ok_or("Cannot resolve data directory")?
        .join("LogLooker")
        .join("cache");
    Ok(dir)
}

/// Path only — does not touch the filesystem. Reads (status, search) must not
/// create directories as a side effect.
pub fn service_dir_path(environment: Environment, name: &str) -> Result<PathBuf, String> {
    let env = match environment {
        Environment::Test => "test",
        Environment::Production => "production",
    };
    Ok(cache_root()?.join(env).join(name))
}

pub fn service_dir(environment: Environment, name: &str) -> Result<PathBuf, String> {
    let dir = service_dir_path(environment, name)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create cache dir: {e}"))?;
    Ok(dir)
}

pub fn load_manifest(dir: &std::path::Path) -> Manifest {
    match std::fs::read_to_string(dir.join("manifest.json")) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Manifest::default(),
    }
}

fn save_manifest(dir: &std::path::Path, manifest: &Manifest) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("manifest.json"), text)
        .map_err(|e| format!("Cannot write manifest: {e}"))
}

/// Syncs one service's logs for a date range into the local cache.
/// `on_progress` is called with download progress for the UI.
pub async fn sync_service(
    client: &KuduClient,
    service: &ServiceConfig,
    profile: ProfileKind,
    from: NaiveDate,
    to: NaiveDate,
    mut on_progress: impl FnMut(SyncProgress),
) -> Result<SyncSummary, String> {
    let dir = service_dir(service.environment, &service.name)?;
    let mut manifest = load_manifest(&dir);
    let files = profiles::list_files(client, profile, from, to).await?;

    let mut summary = SyncSummary {
        service_id: service.id.clone(),
        files_total: files.len(),
        files_downloaded: 0,
        files_skipped: 0,
        bytes_downloaded: 0,
        warnings: Vec::new(),
    };

    let instances: std::collections::HashSet<_> = files
        .iter()
        .filter_map(|f| f.instance.as_deref())
        .collect();
    if instances.len() > 1 {
        summary.warnings.push(format!(
            "Service ran on {} instances in this range; Kudu only exposes files present on disk — verify coverage",
            instances.len()
        ));
    }

    let today = chrono::Local::now().date_naive();
    let file_count = files.len();
    for (index, file) in files.into_iter().enumerate() {
        let unchanged = manifest.entries.get(&file.vfs_path).is_some_and(|entry| {
            entry.remote_size == file.size && entry.remote_mtime == file.mtime
        });
        if unchanged && !is_live(&file, today) {
            backfill_time_range(&dir, &mut manifest, &file.vfs_path)?;
            summary.files_skipped += 1;
            on_progress(SyncProgress {
                service_id: service.id.clone(),
                file_name: file.name.clone(),
                file_index: index + 1,
                file_count,
                bytes_downloaded: 0,
                total_bytes: file.size,
                state: "skipped".into(),
            });
            continue;
        }

        let cached = manifest.entries.get(&file.vfs_path);
        // For an unchanged live file the sizes are equal and the ranged probe
        // starts at the end — zero new bytes confirms the cache is current.
        let known_size = cached
            .map(|entry| entry.remote_size)
            .filter(|&size| size < file.size || unchanged);
        let known_range = cached.map_or_else(TimeRange::default, |entry| TimeRange {
            first: entry.first_ts,
            last: entry.last_ts,
        });

        let stored = match download_and_compress(
            client,
            &dir,
            &file,
            known_size,
            |downloaded| {
                on_progress(SyncProgress {
                    service_id: service.id.clone(),
                    file_name: file.name.clone(),
                    file_index: index + 1,
                    file_count,
                    bytes_downloaded: downloaded,
                    total_bytes: file.size,
                    state: "downloading".into(),
                });
            },
        )
        .await
        {
            Ok(stored) => stored,
            // A single file failing to download must not abort the rest of the
            // sync — record it and move on. Leaving it out of the manifest means
            // the next sync retries it.
            Err(e) => {
                summary.warnings.push(format!("{}: download failed — {e}", file.name));
                on_progress(SyncProgress {
                    service_id: service.id.clone(),
                    file_name: file.name.clone(),
                    file_index: index + 1,
                    file_count,
                    bytes_downloaded: 0,
                    total_bytes: file.size,
                    state: "skipped".into(),
                });
                continue;
            }
        };

        // A probe that found nothing past the cached size — the file is current
        if stored.appended && stored.bytes == 0 {
            backfill_time_range(&dir, &mut manifest, &file.vfs_path)?;
            summary.files_skipped += 1;
            on_progress(SyncProgress {
                service_id: service.id.clone(),
                file_name: file.name.clone(),
                file_index: index + 1,
                file_count,
                bytes_downloaded: 0,
                total_bytes: file.size,
                state: "skipped".into(),
            });
            continue;
        }

        // An appended chunk only extends the span; a full download replaces it
        let range = if stored.appended {
            known_range.extended_with(stored.range)
        } else {
            stored.range
        };

        // The file may have grown between listing and download, so record what
        // was actually stored — the next ranged sync must start past it
        let cached_size = if stored.appended {
            known_size.unwrap_or(0) + stored.bytes
        } else {
            stored.bytes
        };

        manifest.entries.insert(
            file.vfs_path.clone(),
            CachedFile {
                remote_size: cached_size,
                remote_mtime: file.mtime.clone(),
                local_name: format!("{}.zst", file.name),
                date: file.date,
                instance: file.instance.clone(),
                first_ts: range.first,
                last_ts: range.last,
            },
        );
        save_manifest(&dir, &manifest)?;
        summary.files_downloaded += 1;
        summary.bytes_downloaded += stored.bytes;
        on_progress(SyncProgress {
            service_id: service.id.clone(),
            file_name: file.name.clone(),
            file_index: index + 1,
            file_count,
            bytes_downloaded: stored.bytes,
            total_bytes: file.size,
            state: "done".into(),
        });
    }

    Ok(summary)
}

/// A file whose content can still grow: dated today (or later), or undated —
/// a growing file like Log.txt. Live files are always probed for new content.
fn is_live(file: &RemoteLogFile, today: NaiveDate) -> bool {
    file.date.map_or(true, |date| date >= today)
}

/// Reads the time range of an already cached file that predates time ranges in
/// the manifest. Costs one decompression pass, once per file.
fn backfill_time_range(
    dir: &std::path::Path,
    manifest: &mut Manifest,
    vfs_path: &str,
) -> Result<(), String> {
    let Some(entry) = manifest.entries.get(vfs_path) else {
        return Ok(());
    };
    if entry.first_ts.is_some() || entry.last_ts.is_some() {
        return Ok(());
    }
    let range = scan_cached_file(&dir.join(&entry.local_name));
    if range.first.is_none() {
        return Ok(());
    }
    let entry = manifest.entries.get_mut(vfs_path).expect("just read");
    entry.first_ts = range.first;
    entry.last_ts = range.last;
    save_manifest(dir, manifest)
}

struct StoredFile {
    bytes: u64,
    /// Span of the downloaded chunk — the whole file, or just the new tail
    range: TimeRange,
    appended: bool,
}

/// Downloads (fully, or from `range_from` for grown files) and stores the file
/// as zstd. A ranged download appends a new frame; a full one replaces the file.
async fn download_and_compress(
    client: &KuduClient,
    dir: &std::path::Path,
    file: &RemoteLogFile,
    range_from: Option<u64>,
    on_chunk: impl FnMut(u64),
) -> Result<StoredFile, String> {
    let temp_path = dir.join(format!("{}.download", file.name));
    let final_path = dir.join(format!("{}.zst", file.name));

    let result = client
        .download_file(&file.vfs_path, &temp_path, range_from, on_chunk)
        .await?;

    // A ranged probe answered with no bytes — nothing new past the cached size
    if result.was_partial && result.bytes_written == 0 {
        std::fs::remove_file(&temp_path).ok();
        return Ok(StoredFile {
            bytes: 0,
            range: TimeRange::default(),
            appended: true,
        });
    }

    let append = range_from.is_some() && result.was_partial && final_path.exists();
    let raw = std::fs::File::open(&temp_path).map_err(|e| e.to_string())?;
    let range = scan_time_range(std::io::BufReader::new(&raw));

    let raw = std::fs::File::open(&temp_path).map_err(|e| e.to_string())?;
    let output = std::fs::OpenOptions::new()
        .create(true)
        .append(append)
        .write(true)
        .truncate(!append)
        .open(&final_path)
        .map_err(|e| format!("Cannot open {}: {e}", final_path.display()))?;

    zstd::stream::copy_encode(std::io::BufReader::new(raw), output, 3)
        .map_err(|e| format!("zstd compression failed: {e}"))?;
    std::fs::remove_file(&temp_path).ok();

    Ok(StoredFile {
        bytes: result.bytes_written,
        range,
        appended: append,
    })
}

/// `oldest`/`newest` span the log lines actually cached, not the file names, so
/// undated growing files (Log.txt) count and the range carries a time of day.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatus {
    pub service_id: String,
    pub files: usize,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub oldest: Option<NaiveDateTime>,
    pub newest: Option<NaiveDateTime>,
    /// Minutes the log clock runs ahead of UTC; `None` while it cannot be told
    pub log_offset_minutes: Option<i32>,
}

/// No real time zone is further from UTC than this, so a wider delta is evidence
/// of a stale mtime, not of a log clock.
const MAX_OFFSET_MINUTES: i64 = 14 * 60;

/// Minutes the service's log clock runs ahead of UTC, or `None` if no cached file
/// can say. A log line carries no zone, and an App Service with `WEBSITE_TIME_ZONE`
/// set writes wall-clock local time while a default one writes UTC — the same line
/// either way. Kudu dates every file in UTC, and a log file's last write *is* its
/// last timestamped line, so `last_ts - mtime` measures the clock the lines are in.
/// This holds for files of any age, unlike comparing the newest line against "now",
/// which only says anything while a service is actively logging.
///
/// Reduced by median because a manifest's mtime comes from the listing that preceded
/// the download and can lag content appended after it — such a file reads hours ahead
/// on its own, but cannot move the middle of the set.
pub fn detect_offset_minutes(manifest: &Manifest) -> Option<i32> {
    let mut deltas: Vec<i64> = manifest
        .entries
        .values()
        .filter_map(|entry| {
            let last = entry.last_ts?;
            let mtime = chrono::DateTime::parse_from_rfc3339(&entry.remote_mtime).ok()?;
            Some((last - mtime.naive_utc()).num_minutes())
        })
        .filter(|delta| delta.abs() <= MAX_OFFSET_MINUTES)
        .collect();
    if deltas.is_empty() {
        return None;
    }
    deltas.sort_unstable();
    let median = deltas[deltas.len() / 2] as f64;
    // Every real offset is a whole quarter hour; the delta is only ever that plus
    // the seconds between the final write and the mtime the listing recorded.
    Some((median / 15.0).round() as i32 * 15)
}

/// The service's log clock offset, read from its sync manifest. Absent for a service
/// with nothing cached, or one whose files were all cached before time ranges were.
pub fn service_offset_minutes(service: &ServiceConfig) -> Option<i32> {
    let dir = service_dir_path(service.environment, &service.name).ok()?;
    detect_offset_minutes(&load_manifest(&dir))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedFileInfo {
    pub service_id: String,
    /// Name as the search hits report it — the cache file without the .zst suffix
    pub file: String,
    pub date: Option<NaiveDate>,
    pub size_bytes: u64,
    pub compressed_bytes: u64,
    pub instance: Option<String>,
}

/// The service's cached files, newest first. Undated files (growing Log.txt) sort last.
pub fn cached_files(service: &ServiceConfig) -> Result<Vec<CachedFileInfo>, String> {
    let dir = service_dir_path(service.environment, &service.name)?;
    let manifest = load_manifest(&dir);
    let mut files: Vec<CachedFileInfo> = manifest
        .entries
        .values()
        .map(|entry| CachedFileInfo {
            service_id: service.id.clone(),
            file: entry.local_name.trim_end_matches(".zst").to_string(),
            date: entry.date,
            size_bytes: entry.remote_size,
            compressed_bytes: std::fs::metadata(dir.join(&entry.local_name))
                .map(|m| m.len())
                .unwrap_or(0),
            instance: entry.instance.clone(),
        })
        .collect();

    files.sort_by(|a, b| match (a.date, b.date) {
        (Some(x), Some(y)) => y.cmp(&x).then(a.file.cmp(&b.file)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.file.cmp(&b.file),
    });
    Ok(files)
}

/// Removes one cached file and its manifest entry. The next sync of a date
/// range covering it simply downloads it again.
pub fn delete_cached_file(service: &ServiceConfig, file: &str) -> Result<(), String> {
    let dir = service_dir_path(service.environment, &service.name)?;
    let mut manifest = load_manifest(&dir);
    let local_name = format!("{file}.zst");
    let key = manifest
        .entries
        .iter()
        .find(|(_, entry)| entry.local_name == local_name)
        .map(|(key, _)| key.clone())
        .ok_or_else(|| format!("{file} is not cached for {}", service.name))?;

    if let Err(e) = std::fs::remove_file(dir.join(&local_name)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(format!("Cannot delete {local_name}: {e}"));
        }
    }
    manifest.entries.remove(&key);
    save_manifest(&dir, &manifest)
}

pub fn cache_status(service: &ServiceConfig) -> Result<CacheStatus, String> {
    let dir = service_dir_path(service.environment, &service.name)?;
    let manifest = load_manifest(&dir);
    let mut status = CacheStatus {
        service_id: service.id.clone(),
        files: manifest.entries.len(),
        compressed_bytes: 0,
        uncompressed_bytes: 0,
        oldest: None,
        newest: None,
        log_offset_minutes: detect_offset_minutes(&manifest),
    };
    for entry in manifest.entries.values() {
        status.uncompressed_bytes += entry.remote_size;
        if let Ok(meta) = std::fs::metadata(dir.join(&entry.local_name)) {
            status.compressed_bytes += meta.len();
        }
        // Files cached before time ranges were recorded fall back to day granularity
        let midnight = entry.date.and_then(|date| date.and_hms_opt(0, 0, 0));
        if let Some(first) = entry.first_ts.or(midnight) {
            status.oldest = Some(status.oldest.map_or(first, |ts| ts.min(first)));
        }
        if let Some(last) = entry.last_ts.or(midnight) {
            status.newest = Some(status.newest.map_or(last, |ts| ts.max(last)));
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Log.txt slice: an SQL entry whose @Body spills raw HTML over many lines,
    /// then a later entry. Only the two line-start timestamps may count.
    const LOG_TXT: &str = concat!(
        "10.07.2026 11:28:01 debug (sql) - exec SF_Email_EDIT_Translate @ID=1086895, @Body='<title></title>\n",
        "<meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"/>\n",
        "<!--[if mso]><xml><o:OfficeDocumentSettings><o:PixelsPerInch>96</o:PixelsPerInch></xml><![endif]-->\n",
        "<link href=\"https://fonts.googleapis.com/css?family=Raleway\" rel=\"stylesheet\">\n",
        "<style>\n",
        "10.07.2026 11:28:04 debug (other) - SET modified date\n",
    );

    fn range_of(text: &str) -> TimeRange {
        scan_time_range(std::io::BufReader::new(text.as_bytes()))
    }

    #[test]
    fn spans_multiline_entries_by_their_first_line() {
        let range = range_of(LOG_TXT);
        assert_eq!(range.first.unwrap().to_string(), "2026-07-10 11:28:01");
        assert_eq!(range.last.unwrap().to_string(), "2026-07-10 11:28:04");
    }

    #[test]
    fn ignores_a_trailing_continuation_line() {
        // The tail of a growing file is often mid-entry
        let range = range_of("10.07.2026 11:28:01 debug (sql) - exec SF_X @Body='<html>\n<body>\n");
        assert_eq!(range.last.unwrap().to_string(), "2026-07-10 11:28:01");
    }

    #[test]
    fn a_file_without_timestamps_has_no_range() {
        let range = range_of("SnapshotHelper::RestoreSnapshotInternal SUCCESS\n");
        assert!(range.first.is_none() && range.last.is_none());
    }

    #[test]
    fn appended_chunk_extends_the_known_span() {
        let known = range_of("10.07.2026 11:28:01 debug (sql) - exec SF_X\n");
        let chunk = range_of("14.07.2026 09:03:12 debug (sql) - exec SF_Y\n");
        let merged = known.extended_with(chunk);
        assert_eq!(merged.first.unwrap().to_string(), "2026-07-10 11:28:01");
        assert_eq!(merged.last.unwrap().to_string(), "2026-07-14 09:03:12");
    }

    #[test]
    fn appended_chunk_of_pure_continuation_keeps_the_known_span() {
        let known = range_of("10.07.2026 11:28:01 debug (sql) - exec SF_X\n");
        let merged = known.extended_with(range_of("</style>\n</html>\n"));
        assert_eq!(merged.last.unwrap().to_string(), "2026-07-10 11:28:01");
    }

    fn remote_file(date: Option<NaiveDate>) -> RemoteLogFile {
        RemoteLogFile {
            vfs_path: "applogs/x.log".into(),
            name: "x.log".into(),
            size: 10,
            mtime: "2026-07-14T10:00:00Z".into(),
            date,
            instance: None,
        }
    }

    /// One manifest entry: a file whose Kudu mtime is `mtime` and whose last log
    /// line reads `last_ts` — the pair the offset is measured from.
    fn cached(name: &str, mtime: &str, last_ts: Option<&str>) -> (String, CachedFile) {
        let parse = |ts: &str| NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%.f").unwrap();
        (
            name.to_string(),
            CachedFile {
                remote_size: 10,
                remote_mtime: mtime.to_string(),
                local_name: format!("{name}.zst"),
                date: None,
                instance: None,
                first_ts: None,
                last_ts: last_ts.map(parse),
            },
        )
    }

    fn manifest_of(files: Vec<(String, CachedFile)>) -> Manifest {
        Manifest {
            entries: files.into_iter().collect(),
        }
    }

    #[test]
    fn logs_written_in_utc_measure_no_offset() {
        // The seconds between the last line and the file's mtime must not register
        let manifest = manifest_of(vec![
            cached("a", "2026-07-14T11:35:03.860Z", Some("2026-07-14T11:35:03.881")),
            cached("b", "2026-07-13T23:59:51.031Z", Some("2026-07-13T23:59:51.066")),
            cached("c", "2026-07-13T00:00:28.345Z", Some("2026-07-12T23:59:57.646")),
        ]);
        assert_eq!(detect_offset_minutes(&manifest), Some(0));
    }

    #[test]
    fn logs_written_in_server_local_time_measure_that_offset() {
        let manifest = manifest_of(vec![
            cached("a", "2026-07-14T11:35:03Z", Some("2026-07-14T13:35:04")),
            cached("b", "2026-07-13T23:59:51Z", Some("2026-07-14T01:59:52")),
        ]);
        assert_eq!(detect_offset_minutes(&manifest), Some(120));
    }

    #[test]
    fn a_file_whose_mtime_lags_its_appended_content_does_not_skew_the_offset() {
        // Seen in the wild: the listing that dated the file preceded a later append
        let manifest = manifest_of(vec![
            cached("a", "2026-07-14T01:12:09Z", Some("2026-07-14T11:35:27")),
            cached("b", "2026-07-13T23:59:37Z", Some("2026-07-13T23:59:37")),
            cached("c", "2026-07-12T23:59:52Z", Some("2026-07-12T23:59:52")),
        ]);
        assert_eq!(detect_offset_minutes(&manifest), Some(0));
    }

    #[test]
    fn a_manifest_without_time_ranges_cannot_tell_the_offset() {
        let manifest = manifest_of(vec![cached("a", "2026-07-14T11:35:03Z", None)]);
        assert_eq!(detect_offset_minutes(&manifest), None);
    }

    #[test]
    fn todays_and_undated_files_are_live_older_ones_are_not() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        assert!(is_live(&remote_file(None), today));
        assert!(is_live(&remote_file(Some(today)), today));
        // A UTC-named file can be dated a day ahead of local time
        assert!(is_live(&remote_file(today.succ_opt()), today));
        assert!(!is_live(&remote_file(today.pred_opt()), today));
    }
}

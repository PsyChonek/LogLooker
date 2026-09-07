use crate::config::ServiceConfig;
use crate::locations::{self, RemoteLogFile};
use crate::plugin::CompiledLocation;
use crate::source::LogSource;
use crate::search::parse_timestamp;
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cached files are stored zstd-compressed. Growing remote files are synced
/// incrementally: only bytes past the last known size are fetched (HTTP Range)
/// and appended as a new zstd frame - decoders handle concatenated frames.
/// Files that can still grow (dated today, or undated like Log.txt) are never
/// skipped on listing equality alone - the listing may lag behind writes - so
/// they are probed with a ranged request past the cached size on every sync.
/// A single growing remote file (FrameworkAppData's Log.txt) does not go
/// through the per-file path at all - see the `growing` module.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: HashMap<String, CachedFile>,
    /// Sync state of single growing remote files (Log.txt), keyed by VFS path.
    /// Their lines live as per-day segment `entries`; this remembers how much
    /// of the remote file was already consumed into them.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub growing: HashMap<String, crate::growing::GrowingState>,
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
    /// Absent in manifests written before this was recorded - backfilled on sync.
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
    /// Files whose download failed; each has a matching entry in `warnings`.
    /// They stay out of the manifest so the next sync retries them.
    pub files_failed: usize,
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

/// Path only - does not touch the filesystem. Reads (status, search) must not
/// create directories as a side effect.
pub fn service_dir_path(environment: &str, name: &str) -> Result<PathBuf, String> {
    Ok(cache_root()?.join(environment).join(name))
}

pub fn service_dir(environment: &str, name: &str) -> Result<PathBuf, String> {
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

pub(crate) fn save_manifest(dir: &std::path::Path, manifest: &Manifest) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("manifest.json"), text)
        .map_err(|e| format!("Cannot write manifest: {e}"))
}

/// Syncs one service's logs for a date range into the local cache.
/// `on_progress` is called with download progress for the UI. `cancel` is polled
/// before each file so a running sync stops promptly when the user cancels;
/// files already stored stay cached and are reflected in the returned summary.
pub async fn sync_service(
    source: &LogSource,
    service: &ServiceConfig,
    location: &CompiledLocation,
    from: NaiveDate,
    to: NaiveDate,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(SyncProgress),
) -> Result<SyncSummary, String> {
    let dir = service_dir(&service.environment, &service.name)?;
    let mut manifest = load_manifest(&dir);
    let files = locations::list_files(source, location, from, to).await?;

    // An undated growing file is not mirrored one-to-one: its lines are split
    // into per-day segments that survive the remote file's rotation
    if !location.dated {
        return crate::growing::sync_growing(
            source, &dir, service, &mut manifest, files, cancel, on_progress,
        )
        .await;
    }

    let mut summary = SyncSummary {
        service_id: service.id.clone(),
        files_total: files.len(),
        files_downloaded: 0,
        files_skipped: 0,
        files_failed: 0,
        bytes_downloaded: 0,
        warnings: Vec::new(),
    };

    let instances: std::collections::HashSet<_> = files
        .iter()
        .filter_map(|f| f.instance.as_deref())
        .collect();
    if instances.len() > 1 {
        summary.warnings.push(format!(
            "Service ran on {} instances in this range; Kudu only exposes files present on disk - verify coverage",
            instances.len()
        ));
    }

    let today = chrono::Local::now().date_naive();
    let file_count = files.len();
    for (index, file) in files.into_iter().enumerate() {
        // Stop before starting another file; the manifest is saved after each
        // completed one, so what was already downloaded is safely cached.
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // A manifest entry is usable only while its compressed file exists.
        // Otherwise skipping it loses the file, and resuming fetches only its tail.
        let cached = manifest
            .entries
            .get(&file.vfs_path)
            .filter(|entry| dir.join(&entry.local_name).is_file());
        let unchanged = cached.is_some_and(|entry| {
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

        // For an unchanged live file the sizes are equal and the ranged probe
        // starts at the end - zero new bytes confirms the cache is current.
        let known_size = cached
            .map(|entry| entry.remote_size)
            .filter(|&size| size < file.size || unchanged);
        let known_range = cached.map_or_else(TimeRange::default, |entry| TimeRange {
            first: entry.first_ts,
            last: entry.last_ts,
        });

        // A failing file gets one more full attempt before it counts as failed -
        // on top of the per-request retries inside download_file, this restarts
        // the transfer from scratch, which also clears a poisoned partial state.
        let mut result: Result<StoredFile, String> = Err(String::new());
        for attempt in 0..2 {
            if attempt > 0 {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            result = download_and_compress(
                source,
                &dir,
                &file,
                known_size,
                cancel,
                |downloaded, state| {
                    on_progress(SyncProgress {
                        service_id: service.id.clone(),
                        file_name: file.name.clone(),
                        file_index: index + 1,
                        file_count,
                        bytes_downloaded: downloaded,
                        // The listing can lag behind the content actually transferred.
                        total_bytes: file.size.max(downloaded),
                        state: state.into(),
                    });
                },
            )
            .await;
            if result.is_ok() {
                break;
            }
        }
        let stored = match result {
            Ok(stored) => stored,
            // A single file failing to download must not abort the rest of the
            // sync - record it and move on. Leaving it out of the manifest means
            // the next sync retries it.
            Err(e) => {
                summary.files_failed += 1;
                summary.warnings.push(format!("{}: download failed - {e}", file.name));
                on_progress(SyncProgress {
                    service_id: service.id.clone(),
                    file_name: file.name.clone(),
                    file_index: index + 1,
                    file_count,
                    bytes_downloaded: 0,
                    total_bytes: file.size,
                    state: "failed".into(),
                });
                continue;
            }
        };

        // A probe that found nothing past the cached size - the file is current
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
        // was actually stored - the next ranged sync must start past it
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
        if stored.truncated {
            summary.warnings.push(format!(
                "{}: transfer interrupted - kept {} of {} bytes, the rest resumes on the next sync",
                file.name, cached_size, file.size
            ));
        }
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

/// A file whose content can still grow: dated today (or later), or undated -
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
    /// Span of the downloaded chunk - the whole file, or just the new tail
    range: TimeRange,
    appended: bool,
    /// The transfer was cut short and salvaged: what is stored is a valid prefix,
    /// and the next ranged sync resumes past it.
    truncated: bool,
}

/// Downloads (fully, or from `range_from` for grown files) and stores the file
/// as zstd. A ranged download appends a new frame; a full one replaces the file.
async fn download_and_compress(
    source: &LogSource,
    dir: &std::path::Path,
    file: &RemoteLogFile,
    range_from: Option<u64>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, &str),
) -> Result<StoredFile, String> {
    let temp_path = dir.join(format!("{}.download", file.name));
    let final_path = dir.join(format!("{}.zst", file.name));

    let result = source
        .download_file(&file.vfs_path, &temp_path, range_from, Some(file.size), cancel, |d| {
            on_progress(d, "downloading")
        })
        .await?;

    // A ranged probe answered with no bytes - nothing new past the cached size
    if result.was_partial && result.bytes_written == 0 {
        std::fs::remove_file(&temp_path).ok();
        return Ok(StoredFile {
            bytes: 0,
            range: TimeRange::default(),
            appended: true,
            truncated: false,
        });
    }

    // Scanning and compressing a large file takes a while - tell the UI the
    // download itself is finished so it does not look stuck at 100%
    on_progress(result.bytes_written, "processing");

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
        truncated: result.truncated,
    })
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    pub files_written: usize,
    pub bytes_written: u64,
    /// The per-service folder the files were written to
    pub target_dir: String,
    pub warnings: Vec<String>,
}

/// Decompresses the service's cached files that fall in `[from, to]` into
/// `target_root/<env>/<name>/`, one plain-text file per cached file (the `.zst`
/// suffix dropped). Undated growing files (Log.txt) are always included, since
/// they cannot be sliced by day. The per-service subfolder is created only once
/// there is something to write into it.
pub fn export_service(
    service: &ServiceConfig,
    from: NaiveDate,
    to: NaiveDate,
    target_root: &std::path::Path,
) -> Result<ExportSummary, String> {
    let dir = service_dir_path(&service.environment, &service.name)?;
    let manifest = load_manifest(&dir);
    let out_dir = target_root
        .join(&service.environment)
        .join(&service.name);
    let mut summary = ExportSummary {
        target_dir: out_dir.display().to_string(),
        ..Default::default()
    };

    for entry in manifest.entries.values() {
        // Dated files outside the range are skipped; undated ones always export
        if let Some(date) = entry.date {
            if date < from || date > to {
                continue;
            }
        }
        // The name ultimately derives from a remote Kudu listing; refuse any that
        // carries a path separator or `..` so a decompressed file can never land
        // outside the service's export folder.
        let name = entry.local_name.trim_end_matches(".zst");
        if std::path::Path::new(name)
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            summary.warnings.push(format!("Skipped unsafe file name: {name}"));
            continue;
        }
        let source = dir.join(&entry.local_name);
        if !source.exists() {
            summary.warnings.push(format!(
                "{name}: cached file is missing; sync its date range to fetch it again"
            ));
            continue;
        }
        std::fs::create_dir_all(&out_dir)
            .map_err(|e| format!("Cannot create {}: {e}", out_dir.display()))?;
        let target = out_dir.join(name);
        if let Err(e) = decompress_to(&source, &target) {
            summary.warnings.push(e);
            continue;
        }
        summary.files_written += 1;
        summary.bytes_written += std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
    }
    Ok(summary)
}

/// Decompresses one cached file (`<file>.zst`) to a path the user chose. Returns
/// the number of bytes written. Used by the "Export" actions in the Files view
/// and the raw viewer, which both name a file by service and base name.
pub fn export_cached_file(
    service: &ServiceConfig,
    file: &str,
    target: &std::path::Path,
) -> Result<u64, String> {
    let dir = service_dir_path(&service.environment, &service.name)?;
    let source = dir.join(format!("{file}.zst"));
    if !source.exists() {
        return Err(format!("{file} is not cached for {}", service.name));
    }
    decompress_to(&source, target)?;
    Ok(std::fs::metadata(target).map(|m| m.len()).unwrap_or(0))
}

/// Decompresses one zstd cache file to a plain-text file. Cache files may hold
/// several concatenated zstd frames (incremental appends) - the decoder reads
/// them all.
fn decompress_to(source: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    let raw = std::fs::File::open(source)
        .map_err(|e| format!("Cannot open {}: {e}", source.display()))?;
    let output = std::fs::File::create(target)
        .map_err(|e| format!("Cannot create {}: {e}", target.display()))?;
    zstd::stream::copy_decode(std::io::BufReader::new(raw), output)
        .map_err(|e| format!("zstd decompression failed for {}: {e}", source.display()))
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
    /// Holes inside the oldest-newest span - see `coverage_gaps`
    pub gaps: Vec<CoverageGap>,
}

/// A span with nothing cached: from the last cached line before the hole to the
/// first cached line after it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageGap {
    pub from: NaiveDateTime,
    pub to: NaiveDateTime,
}

/// Whole days with nothing cached, strictly between the first and last covered
/// day. Each gap carries the exact cached timestamps on either side, so the UI
/// can say precisely what span is missing. Days are the unit on purpose: the
/// slack between one day's last line and the next day's first (a quiet night)
/// is not a gap.
fn coverage_gaps(manifest: &Manifest) -> Vec<CoverageGap> {
    let day_start = |day: NaiveDate| day.and_hms_opt(0, 0, 0).expect("midnight is valid");
    let day_end = |day: NaiveDate| day.and_hms_opt(23, 59, 59).expect("valid time");

    // Day -> first and last cached moment on it. Entries without timestamps
    // fall back to the whole day their filename dates; entries with neither
    // cannot place any coverage and are skipped.
    let mut days: std::collections::BTreeMap<NaiveDate, (NaiveDateTime, NaiveDateTime)> =
        std::collections::BTreeMap::new();
    for entry in manifest.entries.values() {
        let first = entry.first_ts.or_else(|| entry.date.map(day_start));
        let last = entry.last_ts.or_else(|| entry.date.map(day_end));
        let (Some(first), Some(last)) = (first, last) else {
            continue;
        };
        let mut day = first.date();
        while day <= last.date() {
            let covered = (first.max(day_start(day)), last.min(day_end(day)));
            days.entry(day)
                .and_modify(|(f, l)| {
                    *f = (*f).min(covered.0);
                    *l = (*l).max(covered.1);
                })
                .or_insert(covered);
            let Some(next) = day.succ_opt() else { break };
            day = next;
        }
    }

    let mut gaps = Vec::new();
    let mut previous: Option<(NaiveDate, NaiveDateTime)> = None;
    for (&day, &(first, last)) in &days {
        if let Some((previous_day, previous_last)) = previous {
            if day.signed_duration_since(previous_day).num_days() > 1 {
                gaps.push(CoverageGap {
                    from: previous_last,
                    to: first,
                });
            }
        }
        previous = Some((day, last));
    }
    gaps
}

/// No real time zone is further from UTC than this, so a wider delta is evidence
/// of a stale mtime, not of a log clock.
const MAX_OFFSET_MINUTES: i64 = 14 * 60;

/// Minutes the service's log clock runs ahead of UTC, or `None` if no cached file
/// can say. A log line carries no zone, and an App Service with `WEBSITE_TIME_ZONE`
/// set writes wall-clock local time while a default one writes UTC - the same line
/// either way. Kudu dates every file in UTC, and a log file's last write *is* its
/// last timestamped line, so `last_ts - mtime` measures the clock the lines are in.
/// This holds for files of any age, unlike comparing the newest line against "now",
/// which only says anything while a service is actively logging.
///
/// Reduced by median because a manifest's mtime comes from the listing that preceded
/// the download and can lag content appended after it - such a file reads hours ahead
/// on its own, but cannot move the middle of the set.
///
/// Day segments of a growing file break the "last write is the last line" rule: a
/// closed segment keeps the source file's mtime from the sync that last appended to
/// it, hours after that day's final line, and recent ones pass the sanity filter
/// with deltas that read as a fictional zone. Only the newest segment of each
/// growing file - the one the last sync actually ended on - measures the clock.
pub fn detect_offset_minutes(manifest: &Manifest) -> Option<i32> {
    let mut newest_segment: HashMap<&str, NaiveDate> = HashMap::new();
    for key in manifest.entries.keys() {
        if let Some((path, day)) = crate::growing::split_segment_key(key) {
            let newest = newest_segment.entry(path).or_insert(day);
            *newest = (*newest).max(day);
        }
    }
    let mut deltas: Vec<i64> = manifest
        .entries
        .iter()
        .filter_map(|(key, entry)| {
            if let Some((path, day)) = crate::growing::split_segment_key(key) {
                if newest_segment.get(path) != Some(&day) {
                    return None;
                }
            }
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
    let dir = service_dir_path(&service.environment, &service.name).ok()?;
    detect_offset_minutes(&load_manifest(&dir))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedFileInfo {
    pub service_id: String,
    /// Name as the search hits report it - the cache file without the .zst suffix
    pub file: String,
    pub date: Option<NaiveDate>,
    pub size_bytes: u64,
    pub compressed_bytes: u64,
    pub instance: Option<String>,
}

/// The service's cached files, newest first. Undated files (growing Log.txt) sort last.
pub fn cached_files(service: &ServiceConfig) -> Result<Vec<CachedFileInfo>, String> {
    let dir = service_dir_path(&service.environment, &service.name)?;
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
    let dir = service_dir_path(&service.environment, &service.name)?;
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
    // Deleting the last day segment of a growing file also forgets its sync
    // state, so a later sync re-downloads whatever the server still has
    let entries = &manifest.entries;
    manifest.growing.retain(|vfs_path, _| {
        entries
            .keys()
            .any(|k| k.strip_prefix(vfs_path.as_str()).is_some_and(|rest| rest.starts_with('@')))
    });
    save_manifest(&dir, &manifest)
}

pub fn cache_status(service: &ServiceConfig) -> Result<CacheStatus, String> {
    let dir = service_dir_path(&service.environment, &service.name)?;
    let manifest = load_manifest(&dir);
    let mut status = CacheStatus {
        service_id: service.id.clone(),
        files: manifest.entries.len(),
        compressed_bytes: 0,
        uncompressed_bytes: 0,
        oldest: None,
        newest: None,
        log_offset_minutes: detect_offset_minutes(&manifest),
        gaps: coverage_gaps(&manifest),
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

    #[tokio::test]
    async fn sync_restores_missing_cached_files_before_export() {
        let root = std::env::temp_dir().join(format!(
            "loglooker-missing-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("LOGLOOKER_CACHE_DIR", root.join("cache"));
        let pack = crate::plugin::parse_and_compile(
            r#"{
                "id": "local", "name": "Local", "source": { "type": "local-folder" },
                "logLocations": [{ "id": "logs", "label": "Logs", "dir": ".",
                    "file": "^log-(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})\\.log$" }]
            }"#,
            crate::plugin::PackOrigin::Bundled,
            false,
        )
        .unwrap();
        let location = pack.location("logs").unwrap();
        let today = chrono::Local::now().date_naive();
        for (name, day, grows) in [
            ("historical", today.pred_opt().unwrap(), false),
            ("live", today, false),
            ("grown", today, true),
        ] {
            let source_dir = root.join(name);
            std::fs::create_dir_all(&source_dir).unwrap();
            let filename = format!("log-{day}.log");
            let original = format!("{day} 10:00:00 INFO original content\n");
            std::fs::write(source_dir.join(&filename), &original).unwrap();
            let source = LogSource::new(
                &crate::plugin::SourceDef::LocalFolder { root: None },
                source_dir.to_str().unwrap(),
                None,
            )
            .unwrap();
            let service: ServiceConfig = serde_json::from_value(serde_json::json!({
                "id": format!("test/{name}"), "name": name, "environment": "test"
            }))
            .unwrap();
            let cancel = AtomicBool::new(false);
            sync_service(&source, &service, location, day, day, &cancel, |_| {})
                .await
                .unwrap();
            let cached = service_dir_path("test", name)
                .unwrap()
                .join(format!("{filename}.zst"));
            std::fs::remove_file(&cached).unwrap();
            let missing = export_service(&service, day, day, &root.join("export")).unwrap();
            assert_eq!(missing.files_written, 0);
            assert!(missing
                .warnings
                .iter()
                .any(|warning| warning.contains("cached file is missing")));
            let expected = if grows {
                format!("{original}{day} 11:00:00 INFO appended content\n")
            } else {
                original
            };
            if grows {
                std::fs::write(source_dir.join(&filename), &expected).unwrap();
            }

            let summary = sync_service(&source, &service, location, day, day, &cancel, |_| {})
                .await
                .unwrap();
            assert_eq!(
                summary.files_downloaded, 1,
                "{name}: missing cache must be fetched"
            );
            assert_eq!(summary.files_skipped, 0, "{name}");
            let export = export_service(&service, day, day, &root.join("export")).unwrap();
            assert_eq!(export.files_written, 1, "{name}");
            assert_eq!(
                std::fs::read_to_string(std::path::Path::new(&export.target_dir).join(filename))
                    .unwrap(),
                expected
            );
        }
        std::env::remove_var("LOGLOOKER_CACHE_DIR");
        std::fs::remove_dir_all(root).unwrap();
    }

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
    /// line reads `last_ts` - the pair the offset is measured from.
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
            ..Default::default()
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

    /// One manifest entry spanning `first..last`, dated when the name is - the
    /// shape coverage_gaps reads.
    fn spanning(name: &str, date: Option<&str>, first: Option<&str>, last: Option<&str>) -> (String, CachedFile) {
        let parse_ts = |ts: &str| NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S").unwrap();
        (
            name.to_string(),
            CachedFile {
                remote_size: 10,
                remote_mtime: "2026-07-14T10:00:00Z".to_string(),
                local_name: format!("{name}.zst"),
                date: date.map(|d| d.parse().unwrap()),
                instance: None,
                first_ts: first.map(parse_ts),
                last_ts: last.map(parse_ts),
            },
        )
    }

    #[test]
    fn a_missing_day_is_a_gap_bounded_by_the_cached_lines_around_it() {
        let manifest = manifest_of(vec![
            spanning("a", None, Some("2026-07-15T06:00:01"), Some("2026-07-15T21:14:09")),
            spanning("b", None, Some("2026-07-17T05:30:12"), Some("2026-07-17T22:00:00")),
        ]);
        assert_eq!(
            coverage_gaps(&manifest),
            vec![CoverageGap {
                from: "2026-07-15T21:14:09".parse().unwrap(),
                to: "2026-07-17T05:30:12".parse().unwrap(),
            }]
        );
    }

    #[test]
    fn consecutive_days_have_no_gap_despite_the_quiet_night_between_them() {
        let manifest = manifest_of(vec![
            spanning("a", None, Some("2026-07-15T06:00:01"), Some("2026-07-15T21:14:09")),
            spanning("b", None, Some("2026-07-16T05:30:12"), Some("2026-07-16T22:00:00")),
        ]);
        assert_eq!(coverage_gaps(&manifest), vec![]);
    }

    #[test]
    fn an_entry_spanning_several_days_bridges_them() {
        let manifest = manifest_of(vec![
            spanning("a", None, Some("2026-07-13T09:00:00"), Some("2026-07-16T04:00:00")),
            spanning("b", None, Some("2026-07-16T05:30:12"), Some("2026-07-16T22:00:00")),
        ]);
        assert_eq!(coverage_gaps(&manifest), vec![]);
    }

    #[test]
    fn a_dated_entry_without_timestamps_counts_as_its_whole_day() {
        // Cached before time ranges were recorded - the filename still dates it
        let manifest = manifest_of(vec![
            spanning("a", Some("2026-07-13"), None, None),
            spanning("b", None, Some("2026-07-15T05:30:12"), Some("2026-07-15T22:00:00")),
        ]);
        assert_eq!(
            coverage_gaps(&manifest),
            vec![CoverageGap {
                from: "2026-07-13T23:59:59".parse().unwrap(),
                to: "2026-07-15T05:30:12".parse().unwrap(),
            }]
        );
    }

    #[test]
    fn closed_day_segments_of_a_growing_file_do_not_skew_the_offset() {
        // Seen in the wild: closed segments keep the mtime of the sync that last
        // appended to them, hours after their own last line. Read together they
        // said the log clock ran on UTC-6:30; only the newest segment may count.
        let manifest = manifest_of(vec![
            cached("app/Log.txt@2026-07-19", "2026-07-20T07:44:35Z", Some("2026-07-19T23:55:01")),
            cached("app/Log.txt@2026-07-20", "2026-07-21T06:25:01Z", Some("2026-07-20T23:55:01")),
            cached("app/Log.txt@2026-07-21", "2026-07-21T08:22:32Z", Some("2026-07-21T08:22:32")),
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

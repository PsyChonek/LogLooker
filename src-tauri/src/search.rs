use crate::cache;
use crate::config::ServiceConfig;
use crate::memcache::{MemCache, Source};
use chrono::{NaiveDate, NaiveDateTime};
use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub service_ids: Vec<String>,
    pub date_from: NaiveDate,
    pub date_to: NaiveDate,
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    #[serde(default = "default_context")]
    pub context_lines: usize,
}

fn default_context() -> usize {
    2
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub service_id: String,
    pub file: String,
    pub line_number: u64,
    pub timestamp: Option<NaiveDateTime>,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
    pub fields: ExtractedFields,
    /// Named capture groups of the query that matched somewhere in this line
    pub matched_groups: Vec<String>,
}

/// Fields pulled from the self-contained MediatR/request lines. Never attribute
/// by line proximity — the APIs are concurrent and streams interleave.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedFields {
    pub id_login: Option<String>,
    pub id_command: Option<String>,
    pub operation: Option<String>,
    pub duration_ms: Option<f64>,
}

/// Live progress of a running search. Files scan in parallel, so `current_file`
/// is merely the file that finished most recently, not a strict position.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchProgress {
    pub phase: String,
    pub files_done: usize,
    pub files_total: usize,
    pub hits: usize,
    pub lines_scanned: u64,
    pub current_file: String,
}

/// Full, untruncated result. The command layer keeps it in state and serves
/// pages to the UI, so millions of hits never cross IPC at once.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    /// Named capture groups of the query, in pattern order
    pub group_names: Vec<String>,
    pub files_scanned: usize,
    pub lines_scanned: u64,
    pub duration_ms: u64,
}

/// Shared by the cross-service search and the raw viewer's in-file search.
/// Patterns the fast `regex` engine rejects (look-around, as compiled from the
/// query builder's ALL/NOT criteria) fall back to the backtracking engine.
pub struct Matcher {
    engine: Engine,
    /// Named capture groups of the pattern, in definition order. Hits record
    /// which of them matched, so the UI can colour lines per group and charts
    /// can draw one series per group.
    group_names: Vec<String>,
}

enum Engine {
    /// Empty query — every line in the selected services and date range matches
    All,
    Substring(String),
    Pattern(Regex),
    /// Look-around pattern the fast engine cannot run
    Fancy(fancy_regex::Regex),
}

/// Lines can hold megabytes of SQL or HTML; the per-group scan stops after this
/// many matches instead of walking all of them.
const GROUP_SCAN_CAP: usize = 200;

impl Matcher {
    pub fn new(query: &str, is_regex: bool, case_sensitive: bool) -> Result<Self, String> {
        if query.is_empty() {
            return Ok(Matcher {
                engine: Engine::All,
                group_names: Vec::new(),
            });
        }
        if !is_regex && case_sensitive {
            return Ok(Matcher {
                engine: Engine::Substring(query.to_string()),
                group_names: Vec::new(),
            });
        }
        // A case-insensitive substring runs as an escaped literal pattern: the
        // regex engine folds case as it scans, `line.to_lowercase()` would
        // allocate a String for every line of every file.
        let pattern = if is_regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        let pattern = if case_sensitive {
            pattern
        } else {
            format!("(?i){pattern}")
        };
        match Regex::new(&pattern) {
            Ok(regex) => Ok(Matcher {
                group_names: regex.capture_names().flatten().map(str::to_string).collect(),
                engine: Engine::Pattern(regex),
            }),
            Err(_) => match fancy_regex::Regex::new(&pattern) {
                Ok(regex) => Ok(Matcher {
                    group_names: regex.capture_names().flatten().map(str::to_string).collect(),
                    engine: Engine::Fancy(regex),
                }),
                // The fancy engine accepts a superset of the syntax, so when
                // both refuse the pattern its error names the real problem
                Err(e) => Err(format!("Invalid regex: {e}")),
            },
        }
    }

    fn build(request: &SearchRequest) -> Result<Self, String> {
        Matcher::new(&request.query, request.is_regex, request.case_sensitive)
    }

    pub fn group_names(&self) -> &[String] {
        &self.group_names
    }

    pub fn matches(&self, line: &str) -> bool {
        match &self.engine {
            Engine::All => true,
            Engine::Substring(needle) => line.contains(needle.as_str()),
            Engine::Pattern(regex) => regex.is_match(line),
            Engine::Fancy(regex) => regex.is_match(line).unwrap_or(false),
        }
    }

    /// Match test that also reports which named groups took part anywhere in
    /// the line; `None` is no match. Patterns without named groups skip the
    /// capture cost entirely and behave exactly like `matches`.
    pub fn match_line(&self, line: &str) -> Option<Vec<String>> {
        if self.group_names.is_empty() {
            return self.matches(line).then(Vec::new);
        }
        let mut found = vec![false; self.group_names.len()];
        let mut matched = false;
        match &self.engine {
            Engine::Pattern(regex) => {
                for caps in regex.captures_iter(line).take(GROUP_SCAN_CAP) {
                    matched = true;
                    self.mark(&mut found, |name| caps.name(name).is_some());
                    if found.iter().all(|seen| *seen) {
                        break;
                    }
                }
            }
            Engine::Fancy(regex) => {
                for caps in regex.captures_iter(line).take(GROUP_SCAN_CAP).flatten() {
                    matched = true;
                    self.mark(&mut found, |name| caps.name(name).is_some());
                    if found.iter().all(|seen| *seen) {
                        break;
                    }
                }
            }
            // All and Substring never carry groups
            _ => return self.matches(line).then(Vec::new),
        }
        matched.then(|| {
            self.group_names
                .iter()
                .zip(&found)
                .filter(|(_, seen)| **seen)
                .map(|(name, _)| name.clone())
                .collect()
        })
    }

    fn mark(&self, found: &mut [bool], participated: impl Fn(&str) -> bool) {
        for (name, seen) in self.group_names.iter().zip(found.iter_mut()) {
            *seen = *seen || participated(name);
        }
    }
}

struct ScanFile {
    service_id: String,
    path: PathBuf,
    name: String,
    /// Size of the original log, from the manifest. The memory cache budgets the
    /// decompressed copy against it before it decompresses anything.
    uncompressed_size: u64,
    /// Undated growing files (Log.txt) need per-line date filtering
    filter_lines_by_date: bool,
}

/// Runs a search across the cached files of the selected services.
/// CPU-bound; call via spawn_blocking. Files scan in parallel (rayon),
/// results merge into one timeline sorted by timestamp.
/// `on_progress` is called from worker threads as files finish.
///
/// Each file is read through `cache`, which serves it from RAM when it is held
/// there and streams it from the zstd file on disk when it is not.
pub fn run(
    request: &SearchRequest,
    services: &[ServiceConfig],
    cache: &MemCache,
    on_progress: impl Fn(SearchProgress) + Sync,
) -> Result<SearchResult, String> {
    let started = std::time::Instant::now();
    let matcher = Matcher::build(request)?;
    let files = collect_files(request, services)?;
    let files_scanned = files.len();

    cache.begin_pass();

    let files_done = AtomicUsize::new(0);
    let hits_found = AtomicUsize::new(0);
    let lines_seen = AtomicU64::new(0);
    on_progress(SearchProgress {
        phase: "scanning".into(),
        files_done: 0,
        files_total: files_scanned,
        hits: 0,
        lines_scanned: 0,
        current_file: String::new(),
    });

    let per_file: Vec<Result<(Vec<SearchHit>, u64), String>> = files
        .par_iter()
        .map(|file| {
            let result = scan_file(file, &matcher, request, cache);
            if let Ok((file_hits, file_lines)) = &result {
                let done = files_done.fetch_add(1, Ordering::Relaxed) + 1;
                let hits = hits_found.fetch_add(file_hits.len(), Ordering::Relaxed) + file_hits.len();
                let lines = lines_seen.fetch_add(*file_lines, Ordering::Relaxed) + *file_lines;
                on_progress(SearchProgress {
                    phase: "scanning".into(),
                    files_done: done,
                    files_total: files_scanned,
                    hits,
                    lines_scanned: lines,
                    current_file: file.name.clone(),
                });
            }
            result
        })
        .collect();

    // An unfiltered query makes a hit of every line, so the merged vector can
    // reach millions of entries. Sizing it once beats growing it by doubling.
    let total: usize = per_file
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .map(|(file_hits, _)| file_hits.len())
        .sum();
    let mut hits = Vec::with_capacity(total);
    let mut lines_scanned = 0;
    for result in per_file {
        let (file_hits, file_lines) = result?;
        hits.extend(file_hits);
        lines_scanned += file_lines;
    }

    // Sorting millions of hits takes long enough to deserve its own phase
    on_progress(SearchProgress {
        phase: "sorting".into(),
        files_done: files_scanned,
        files_total: files_scanned,
        hits: hits.len(),
        lines_scanned,
        current_file: String::new(),
    });

    // Merged multi-service timeline; undatable lines sort last
    hits.sort_by(|a, b| match (a.timestamp, b.timestamp) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    Ok(SearchResult {
        hits,
        group_names: matcher.group_names().to_vec(),
        files_scanned,
        lines_scanned,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

fn collect_files(
    request: &SearchRequest,
    services: &[ServiceConfig],
) -> Result<Vec<ScanFile>, String> {
    let mut files = Vec::new();
    for id in &request.service_ids {
        let service = services
            .iter()
            .find(|s| &s.id == id)
            .ok_or_else(|| format!("Unknown service: {id}"))?;
        let dir = cache::service_dir_path(service.environment, &service.name)?;
        let manifest = cache::load_manifest(&dir);
        for entry in manifest.entries.values() {
            let in_range = match entry.date {
                Some(date) => date >= request.date_from && date <= request.date_to,
                None => true,
            };
            if !in_range {
                continue;
            }
            files.push(ScanFile {
                service_id: service.id.clone(),
                path: dir.join(&entry.local_name),
                name: entry.local_name.trim_end_matches(".zst").to_string(),
                uncompressed_size: entry.remote_size,
                filter_lines_by_date: entry.date.is_none(),
            });
        }
    }
    Ok(files)
}

fn scan_file(
    file: &ScanFile,
    matcher: &Matcher,
    request: &SearchRequest,
    cache: &MemCache,
) -> Result<(Vec<SearchHit>, u64), String> {
    // Text in memory can be split at line boundaries and scanned on every core.
    // A zstd stream can only be read from the front, so a search over a single
    // file — the common case — would otherwise be stuck on one core.
    if let Some(text) = cache.load(&file.path, file.uncompressed_size)? {
        return Ok(scan_memory(file, matcher, request, &text));
    }

    // Too big for the budget, or the cache is off: stream it, one core
    let source = cache.source(&file.path)?;
    let mut reader = source.reader()?;
    let mut scanner = Scanner::new(file, matcher, request);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|e| format!("Read failed in {}: {e}", file.name))?;
        if read == 0 {
            break;
        }
        scanner.push(&String::from_utf8_lossy(&buffer));
    }
    Ok(scanner.finish())
}

/// Text below this is not worth splitting — the per-chunk setup would cost more
/// than the scan it saves.
const PARALLEL_MIN_BYTES: usize = 4 * 1024 * 1024;

/// Scans text held in memory, in parallel over line-aligned chunks.
///
/// A chunk cannot be scanned in isolation: its first lines may inherit a
/// timestamp from an entry that opened before it, a hit in its first lines needs
/// context from the chunk before, and a hit in its last lines needs context from
/// the chunk after. All three are read straight out of the shared text.
fn scan_memory(
    file: &ScanFile,
    matcher: &Matcher,
    request: &SearchRequest,
    text: &[u8],
) -> (Vec<SearchHit>, u64) {
    let chunks = split_at_lines(text, rayon::current_num_threads());

    // Each chunk numbers its lines from its own start; the offsets are only
    // known once every chunk has counted, so they are added afterwards.
    let scanned: Vec<(Vec<SearchHit>, u64)> = chunks
        .par_iter()
        .map(|chunk| {
            let mut scanner = Scanner::resume(
                file,
                matcher,
                request,
                timestamp_before(text, chunk.start),
                context_before(text, chunk.start, request.context_lines),
            );
            for raw in text[chunk.clone()].split_inclusive(|byte| *byte == b'\n') {
                scanner.push(&String::from_utf8_lossy(raw));
            }
            // The after-context of a hit at the end of a chunk lives in the next
            for raw in text[chunk.end..]
                .split_inclusive(|byte| *byte == b'\n')
                .take(request.context_lines)
            {
                scanner.push_after_context(&String::from_utf8_lossy(raw));
            }
            scanner.finish()
        })
        .collect();

    let total: usize = scanned.iter().map(|(hits, _)| hits.len()).sum();
    let mut hits = Vec::with_capacity(total);
    let mut lines_before: u64 = 0;
    for (mut chunk_hits, chunk_lines) in scanned {
        for hit in &mut chunk_hits {
            hit.line_number += lines_before;
        }
        hits.extend(chunk_hits);
        lines_before += chunk_lines;
    }
    (hits, lines_before)
}

/// Splits into at most `parts` ranges, each starting just after a newline so no
/// line is cut in half.
fn split_at_lines(text: &[u8], parts: usize) -> Vec<std::ops::Range<usize>> {
    if parts <= 1 || text.len() < PARALLEL_MIN_BYTES {
        return vec![0..text.len()];
    }
    let mut bounds = vec![0];
    for part in 1..parts {
        let mut at = text.len() / parts * part;
        while at < text.len() && text[at] != b'\n' {
            at += 1;
        }
        if at < text.len() {
            at += 1;
        }
        if at > *bounds.last().expect("seeded with 0") {
            bounds.push(at);
        }
    }
    bounds.push(text.len());
    bounds
        .windows(2)
        .map(|pair| pair[0]..pair[1])
        .filter(|range| !range.is_empty())
        .collect()
}

/// Walks back from a chunk start for the timestamp its first lines inherit. A
/// run of continuation lines longer than the limit (a multi-megabyte SQL body)
/// yields none — exactly as at the start of a file.
fn timestamp_before(text: &[u8], start: usize) -> Option<NaiveDateTime> {
    const LOOKBACK_LINES: usize = 4096;
    let mut end = start;
    for _ in 0..LOOKBACK_LINES {
        let (line, line_start) = previous_line(text, end)?;
        if let Ok(text) = std::str::from_utf8(line) {
            if let Some(ts) = parse_timestamp(text.trim_end_matches('\r')) {
                return Some(ts);
            }
        }
        end = line_start;
    }
    None
}

/// The lines a chunk's first hits need as their before-context.
fn context_before(text: &[u8], start: usize, count: usize) -> VecDeque<String> {
    let mut lines = VecDeque::with_capacity(count + 1);
    let mut end = start;
    for _ in 0..count {
        let Some((line, line_start)) = previous_line(text, end) else {
            break;
        };
        let line = String::from_utf8_lossy(line);
        lines.push_front(line.trim_end_matches('\r').to_string());
        end = line_start;
    }
    lines
}

/// The line ending at `end` (which sits just past its newline), and where it
/// starts. `None` at the beginning of the text.
fn previous_line(text: &[u8], end: usize) -> Option<(&[u8], usize)> {
    if end == 0 {
        return None;
    }
    let newline = end - 1;
    let line_start = text[..newline]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    Some((&text[line_start..newline], line_start))
}

/// The per-line body of a scan, shared by the in-memory and the streaming path.
struct Scanner<'a> {
    file: &'a ScanFile,
    matcher: &'a Matcher,
    request: &'a SearchRequest,
    hits: Vec<SearchHit>,
    /// Hits still collecting after-context: (index into `hits`, lines remaining)
    pending_after: Vec<(usize, usize)>,
    before: VecDeque<String>,
    last_timestamp: Option<NaiveDateTime>,
    line_number: u64,
}

impl<'a> Scanner<'a> {
    fn new(file: &'a ScanFile, matcher: &'a Matcher, request: &'a SearchRequest) -> Self {
        Scanner::resume(file, matcher, request, None, VecDeque::new())
    }

    /// A scanner picking up in the middle of a file, as a parallel chunk does:
    /// it starts with the timestamp its first lines inherit and the context
    /// lines that precede them. Line numbers count from its own start.
    fn resume(
        file: &'a ScanFile,
        matcher: &'a Matcher,
        request: &'a SearchRequest,
        last_timestamp: Option<NaiveDateTime>,
        before: VecDeque<String>,
    ) -> Self {
        let mut ring = VecDeque::with_capacity(request.context_lines + 1);
        ring.extend(before);
        Scanner {
            file,
            matcher,
            request,
            hits: Vec::new(),
            pending_after: Vec::new(),
            before: ring,
            last_timestamp,
            line_number: 0,
        }
    }

    /// Feeds a line that may only complete the after-context of hits already
    /// found — it can never become a hit itself. A chunk uses this for the lines
    /// past its end, which belong to the next chunk.
    fn push_after_context(&mut self, line: &str) {
        let line = line.trim_end_matches(['\r', '\n']);
        for (hit_index, remaining) in self.pending_after.iter_mut() {
            self.hits[*hit_index].context_after.push(line.to_string());
            *remaining -= 1;
        }
        self.pending_after.retain(|(_, remaining)| *remaining > 0);
    }

    fn push(&mut self, line: &str) {
        let line = line.trim_end_matches(['\r', '\n']);
        self.line_number += 1;

        // Lines without a timestamp (stack traces, continuations) inherit the last one
        if let Some(ts) = parse_timestamp(line) {
            self.last_timestamp = Some(ts);
        }

        for (hit_index, remaining) in self.pending_after.iter_mut() {
            self.hits[*hit_index].context_after.push(line.to_string());
            *remaining -= 1;
        }
        self.pending_after.retain(|(_, remaining)| *remaining > 0);

        let date_ok = !self.file.filter_lines_by_date
            || self.last_timestamp.is_some_and(|ts| {
                ts.date() >= self.request.date_from && ts.date() <= self.request.date_to
            });

        if date_ok {
            if let Some(matched_groups) = self.matcher.match_line(line) {
                self.hits.push(SearchHit {
                    service_id: self.file.service_id.clone(),
                    file: self.file.name.clone(),
                    line_number: self.line_number,
                    timestamp: self.last_timestamp,
                    line: line.to_string(),
                    context_before: self.before.iter().cloned().collect(),
                    context_after: Vec::new(),
                    fields: extract_fields(line),
                    matched_groups,
                });
                if self.request.context_lines > 0 {
                    self.pending_after
                        .push((self.hits.len() - 1, self.request.context_lines));
                }
            }
        }

        // The ring rotates its Strings instead of allocating one per line — this
        // runs for every line of every file, matching or not.
        if self.request.context_lines > 0 {
            let mut slot = if self.before.len() == self.request.context_lines {
                self.before.pop_front().unwrap_or_default()
            } else {
                String::new()
            };
            slot.clear();
            slot.push_str(line);
            self.before.push_back(slot);
        }
    }

    fn finish(self) -> (Vec<SearchHit>, u64) {
        (self.hits, self.line_number)
    }
}

/// Known line-start formats:
///   2026-07-15T08:08:09.9533870Z  (IIS/W3C ISO 8601, up to 100ns precision)
///   13.07.2026 00:00:02.611       (core apps, ms precision)
///   14.07.2026 01:16:45           (framework Log.txt)
///   7/14/2026 1:15:55 AM          (framework Log.txt, mixed US format)
///
/// Continuation lines (SQL bodies, embedded HTML, stack traces) have no
/// timestamp of their own and yield None.
pub fn parse_timestamp(line: &str) -> Option<NaiveDateTime> {
    let bytes = line.as_bytes();
    if !bytes.first()?.is_ascii_digit() {
        return None;
    }
    if let Some(ts) = parse_iso(bytes) {
        return Some(ts);
    }
    if let Some(ts) = parse_dotted(bytes) {
        return Some(ts);
    }

    // US format has variable width; take up to the third space-separated token
    let mut end = 0;
    let mut spaces = 0;
    for (i, c) in line.char_indices() {
        if c == ' ' {
            spaces += 1;
            if spaces == 3 {
                end = i;
                break;
            }
        }
    }
    if end > 0 {
        if let Ok(ts) = NaiveDateTime::parse_from_str(&line[..end], "%m/%d/%Y %I:%M:%S %p") {
            return Some(ts);
        }
    }
    None
}

/// `yyyy-mm-ddThh:mm:ss` with an optional fractional part of any width (IIS/W3C
/// logs write 7 digits, i.e. 100ns ticks) and an optional trailing `Z` or
/// numeric offset, which we ignore. Parsed by hand for the same reason as
/// `parse_dotted`: it runs on every line, and chrono's format machinery is
/// heavier than the match test the line is being read for.
fn parse_iso(bytes: &[u8]) -> Option<NaiveDateTime> {
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || (bytes[10] != b'T' && bytes[10] != b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year = digits(bytes, 0, 4)?;
    let month = digits(bytes, 5, 2)?;
    let day = digits(bytes, 8, 2)?;
    let hour = digits(bytes, 11, 2)?;
    let minute = digits(bytes, 14, 2)?;
    let second = digits(bytes, 17, 2)?;

    // Optional `.fffffff` fractional seconds of any width; scaled to nanoseconds.
    let mut nano = 0;
    if bytes.get(19) == Some(&b'.') {
        let mut scale = 100_000_000; // first fractional digit is tenths of a second
        let mut i = 20;
        while let Some(&byte) = bytes.get(i) {
            if !byte.is_ascii_digit() {
                break;
            }
            if scale > 0 {
                nano += u32::from(byte - b'0') * scale;
                scale /= 10;
            }
            i += 1;
        }
    }

    NaiveDate::from_ymd_opt(year as i32, month, day)?
        .and_hms_nano_opt(hour, minute, second, nano)
}

/// `dd.mm.yyyy hh:mm:ss` with optional `.mmm` — the shape both app log families
/// write, so this runs on every line of every file. Parsed by hand: chrono's
/// format interpreter, and the String its input needed, together cost more than
/// the match test the line is being read for.
fn parse_dotted(bytes: &[u8]) -> Option<NaiveDateTime> {
    if bytes.len() < 19
        || bytes[2] != b'.'
        || bytes[5] != b'.'
        || bytes[10] != b' '
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let day = digits(bytes, 0, 2)?;
    let month = digits(bytes, 3, 2)?;
    let year = digits(bytes, 6, 4)?;
    let hour = digits(bytes, 11, 2)?;
    let minute = digits(bytes, 14, 2)?;
    let second = digits(bytes, 17, 2)?;
    // The millisecond part is optional, and a malformed one is simply not there
    let milli = match bytes.get(19) {
        Some(b'.') => digits(bytes, 20, 3).unwrap_or(0),
        _ => 0,
    };

    NaiveDate::from_ymd_opt(year as i32, month, day)?.and_hms_milli_opt(hour, minute, second, milli)
}

fn digits(bytes: &[u8], at: usize, count: usize) -> Option<u32> {
    let slice = bytes.get(at..at + count)?;
    let mut value = 0;
    for &byte in slice {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + u32::from(byte - b'0');
    }
    Some(value)
}

fn guid_login_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"ID_Login[:=]'?([0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12})").unwrap())
}

fn id_command_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"ID Command: ([0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12})").unwrap())
}

fn operation_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Handling (\w+)").unwrap())
}

fn duration_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r" (\d+(?:\.\d+)?)ms\b").unwrap())
}

/// Each regex is gated on the literal it cannot match without: the substring
/// test costs a fraction of a capture run, and this runs on every hit — an
/// unfiltered query makes a hit of every line in the range.
fn extract_fields(line: &str) -> ExtractedFields {
    let mut fields = ExtractedFields::default();
    if line.contains("ID_Login") {
        fields.id_login = guid_login_regex()
            .captures(line)
            .map(|c| c[1].to_lowercase());
    }
    if line.contains("ID Command: ") {
        fields.id_command = id_command_regex()
            .captures(line)
            .map(|c| c[1].to_lowercase());
    }
    if line.contains("Handling ") {
        fields.operation = operation_regex().captures(line).map(|c| c[1].to_string());
    }
    if line.contains("ms") {
        fields.duration_ms = duration_regex()
            .captures(line)
            .and_then(|c| c[1].parse().ok());
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text big enough to be split across cores, shaped like a real log: entries
    /// that carry a timestamp, continuation lines that inherit one, and matches
    /// scattered densely enough to land on chunk boundaries.
    fn parallel_text() -> Vec<u8> {
        let mut text = String::with_capacity(PARALLEL_MIN_BYTES * 2);
        let mut n = 0;
        while text.len() < PARALLEL_MIN_BYTES * 2 {
            n += 1;
            let (s, m, h) = (n % 60, (n / 60) % 60, (n / 24) % 24);
            text.push_str(&format!(
                "14.07.2026 {h:02}:{m:02}:{s:02}.{:03} INFO - Request starting id={n}\n",
                n % 1000
            ));
            // A hit every 17 lines, so some fall next to every chunk boundary
            if n % 17 == 0 {
                text.push_str(&format!(
                    "14.07.2026 {h:02}:{m:02}:{s:02}.{:03} ERROR - BOOM at id={n}\n",
                    n % 1000
                ));
            }
            // Continuation lines carry no timestamp of their own
            if n % 5 == 0 {
                text.push_str("    at Example.Api.Handler.Handle()\n");
            }
        }
        text.into_bytes()
    }

    fn scan_file_of(name: &str) -> ScanFile {
        ScanFile {
            service_id: "test/Svc".into(),
            path: PathBuf::from(name),
            name: name.into(),
            uncompressed_size: 0,
            filter_lines_by_date: false,
        }
    }

    fn sequential_scan(
        file: &ScanFile,
        matcher: &Matcher,
        request: &SearchRequest,
        text: &[u8],
    ) -> (Vec<SearchHit>, u64) {
        let mut scanner = Scanner::new(file, matcher, request);
        for raw in text.split_inclusive(|byte| *byte == b'\n') {
            scanner.push(&String::from_utf8_lossy(raw));
        }
        scanner.finish()
    }

    /// The parallel scan splits the text at line boundaries and stitches the
    /// results back together. It must be indistinguishable from one pass down
    /// the file — same hits, same line numbers, same context, same timestamps.
    #[test]
    fn a_parallel_scan_is_identical_to_a_sequential_one() {
        let text = parallel_text();
        assert!(
            split_at_lines(&text, 8).len() > 1,
            "the text must actually be split for this to test anything"
        );

        let file = scan_file_of("log-2026-07-14.log");
        let matcher = Matcher::new("BOOM", false, true).unwrap();
        let request = SearchRequest {
            service_ids: vec![file.service_id.clone()],
            date_from: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
            date_to: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
            query: "BOOM".into(),
            is_regex: false,
            case_sensitive: true,
            context_lines: 2,
        };

        let (expected, expected_lines) = sequential_scan(&file, &matcher, &request, &text);
        let (actual, actual_lines) = scan_memory(&file, &matcher, &request, &text);

        assert_eq!(actual_lines, expected_lines, "line count");
        assert_eq!(actual.len(), expected.len(), "hit count");
        assert!(expected.len() > 100, "a meaningful number of hits");

        for (actual, expected) in actual.iter().zip(&expected) {
            assert_eq!(actual.line_number, expected.line_number);
            assert_eq!(actual.line, expected.line);
            assert_eq!(actual.timestamp, expected.timestamp);
            assert_eq!(actual.context_before, expected.context_before);
            assert_eq!(actual.context_after, expected.context_after);
        }
    }

    /// A chunk can open in the middle of a run of continuation lines, whose
    /// timestamp belongs to an entry that started in the chunk before it.
    #[test]
    fn a_chunk_inherits_the_timestamp_of_the_entry_it_opens_inside() {
        let text = b"14.07.2026 09:00:00.100 INFO - exec SF_X @Body='<html>\n<body>\n</body>\nplain\n";
        // Offsets of the lines after the timestamped one
        let at_body = text.iter().position(|b| *b == b'\n').unwrap() + 1;
        let ts = timestamp_before(text, at_body).unwrap();
        assert_eq!(ts.to_string(), "2026-07-14 09:00:00.100");

        // Still inherited several continuation lines later
        let at_plain = text.len() - "plain\n".len();
        assert_eq!(timestamp_before(text, at_plain), Some(ts));

        // Nothing precedes the first line
        assert!(timestamp_before(text, 0).is_none());
    }

    #[test]
    fn context_before_a_chunk_comes_from_the_lines_that_precede_it() {
        let text = b"one\ntwo\nthree\nfour\n";
        let at_four = text.len() - "four\n".len();
        assert_eq!(
            context_before(text, at_four, 2),
            VecDeque::from(vec!["two".to_string(), "three".to_string()])
        );
        // Fewer lines available than asked for, and none at the very start
        assert_eq!(context_before(text, 4, 2), VecDeque::from(vec!["one".to_string()]));
        assert!(context_before(text, 0, 2).is_empty());
    }

    #[test]
    fn chunks_start_on_line_boundaries_and_cover_the_text_exactly() {
        let text = parallel_text();
        let chunks = split_at_lines(&text, 8);

        assert_eq!(chunks[0].start, 0);
        assert_eq!(chunks.last().unwrap().end, text.len());
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "no gap, no overlap");
        }
        for chunk in chunks.iter().skip(1) {
            assert_eq!(text[chunk.start - 1], b'\n', "a chunk opens a fresh line");
        }
        // Text too small to be worth splitting stays whole
        assert_eq!(split_at_lines(b"one\ntwo\n", 8), vec![0..8]);
    }

    #[test]
    fn parses_core_timestamp() {
        let ts = parse_timestamp(
            "13.07.2026 00:00:02.611 INFO - Request starting HTTP/1.1 GET http://x - - -",
        )
        .unwrap();
        assert_eq!(ts.to_string(), "2026-07-13 00:00:02.611");
    }

    #[test]
    fn parses_framework_timestamp() {
        let ts = parse_timestamp("14.07.2026 01:16:45 debug (sql) - exec SF_X @ID='Y'").unwrap();
        assert_eq!(ts.to_string(), "2026-07-14 01:16:45");
    }

    #[test]
    fn parses_us_timestamp() {
        let ts = parse_timestamp("7/14/2026 1:15:55 AM debug (other) - SET modified date").unwrap();
        assert_eq!(ts.to_string(), "2026-07-14 01:15:55");
    }

    #[test]
    fn parses_iso_timestamp_with_100ns_ticks() {
        let ts = parse_timestamp(
            "2026-07-15T08:08:09.9533870Z 169.254.131.1 - - [15/Jul/2026] \"GET / HTTP/1.1\"",
        )
        .unwrap();
        assert_eq!(ts.to_string(), "2026-07-15 08:08:09.953387");
    }

    #[test]
    fn parses_iso_timestamp_without_fraction() {
        let ts = parse_timestamp("2026-07-15T08:08:09Z rest of the line").unwrap();
        assert_eq!(ts.to_string(), "2026-07-15 08:08:09");
    }

    #[test]
    fn ignores_lines_without_timestamp() {
        assert!(parse_timestamp("SnapshotHelper::RestoreSnapshotInternal SUCCESS").is_none());
    }

    #[test]
    fn extracts_mediatr_fields() {
        let line = "13.07.2026 09:15:23.863 INFO - (ID Command: 7391c08f-1111-2222-3333-444455556666) Handling UpdateReadDetailPostCommand ID_Login:3306928E-aaaa-bbbb-cccc-ddddeeeeffff ID:1682888";
        let fields = extract_fields(line);
        assert_eq!(
            fields.id_command.as_deref(),
            Some("7391c08f-1111-2222-3333-444455556666")
        );
        assert_eq!(
            fields.id_login.as_deref(),
            Some("3306928e-aaaa-bbbb-cccc-ddddeeeeffff")
        );
        assert_eq!(fields.operation.as_deref(), Some("UpdateReadDetailPostCommand"));
    }

    #[test]
    fn extracts_duration() {
        let line = "13.07.2026 00:00:02.804 INFO - Request finished HTTP/1.1 GET http://x - 200 - text/plain 192.9070ms";
        assert_eq!(extract_fields(line).duration_ms, Some(192.907));
    }

    #[test]
    fn matcher_reports_which_named_groups_matched() {
        let matcher = Matcher::new(r"(?<err>ERROR)|(?<warn>WARN)", true, true).unwrap();
        assert_eq!(matcher.group_names(), ["err", "warn"]);

        assert_eq!(matcher.match_line("an ERROR happened"), Some(vec!["err".into()]));
        assert_eq!(
            matcher.match_line("WARN then ERROR"),
            Some(vec!["err".into(), "warn".into()]),
            "groups anywhere in the line count, in pattern order"
        );
        assert_eq!(matcher.match_line("all quiet"), None);
    }

    #[test]
    fn matcher_without_groups_matches_like_before() {
        let matcher = Matcher::new("error", false, false).unwrap();
        assert!(matcher.group_names().is_empty());
        assert_eq!(matcher.match_line("An Error occurred"), Some(Vec::new()));
        assert_eq!(matcher.match_line("fine"), None);
    }

    #[test]
    fn lookahead_pattern_falls_back_to_the_fancy_engine() {
        // The builder's ALL mode compiles to lookaheads, which the fast engine rejects
        let matcher = Matcher::new(r"^(?=.*(?<a>foo))(?=.*(?<b>bar))", true, false).unwrap();
        assert_eq!(matcher.group_names(), ["a", "b"]);

        assert!(matcher.matches("bar and Foo"));
        assert!(!matcher.matches("only foo"));
        assert_eq!(
            matcher.match_line("bar before foo"),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(matcher.match_line("only bar"), None);
    }

    #[test]
    fn any_with_not_captures_every_group_that_occurs() {
        // The builder's ANY + NOT form: each positive in a lookahead with an
        // empty alternative — it always succeeds, but captures when present —
        // behind an "at least one occurs" assertion
        let pattern = r"^(?=.*(?:ERROR|WARN))(?!.*noise)(?=.*(?<err>ERROR)|)(?=.*(?<warn>WARN)|)";
        let matcher = Matcher::new(pattern, true, true).unwrap();

        assert_eq!(
            matcher.match_line("WARN then ERROR"),
            Some(vec!["err".into(), "warn".into()])
        );
        assert_eq!(matcher.match_line("just a WARN"), Some(vec!["warn".into()]));
        assert_eq!(matcher.match_line("noise WARN"), None);
        assert_eq!(matcher.match_line("nothing"), None);
    }

    #[test]
    fn negative_lookahead_excludes_lines() {
        let matcher = Matcher::new(r"^(?!.*noise).*(?<hit>ERROR)", true, true).unwrap();
        assert_eq!(matcher.match_line("an ERROR"), Some(vec!["hit".into()]));
        assert_eq!(matcher.match_line("noise ERROR"), None);
    }

    #[test]
    fn invalid_pattern_reports_an_error_from_the_richer_engine() {
        assert!(Matcher::new(r"(?<broken", true, true).is_err());
    }
}

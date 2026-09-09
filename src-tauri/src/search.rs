use crate::cache;
use crate::config::ServiceConfig;
use crate::fields::{extract_into, FieldColumns, FieldMeta, FieldRow};
use crate::memcache::MemCache;
use crate::plugin::{BuiltinTimestamp, CompiledField, CompiledTimestamp, Registry};
use chrono::{NaiveDate, NaiveDateTime};
use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// The error a cancelled search resolves to. The command layer and the UI
/// match on it to tell a cancel apart from a real failure.
pub const CANCELLED: &str = "Search cancelled";

/// How many lines a scan loop processes between looks at the cancel flag and
/// the hit cap. An atomic load per this many lines costs nothing measurable.
const CANCEL_CHECK_LINES: u64 = 8_192;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub service_ids: Vec<String>,
    pub date_from: NaiveDate,
    pub date_to: NaiveDate,
    /// Optional absolute UTC window refining the date range: only entries whose
    /// service-clock timestamp resolves between these instants are kept. Either
    /// end may be open.
    #[serde(default)]
    pub time_from: Option<NaiveDateTime>,
    #[serde(default)]
    pub time_to: Option<NaiveDateTime>,
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    #[serde(default = "default_context")]
    pub context_lines: usize,
}

fn default_context() -> usize {
    2
}

impl SearchRequest {
    fn has_time_window(&self) -> bool {
        self.time_from.is_some() || self.time_to.is_some()
    }

    /// Whether an entry timestamp falls inside the optional UTC time window.
    /// Log timestamps carry no zone, so their service's detected clock offset
    /// is removed before comparing them with the picker boundaries.
    fn in_time_window(&self, ts: NaiveDateTime, log_offset_minutes: Option<i32>) -> bool {
        let utc = log_offset_minutes.map_or(ts, |offset| {
            ts - chrono::Duration::minutes(i64::from(offset))
        });
        self.time_from.is_none_or(|from| utc >= from)
            && self.time_to.is_none_or(|to| utc <= to)
    }
}

/// One hit as the UI receives it, line text and context included. Only pages
/// of these ever exist - the stored result holds `CompactHit`s and the text is
/// re-read from the cached files when a page is served.
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

/// The fields a hit carries, keyed by the column key (`<packId>.<field>`) the
/// pack declared. Only values that were actually extracted are present, and the
/// column list a result reports (`SearchMeta::fields`) is what labels them.
///
/// Never attribute a field by line proximity - concurrent services interleave
/// their lines, so only values read off the entry's own lines belong to it.
pub type ExtractedFields = HashMap<String, String>;

/// One stored hit, holding no line text - around a hundred bytes instead of
/// the kilobytes a hit with its line and context strings costs. A search with
/// millions of hits stays in the hundreds of megabytes; the text is fetched
/// back from the cached files only for the page being displayed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactHit {
    /// Index into the result's file table
    pub file: u32,
    pub line_number: u64,
    pub timestamp: Option<NaiveDateTime>,
    /// Bitmask over the query's named groups (pattern order, first 32)
    pub matched_groups: u32,
    /// Row of the result's field columns holding this hit's extracted values.
    /// A row index rather than a position, so re-sorting the hits leaves the
    /// columns untouched.
    pub row: u32,
}

/// Appends a hit, folding it into the previous one when both belong to the same
/// entry (same file, same anchor line): group masks union and the rows merge,
/// with the earlier line's values winning.
fn push_or_merge(hits: &mut Vec<CompactHit>, hit: CompactHit, columns: &mut FieldColumns) {
    match hits.last_mut() {
        Some(last) if last.file == hit.file && last.line_number == hit.line_number => {
            last.matched_groups |= hit.matched_groups;
            columns.merge_rows(last.row, hit.row);
        }
        _ => hits.push(hit),
    }
}

/// The group-name bit of a mask, back as names in pattern order.
pub fn mask_to_groups(group_names: &[String], mask: u32) -> Vec<String> {
    group_names
        .iter()
        .take(32)
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, name)| name.clone())
        .collect()
}

pub fn group_mask(group_names: &[String], matched: &[String]) -> u32 {
    let mut mask = 0;
    for name in matched {
        if let Some(pos) = group_names.iter().position(|g| g == name) {
            if pos < 32 {
                mask |= 1 << pos;
            }
        }
    }
    mask
}

#[cfg(test)]
/// GUID text (hyphenated hex, any case) into 16 bytes; `None` when malformed.
fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let mut bytes = [0u8; 16];
    let mut at = 0;
    let mut high: Option<u8> = None;
    for ch in text.bytes() {
        if ch == b'-' {
            continue;
        }
        let digit = (ch as char).to_digit(16)? as u8;
        match high {
            None => high = Some(digit),
            Some(h) => {
                if at >= 16 {
                    return None;
                }
                bytes[at] = (h << 4) | digit;
                at += 1;
                high = None;
            }
        }
    }
    (at == 16 && high.is_none()).then_some(bytes)
}

#[cfg(test)]
/// The canonical 8-4-4-4-12 lowercase form.
fn format_guid(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
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

/// Full result in compact form. The command layer keeps it in state; pages are
/// hydrated with text on demand, so neither RAM nor IPC ever holds it all.
pub struct SearchResult {
    /// The files the hits point into, indexed by `CompactHit::file`
    pub files: Vec<ScanFile>,
    pub hits: Vec<CompactHit>,
    /// Extracted values, one column per field, indexed by `CompactHit::row`
    pub columns: FieldColumns,
    /// What those columns are, in column order
    pub field_meta: Vec<FieldMeta>,
    /// Named capture groups of the query, in pattern order
    pub group_names: Vec<String>,
    pub files_scanned: usize,
    pub lines_scanned: u64,
    pub duration_ms: u64,
    /// The configured hit cap cut the result short
    pub truncated: bool,
}

/// Shared by the cross-service search and the raw viewer's in-file search.
/// Patterns the fast `regex` engine rejects (look-around, as compiled from the
/// query builder's ALL/NOT criteria) fall back to the backtracking engine.
pub struct Matcher {
    engine: Engine,
    /// The query's leading negative look-aheads, lifted out of `engine` and run
    /// on their own (see `split_leading_nots`). A hit is an entry, not a line,
    /// so an exclusion has to veto every line of the entry - left inside the
    /// pattern it would only reject the one line the excluded term sits on,
    /// and the entry would still come back through its stack trace.
    exclude: Option<Engine>,
    /// Named capture groups of the pattern, in definition order. Hits record
    /// which of them matched, so the UI can colour lines per group and charts
    /// can draw one series per group.
    group_names: Vec<String>,
}

enum Engine {
    /// Empty query - every line in the selected services and date range matches
    All,
    Substring(String),
    Pattern(Regex),
    /// Look-around pattern the fast engine cannot run
    Fancy(fancy_regex::Regex),
}

impl Engine {
    fn is_match(&self, line: &str) -> bool {
        match self {
            Engine::All => true,
            Engine::Substring(needle) => line.contains(needle.as_str()),
            Engine::Pattern(regex) => regex.is_match(line),
            Engine::Fancy(regex) => regex.is_match(line).unwrap_or(false),
        }
    }

    fn group_names(&self) -> Vec<String> {
        match self {
            Engine::Pattern(regex) => regex.capture_names().flatten().map(str::to_string).collect(),
            Engine::Fancy(regex) => regex.capture_names().flatten().map(str::to_string).collect(),
            _ => Vec::new(),
        }
    }
}

/// Compiles one pattern, falling back to the backtracking engine for the
/// look-around the fast one refuses.
fn compile_engine(pattern: &str, case_sensitive: bool) -> Result<Engine, String> {
    let pattern = if case_sensitive {
        pattern.to_string()
    } else {
        format!("(?i){pattern}")
    };
    match Regex::new(&pattern) {
        Ok(regex) => Ok(Engine::Pattern(regex)),
        Err(_) => match fancy_regex::Regex::new(&pattern) {
            Ok(regex) => Ok(Engine::Fancy(regex)),
            // The fancy engine accepts a superset of the syntax, so when both
            // refuse the pattern its error names the real problem
            Err(e) => Err(format!("Invalid regex: {e}")),
        },
    }
}

/// Splits `^(?!a)(?!b)rest` into `("^rest", "(?:a)|(?:b)")`: the run of negative
/// look-aheads a query opens with, which are exclusions over the whole entry
/// rather than over the line they are tested on, and the pattern left without
/// them. `None` when the pattern does not open that way, so an ordinary query
/// compiles exactly as before.
fn split_leading_nots(pattern: &str) -> Option<(String, String)> {
    let rest = pattern.strip_prefix('^')?;
    let mut at = 0;
    let mut nots: Vec<String> = Vec::new();
    while let Some(body) = rest[at..].strip_prefix("(?!") {
        let end = closing_paren(body)?;
        nots.push(format!("(?:{})", &body[..end]));
        at += "(?!".len() + end + 1;
    }
    if nots.is_empty() {
        return None;
    }
    Some((format!("^{}", &rest[at..]), nots.join("|")))
}

/// Offset of the `)` closing the group whose body `text` starts, honouring
/// escapes, nested groups and character classes (where a `)` is a literal).
fn closing_paren(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_class = false;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 1,
            b'[' if !in_class => in_class = true,
            b']' if in_class => in_class = false,
            b'(' if !in_class => depth += 1,
            b')' if !in_class => {
                if depth == 0 {
                    return Some(at);
                }
                depth -= 1;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// Lines can hold megabytes of SQL or HTML; the per-group scan stops after this
/// many matches instead of walking all of them.
const GROUP_SCAN_CAP: usize = 200;

impl Matcher {
    pub fn new(query: &str, is_regex: bool, case_sensitive: bool) -> Result<Self, String> {
        if query.is_empty() {
            return Ok(Matcher {
                engine: Engine::All,
                exclude: None,
                group_names: Vec::new(),
            });
        }
        if !is_regex && case_sensitive {
            return Ok(Matcher {
                engine: Engine::Substring(query.to_string()),
                exclude: None,
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
        // The query builder puts its NOT criteria first for exactly this; a
        // hand-written pattern that opens the same way gets the same treatment,
        // which is the only reading that makes sense when a row is an entry.
        let (pattern, excluded) = match split_leading_nots(&pattern) {
            Some((rest, nots)) => match compile_engine(&nots, case_sensitive) {
                Ok(engine) => (rest, Some(engine)),
                // Unsplittable after all - run the pattern as the user wrote it
                Err(_) => (pattern, None),
            },
            None => (pattern, None),
        };
        let engine = compile_engine(&pattern, case_sensitive)?;
        Ok(Matcher {
            group_names: engine.group_names(),
            engine,
            exclude: excluded,
        })
    }

    fn build(request: &SearchRequest) -> Result<Self, String> {
        Matcher::new(&request.query, request.is_regex, request.case_sensitive)
    }

    pub fn group_names(&self) -> &[String] {
        &self.group_names
    }

    /// Whether the query's exclusions reject this line. The scanner widens that
    /// to the whole entry the line belongs to; line-at-a-time callers (the raw
    /// viewer's in-file search) get the per-line reading `matches` always had.
    pub fn excludes(&self, line: &str) -> bool {
        self.exclude.as_ref().is_some_and(|engine| engine.is_match(line))
    }

    pub fn matches(&self, line: &str) -> bool {
        !self.excludes(line) && self.engine.is_match(line)
    }

    /// Whether a whole entry matches - the header line plus every continuation
    /// line, newline-joined, as a stored hit reads back. Each line is tested on
    /// its own (`^` anchors per line and `.` stops at a newline, so testing the
    /// join would miss every match on a stack-trace or SQL-body line), and an
    /// exclusion anywhere in the entry vetoes the whole of it - exactly the
    /// reading the scan gives an entry it builds from the files.
    pub fn matches_entry(&self, entry: &str) -> bool {
        let mut matched = false;
        for line in entry.lines() {
            if self.excludes(line) {
                return false;
            }
            matched = matched || self.engine.is_match(line);
        }
        // An entry of no lines at all still answers to the empty query
        matched || (entry.is_empty() && self.engine.is_match(""))
    }

    /// Match test that also reports which named groups took part anywhere in
    /// the line; `None` is no match. Patterns without named groups skip the
    /// capture cost entirely and behave exactly like `matches`.
    pub fn match_line(&self, line: &str) -> Option<Vec<String>> {
        if self.excludes(line) {
            return None;
        }
        self.match_line_included(line)
    }

    /// `match_line` for a line the caller has already cleared of exclusions.
    fn match_line_included(&self, line: &str) -> Option<Vec<String>> {
        if self.group_names.is_empty() {
            return self.engine.is_match(line).then(Vec::new);
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
            _ => return self.engine.is_match(line).then(Vec::new),
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

    /// The part of the line the caller wants isolated from the rest: every
    /// named capture group that took part in the match, space-joined in
    /// pattern order (the builder makes each criterion one, so a multi-term
    /// query yields each term's value), or the whole matched text when the
    /// pattern has no named groups. Unnamed `(...)` groups are plain grouping
    /// here, exactly as in highlighting - only `(?<x>...)` isolates. `None`
    /// when the line does not match. Empty and substring queries carry nothing
    /// to extract.
    pub fn extract(&self, line: &str) -> Option<String> {
        self.extract_excluding(line, &[])
    }

    /// As `extract`, but leaves out the named groups in `exclude` (those the user
    /// toggled off in the results legend), mirroring the frontend's buildExtractor.
    pub fn extract_excluding(&self, line: &str, exclude: &[String]) -> Option<String> {
        // A hit's line is a whole entry - its header line plus every
        // continuation line, newline-joined - and the scanner matched each of
        // those on its own. Isolating over the join would miss every match on a
        // stack-trace or SQL-body line, since `^` anchors at the entry's start
        // and `.` stops at the first newline; the entry would then export as
        // nothing at all. First line that yields a value wins, so a row still
        // isolates one value.
        if line.contains('\n') {
            return line.lines().find_map(|own| self.extract_one(own, exclude));
        }
        self.extract_one(line, exclude)
    }

    fn extract_one(&self, line: &str, exclude: &[String]) -> Option<String> {
        match &self.engine {
            Engine::All | Engine::Substring(_) => None,
            Engine::Pattern(regex) => regex
                .captures(line)
                .map(|caps| named_groups(&self.group_names, &caps, exclude)),
            Engine::Fancy(regex) => match regex.captures(line) {
                Ok(Some(caps)) => Some(fancy_named_groups(&self.group_names, &caps, exclude)),
                _ => None,
            },
        }
    }
}

/// Every participating, non-empty named group of a match, space-joined in
/// pattern order, falling back to the whole match. The builder's ALL form
/// matches zero-width at the start (group 0 is empty) and captures inside
/// look-aheads, so its values live in the named groups - the fallback only
/// fires for patterns without them.
fn named_groups(names: &[String], caps: &regex::Captures, exclude: &[String]) -> String {
    let joined = names
        .iter()
        .filter(|name| !exclude.iter().any(|e| e == *name))
        .filter_map(|name| caps.name(name))
        .map(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        return caps.get(0).map_or(String::new(), |m| m.as_str().to_string());
    }
    joined
}

fn fancy_named_groups(names: &[String], caps: &fancy_regex::Captures, exclude: &[String]) -> String {
    let joined = names
        .iter()
        .filter(|name| !exclude.iter().any(|e| e == *name))
        .filter_map(|name| caps.name(name))
        .map(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if joined.is_empty() {
        return caps.get(0).map_or(String::new(), |m| m.as_str().to_string());
    }
    joined
}

/// How one pack's lines parse: the timestamp formats to try, and which of the
/// result's columns its fields fill.
#[derive(Debug, Default)]
pub struct PackScanSpec {
    /// The result's whole column layout, shared by every pack's spec. Scan units
    /// all write rows of this width, which is what lets their columns be
    /// concatenated by offsetting row indices and nothing else.
    pub columns: Arc<Vec<FieldMeta>>,
    /// Tried in order; empty means every built-in format, which is what a pack
    /// that does not care about timestamps should get
    pub timestamps: Vec<CompiledTimestamp>,
    /// `(column, field)` per declared field
    pub fields: Vec<(usize, CompiledField)>,
}

/// The parsing rules a search runs with. Columns are the union of every loaded
/// pack's fields in registry order, so one result has one column layout however
/// many packs its services span.
pub struct SearchSpec {
    pub columns: Vec<FieldMeta>,
    packs: HashMap<String, Arc<PackScanSpec>>,
    /// Used for a service whose pack is not loaded: its lines still search, they
    /// just carry no extracted fields
    fallback: Arc<PackScanSpec>,
}

impl SearchSpec {
    pub fn from_registry(registry: &Registry) -> Self {
        // The layout has to be known before any pack's spec can point into it,
        // so the columns are collected first and the specs built against them
        let columns: Arc<Vec<FieldMeta>> = Arc::new(
            registry
                .fields()
                .into_iter()
                .map(FieldMeta::of)
                .collect(),
        );
        let mut packs = HashMap::new();
        let mut next = 0usize;
        for pack in &registry.packs {
            let fields = pack
                .fields
                .iter()
                .map(|field| {
                    let column = next;
                    next += 1;
                    (column, field.clone())
                })
                .collect::<Vec<(usize, CompiledField)>>();
            packs.insert(
                pack.manifest.id.clone(),
                Arc::new(PackScanSpec {
                    columns: Arc::clone(&columns),
                    timestamps: pack.timestamps.clone(),
                    fields,
                }),
            );
        }
        SearchSpec {
            fallback: Arc::new(PackScanSpec {
                columns: Arc::clone(&columns),
                ..PackScanSpec::default()
            }),
            columns: Vec::clone(&columns),
            packs,
        }
    }

    /// An empty spec for callers with no packs in play (the raw file viewer's
    /// in-file search, tests).
    pub fn empty() -> Self {
        SearchSpec {
            columns: Vec::new(),
            packs: HashMap::new(),
            fallback: Arc::new(PackScanSpec::default()),
        }
    }

    pub fn for_pack(&self, pack_id: &str) -> Arc<PackScanSpec> {
        self.packs
            .get(pack_id)
            .cloned()
            .unwrap_or_else(|| Arc::clone(&self.fallback))
    }
}

/// One cached file a search covers. Kept with the stored result so hits can be
/// hydrated with text later.
#[derive(Clone)]
pub struct ScanFile {
    pub service_id: String,
    pub path: PathBuf,
    pub name: String,
    /// Size of the original log, from the manifest. The memory cache budgets the
    /// decompressed copy against it before it decompresses anything.
    pub uncompressed_size: u64,
    /// Minutes this service's naive log clock runs ahead of UTC. Unknown clocks
    /// retain their raw comparison behavior.
    pub log_offset_minutes: Option<i32>,
    /// Undated growing files (Log.txt) need per-line date filtering
    pub filter_lines_by_date: bool,
    /// Day the file covers, when the naming carries one. Chains a file to the
    /// previous day's file of the same service and instance, so an entry split
    /// by rotation can inherit its header timestamp across the seam.
    pub date: Option<chrono::NaiveDate>,
    /// Instance id from the file name (docker logs); files of different
    /// instances are separate streams and never chain to each other.
    pub instance: Option<String>,
    /// How this file's lines parse, from the pack that owns its service
    pub spec: Arc<PackScanSpec>,
}

impl ScanFile {
    /// The column layout a scan of this file writes into: the result's whole
    /// set. A file only ever fills its own pack's columns and leaves the rest
    /// absent, which is what a service having no such field means anyway.
    fn column_meta(&self) -> &[FieldMeta] {
        &self.spec.columns
    }
}

/// Runs a search across the cached files of the selected services.
/// CPU-bound; call via spawn_blocking. Files scan in parallel (rayon),
/// results merge into one timeline sorted by timestamp.
/// `on_progress` is called from worker threads as files finish.
///
/// Each file is read through `cache`, which serves it from RAM when it is held
/// there and streams it from the zstd file on disk when it is not.
#[allow(clippy::too_many_arguments)]
pub fn run(
    request: &SearchRequest,
    services: &[ServiceConfig],
    spec: &SearchSpec,
    cache: &MemCache,
    cancel: &AtomicBool,
    max_hits: usize,
    on_progress: impl Fn(SearchProgress) + Sync,
) -> Result<SearchResult, String> {
    let started = std::time::Instant::now();
    let matcher = Matcher::build(request)?;
    let files = collect_files(request, services, spec)?;
    let files_scanned = files.len();

    cache.begin_pass();

    let files_done = AtomicUsize::new(0);
    let hits_found = AtomicUsize::new(0);
    let lines_seen = AtomicU64::new(0);
    // Trips when the cap is reached; scans still running stop finding more
    let capped = AtomicBool::new(false);
    on_progress(SearchProgress {
        phase: "scanning".into(),
        files_done: 0,
        files_total: files_scanned,
        hits: 0,
        lines_scanned: 0,
        current_file: String::new(),
    });

    let per_file: Vec<Result<FileScan, String>> = files
        .par_iter()
        .enumerate()
        .map(|(index, file)| {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            if capped.load(Ordering::Relaxed) {
                files_done.fetch_add(1, Ordering::Relaxed);
                return Ok(FileScan {
                    head: None,
                    hits: Vec::new(),
                    columns: FieldColumns::new(file.column_meta()),
                    lines: 0,
                    first_header_ts: None,
                    last_ts: None,
                });
            }
            let result = scan_file(file, &matcher, request, cache, cancel, &capped, max_hits);
            if let Ok(scan) = &result {
                let done = files_done.fetch_add(1, Ordering::Relaxed) + 1;
                let hits = hits_found.fetch_add(scan.hits.len(), Ordering::Relaxed) + scan.hits.len();
                if hits >= max_hits {
                    capped.store(true, Ordering::Relaxed);
                }
                let lines = lines_seen.fetch_add(scan.lines, Ordering::Relaxed) + scan.lines;
                on_progress(SearchProgress {
                    phase: "scanning".into(),
                    files_done: done,
                    files_total: files_scanned,
                    hits,
                    lines_scanned: lines,
                    current_file: file.name.clone(),
                });
            }
            result.map(|mut scan| {
                for hit in &mut scan.hits {
                    hit.file = index as u32;
                }
                if let Some(head) = &mut scan.head {
                    head.file = index as u32;
                }
                scan
            })
        })
        .collect();

    let mut scans = Vec::with_capacity(per_file.len());
    for result in per_file {
        scans.push(result?);
    }
    attach_heads(&files, &mut scans, request);

    // An unfiltered query makes a hit of every line, so the merged vector can
    // reach millions of entries. Sizing it once beats growing it by doubling.
    let total: usize = scans.iter().map(|scan| scan.hits.len()).sum();
    let mut hits = Vec::with_capacity(total.min(max_hits));
    let mut columns = FieldColumns::new(&spec.columns);
    let mut lines_scanned = 0;
    for scan in scans {
        // Every file's rows are concatenated whether or not the cap keeps all of
        // its hits: rows past the cap are simply never referenced, and one file's
        // worth of unreferenced rows is not worth a second pass to reclaim.
        let row_offset = columns.append(scan.columns);
        let room = max_hits.saturating_sub(hits.len());
        hits.extend(scan.hits.into_iter().take(room).map(|mut hit| {
            hit.row += row_offset;
            hit
        }));
        lines_scanned += scan.lines;
    }
    let truncated = capped.load(Ordering::Relaxed) || total > max_hits;

    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELLED.into());
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

    // Merged multi-service timeline, newest first by default. Sort ascending
    // (undatable lines last) then reverse, so the initial ordering matches what
    // a descending time re-sort produces in `sort_search_hits`.
    hits.sort_by(|a, b| match (a.timestamp, b.timestamp) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    hits.reverse();

    Ok(SearchResult {
        files,
        hits,
        columns,
        field_meta: spec.columns.clone(),
        group_names: matcher.group_names().to_vec(),
        files_scanned,
        lines_scanned,
        duration_ms: started.elapsed().as_millis() as u64,
        truncated,
    })
}

fn collect_files(
    request: &SearchRequest,
    services: &[ServiceConfig],
    spec: &SearchSpec,
) -> Result<Vec<ScanFile>, String> {
    let mut files = Vec::new();
    for id in &request.service_ids {
        let service = services
            .iter()
            .find(|s| &s.id == id)
            .ok_or_else(|| format!("Unknown service: {id}"))?;
        let dir = cache::service_dir_path(&service.environment, &service.name)?;
        let manifest = cache::load_manifest(&dir);
        let log_offset_minutes = cache::detect_offset_minutes(&manifest);
        // File names follow the service's log clock, while the picker sends UTC.
        // Shift each end back onto that clock before pruning dated files.
        let log_time = |utc: NaiveDateTime| {
            log_offset_minutes.map_or(utc, |offset| {
                utc + chrono::Duration::minutes(i64::from(offset))
            })
        };
        let date_from = request
            .time_from
            .map_or(request.date_from, |time| log_time(time).date());
        let date_to = request
            .time_to
            .map_or(request.date_to, |time| log_time(time).date());
        let pack_spec = spec.for_pack(&service.pack_id);
        for entry in manifest.entries.values() {
            let in_range = match entry.date {
                Some(date) => date >= date_from && date <= date_to,
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
                log_offset_minutes,
                filter_lines_by_date: entry.date.is_none(),
                date: entry.date,
                instance: entry.instance.clone(),
                spec: Arc::clone(&pack_spec),
            });
        }
    }
    Ok(files)
}

/// Dates each file's head block - leading continuation lines that belong to an
/// entry cut in half by log rotation - and folds it into the file's hits.
///
/// The timestamp comes from the previous file of the same service and instance
/// (its last entry header, which is the very entry the block continues). The
/// previous file is the one whose last header falls latest at or before our
/// own first header - dates alone cannot pick it, because hourly rotation
/// (log-*_HH.log) and docker .N rolls split a single day across files. When
/// no such file is in the scan, the file's own first header minus a millisecond
/// stands in, so the block still sorts just before the entries that follow it.
/// A head that resolves to no timestamp at all keeps none - as before - and on
/// per-line date-filtered files it is kept only when its resolved day is in
/// range.
fn attach_heads(files: &[ScanFile], scans: &mut [FileScan], request: &SearchRequest) {
    for at in 0..scans.len() {
        let Some(mut head) = scans[at].head.take() else {
            continue;
        };
        let file = &files[at];
        let own_first = scans[at].first_header_ts;
        let candidates = files
            .iter()
            .zip(scans.iter())
            .enumerate()
            .filter(|&(i, (other, _))| {
                i != at
                    && other.service_id == file.service_id
                    && other.instance == file.instance
            });
        let previous_ts = if let Some(first) = own_first {
            candidates
                .filter_map(|(_, (_, scan))| scan.last_ts)
                .filter(|ts| *ts <= first)
                .max()
        } else {
            // Nothing of our own to anchor on - fall back to the newest
            // earlier-dated file; same-day rotations tie on date and the one
            // that knows its last header wins the tie
            candidates
                .filter(|(_, (other, _))| match (other.date, file.date) {
                    (Some(theirs), Some(ours)) => theirs < ours,
                    _ => false,
                })
                .max_by_key(|&(_, (other, scan))| (other.date, scan.last_ts))
                .and_then(|(_, (_, scan))| scan.last_ts)
        };
        head.timestamp = previous_ts.or_else(|| {
            scans[at]
                .first_header_ts
                .map(|ts| ts - chrono::Duration::milliseconds(1))
        });
        if file.filter_lines_by_date && !request.has_time_window() {
            let in_range = head.timestamp.is_some_and(|ts| {
                ts.date() >= request.date_from && ts.date() <= request.date_to
            });
            if !in_range {
                continue;
            }
        }
        // A time window needs a resolved timestamp to place the head; one that
        // resolves to none cannot be shown to fall inside it and is dropped.
        if request.has_time_window()
            && !head
                .timestamp
                .is_some_and(|ts| request.in_time_window(ts, file.log_offset_minutes))
        {
            continue;
        }
        scans[at].hits.insert(0, head);
    }
}

/// Line text fetched back for one stored hit. For a hit anchored at a
/// timestamped line, `line` holds the whole entry - the header line and every
/// continuation line up to the next timestamped one, newline-joined.
pub struct FetchedLine {
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// An entry's fetched text stops growing here. A multi-megabyte SQL body would
/// otherwise dominate a page or an export; the raw viewer has the rest.
const ENTRY_MAX_BYTES: usize = 64 * 1024;

/// What a pending fetch is still waiting for.
enum Phase {
    /// Collecting the entry's continuation lines until the next header
    Entry,
    /// The entry hit `ENTRY_MAX_BYTES`; skimming to the next header
    EntryOverflow,
    /// Collecting this many after-context lines
    After(usize),
}

struct PendingFetch {
    slot: usize,
    fetched: FetchedLine,
    phase: Phase,
}

/// Re-reads the text of stored hits from the cached files. Each file streams at
/// most once (from RAM when the memory cache holds it, from the zstd file
/// otherwise), stops as soon as its last wanted line has passed, and nothing is
/// held afterwards - memory stays flat no matter how many hits are stored.
///
/// Every wanted line anchors an entry: a timestamped line is a header and the
/// fetch spans the whole entry; an untimestamped one anchors a file's head
/// block and the fetch runs to the file's first header. Either way the
/// after-context starts at the next timestamped line.
///
/// `wanted` lists (file index, line number, slot); `deliver` receives each
/// slot's text. A line the file no longer has (re-synced shorter) is skipped
/// and its slot keeps whatever it was initialised with.
pub fn fetch_hit_lines(
    files: &[ScanFile],
    wanted: &[(u32, u64, usize)],
    context_lines: usize,
    cache: &MemCache,
    deliver: impl FnMut(usize, FetchedLine),
) -> Result<(), String> {
    fetch_hit_lines_cancellable(
        files,
        wanted,
        context_lines,
        cache,
        &AtomicBool::new(false),
        deliver,
    )
}

/// `fetch_hit_lines` for callers that can be asked to stop: re-reading every
/// hit of a large result takes long enough to deserve a cancel button, and the
/// flag is read between files and every few thousand lines.
pub fn fetch_hit_lines_cancellable(
    files: &[ScanFile],
    wanted: &[(u32, u64, usize)],
    context_lines: usize,
    cache: &MemCache,
    cancel: &AtomicBool,
    mut deliver: impl FnMut(usize, FetchedLine),
) -> Result<(), String> {
    let mut by_file: std::collections::BTreeMap<u32, Vec<(u64, usize)>> =
        std::collections::BTreeMap::new();
    for (file, line, slot) in wanted {
        by_file.entry(*file).or_default().push((*line, *slot));
    }

    for (file_index, mut lines) in by_file {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        let file = files
            .get(file_index as usize)
            .ok_or("A stored hit points past the file table")?;
        lines.sort_unstable();
        let source = cache.source(&file.path)?;
        let mut reader = source.reader()?;

        let mut ring: VecDeque<String> = VecDeque::with_capacity(context_lines + 1);
        let mut pending: Vec<PendingFetch> = Vec::new();
        let mut next = 0;
        let mut line_number: u64 = 0;
        let mut buffer = Vec::new();
        loop {
            buffer.clear();
            let read = reader
                .read_until(b'\n', &mut buffer)
                .map_err(|e| format!("Read failed in {}: {e}", file.name))?;
            if read == 0 {
                break;
            }
            line_number += 1;
            if line_number % CANCEL_CHECK_LINES == 0 && cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            let text = String::from_utf8_lossy(&buffer);
            let text = text.trim_end_matches(['\r', '\n']);
            let opens_entry = parse_timestamp(text).is_some();

            let mut at = 0;
            while at < pending.len() {
                let entry = &mut pending[at];
                match entry.phase {
                    Phase::Entry if !opens_entry => {
                        if entry.fetched.line.len() + text.len() < ENTRY_MAX_BYTES {
                            entry.fetched.line.push('\n');
                            entry.fetched.line.push_str(text);
                        } else {
                            entry.phase = Phase::EntryOverflow;
                        }
                        at += 1;
                        continue;
                    }
                    Phase::EntryOverflow if !opens_entry => {
                        at += 1;
                        continue;
                    }
                    // The next entry starts here; this line opens the after-context
                    Phase::Entry | Phase::EntryOverflow => entry.phase = Phase::After(context_lines),
                    Phase::After(_) => {}
                }
                if let Phase::After(remaining) = &mut entry.phase {
                    if *remaining > 0 {
                        entry.fetched.context_after.push(text.to_string());
                        *remaining -= 1;
                    }
                    if *remaining == 0 {
                        let done = pending.remove(at);
                        deliver(done.slot, done.fetched);
                        continue;
                    }
                }
                at += 1;
            }

            while next < lines.len() && lines[next].0 == line_number {
                let fetched = FetchedLine {
                    line: text.to_string(),
                    context_before: ring.iter().cloned().collect(),
                    context_after: Vec::new(),
                };
                pending.push(PendingFetch {
                    slot: lines[next].1,
                    fetched,
                    phase: Phase::Entry,
                });
                next += 1;
            }

            // Everything wanted has passed - no reason to read to the end
            if next >= lines.len() && pending.is_empty() {
                break;
            }

            // The ring rotates its Strings instead of allocating one per line
            if context_lines > 0 {
                let mut slot = if ring.len() == context_lines {
                    ring.pop_front().unwrap_or_default()
                } else {
                    String::new()
                };
                slot.clear();
                slot.push_str(text);
                ring.push_back(slot);
            }
        }
        // Hits near the end of the file get the shorter entry and after-context
        // that exists
        for done in pending {
            deliver(done.slot, done.fetched);
        }
    }
    Ok(())
}

/// A whole file's scan. `head` is the merged hit of the file's leading
/// continuation lines (an entry cut in half by log rotation, or junk before
/// the first header) - undated and kept separate until `attach_heads` resolves
/// its timestamp against the neighbouring files.
#[derive(Debug)]
struct FileScan {
    head: Option<CompactHit>,
    hits: Vec<CompactHit>,
    columns: FieldColumns,
    lines: u64,
    /// Timestamp of the file's first own entry header
    first_header_ts: Option<NaiveDateTime>,
    /// Timestamp of the file's last entry header
    last_ts: Option<NaiveDateTime>,
}

#[allow(clippy::too_many_arguments)]
fn scan_file(
    file: &ScanFile,
    matcher: &Matcher,
    request: &SearchRequest,
    cache: &MemCache,
    cancel: &AtomicBool,
    capped: &AtomicBool,
    max_hits: usize,
) -> Result<FileScan, String> {
    // Text in memory can be split at line boundaries and scanned on every core.
    // A zstd stream can only be read from the front, so a search over a single
    // file - the common case - would otherwise be stuck on one core.
    if let Some(text) = cache.load(&file.path, file.uncompressed_size)? {
        return scan_memory(file, matcher, request, &text, cancel, capped, max_hits);
    }

    // Too big for the budget, or the cache is off: stream it, one core
    let source = cache.source(&file.path)?;
    let mut reader = source.reader()?;
    let mut scanner = Scanner::new(file, matcher, request);
    let mut buffer = Vec::new();
    let mut lines: u64 = 0;
    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|e| format!("Read failed in {}: {e}", file.name))?;
        if read == 0 {
            break;
        }
        lines += 1;
        if lines % CANCEL_CHECK_LINES == 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELLED.into());
            }
            // The cap tripped (here or in a parallel file): partial hits are
            // fine, the result is marked truncated anyway
            if capped.load(Ordering::Relaxed) || scanner.hits.len() >= max_hits {
                break;
            }
        }
        scanner.push(&String::from_utf8_lossy(&buffer));
    }
    // A sequential scan's orphan is the file's head block
    let scan = scanner.finish();
    Ok(FileScan {
        head: scan.orphan,
        hits: scan.hits,
        columns: scan.columns,
        lines: scan.lines,
        first_header_ts: scan.first_header_ts,
        last_ts: scan.last_ts,
    })
}

/// Text below this is not worth splitting - the per-chunk setup would cost more
/// than the scan it saves.
const PARALLEL_MIN_BYTES: usize = 4 * 1024 * 1024;

/// Scans text held in memory, in parallel over line-aligned chunks.
///
/// A chunk cannot be scanned in isolation: its first lines may inherit a
/// timestamp from an entry that opened before it, read straight out of the
/// shared text.
#[allow(clippy::too_many_arguments)]
fn scan_memory(
    file: &ScanFile,
    matcher: &Matcher,
    request: &SearchRequest,
    text: &[u8],
    cancel: &AtomicBool,
    capped: &AtomicBool,
    max_hits: usize,
) -> Result<FileScan, String> {
    let chunks = split_at_lines(text, rayon::current_num_threads());

    // Each chunk numbers its lines from its own start; the offsets are only
    // known once every chunk has counted, so they are added afterwards.
    let scanned: Vec<ChunkScan> = chunks
        .par_iter()
        .map(|chunk| {
            let mut scanner = Scanner::resume(
                file,
                matcher,
                request,
                timestamp_before(text, chunk.start),
            );
            let mut lines: u64 = 0;
            for raw in text[chunk.clone()].split_inclusive(|byte| *byte == b'\n') {
                lines += 1;
                if lines % CANCEL_CHECK_LINES == 0 {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(CANCELLED.into());
                    }
                    if capped.load(Ordering::Relaxed) || scanner.hits.len() >= max_hits {
                        // Line totals must stay exact so later chunks number
                        // correctly even when this one stops finding hits
                        scanner.line_number = text[chunk.clone()]
                            .split_inclusive(|byte| *byte == b'\n')
                            .count() as u64;
                        break;
                    }
                }
                scanner.push(&String::from_utf8_lossy(raw));
            }
            Ok(scanner.finish())
        })
        .collect::<Result<_, String>>()?;

    let total: usize = scanned.iter().map(|chunk| chunk.hits.len()).sum();
    let mut head: Option<CompactHit> = None;
    let mut hits = Vec::with_capacity(total);
    let mut columns = FieldColumns::new(file.column_meta());
    let mut lines_before: u64 = 0;
    // Absolute line of the last entry header any stitched chunk has seen -
    // the anchor a following chunk's orphan hits belong to
    let mut last_header_abs: Option<u64> = None;
    let mut first_header_ts = None;
    let mut last_ts = None;
    // The entry still open at a chunk seam was excluded - by a line in the
    // chunk's own head block, or by one an earlier chunk saw. Everything that
    // entry contributed comes back out, wherever the contribution was made.
    let mut open_excluded = false;
    for chunk in scanned {
        let mut orphan = chunk.orphan;
        let chunk_hits = chunk.hits;
        let chunk_lines = chunk.lines;
        // Rows were numbered from this chunk's own start; concatenating shifts them
        let row_offset = columns.append(chunk.columns);

        open_excluded |= chunk.head_excluded;
        if open_excluded {
            orphan = None;
            match last_header_abs {
                Some(header) => {
                    if hits.last().is_some_and(|last: &CompactHit| last.line_number == header) {
                        hits.pop();
                    }
                }
                // Still inside the file's head block, which no header opened
                None => head = None,
            }
        }

        if let Some(mut orphan) = orphan {
            orphan.row += row_offset;
            match last_header_abs {
                Some(header) => {
                    orphan.line_number = header;
                    push_or_merge(&mut hits, orphan, &mut columns);
                }
                // No header anywhere before: this is (part of) the file's head
                // block; the first matched line stands in for the anchor
                None => match &mut head {
                    Some(head) => {
                        head.matched_groups |= orphan.matched_groups;
                        columns.merge_rows(head.row, orphan.row);
                    }
                    None => {
                        orphan.line_number += lines_before;
                        head = Some(orphan);
                    }
                },
            }
        }
        for mut hit in chunk_hits {
            hit.line_number += lines_before;
            hit.row += row_offset;
            push_or_merge(&mut hits, hit, &mut columns);
        }
        if let Some(header) = chunk.last_header {
            // A header inside this chunk opened a fresh entry; whether the one
            // left open at its end is excluded is the scanner's own verdict.
            // Without a header the entry from before the chunk is still open,
            // and so is the verdict already carried.
            last_header_abs = Some(header + lines_before);
            open_excluded = chunk.tail_excluded;
        }
        if first_header_ts.is_none() {
            first_header_ts = chunk.first_header_ts;
        }
        if chunk.last_ts.is_some() {
            last_ts = chunk.last_ts;
        }
        lines_before += chunk_lines;
    }
    Ok(FileScan {
        head,
        hits,
        columns,
        lines: lines_before,
        first_header_ts,
        last_ts,
    })
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

/// Walks back from a chunk start for the timestamp its first lines inherit.
/// Unbounded on purpose: an entry body can run tens of thousands of lines (a
/// bulk SQL insert), and a capped lookback would leave every chunk opening
/// inside it timestampless and ungrouped. The walk is a cheap reverse scan
/// that ends at the nearest header, or at the start of the text when none
/// exists - exactly as at the start of a file.
fn timestamp_before(text: &[u8], start: usize) -> Option<NaiveDateTime> {
    let mut end = start;
    loop {
        let (line, line_start) = previous_line(text, end)?;
        if let Ok(text) = std::str::from_utf8(line) {
            if let Some(ts) = parse_timestamp(text.trim_end_matches('\r')) {
                return Some(ts);
            }
        }
        end = line_start;
    }
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

/// What one scanner pass over a file or chunk produced. Line numbers in it are
/// relative to the scanned range; `scan_memory` shifts them when stitching
/// chunks back together.
struct ChunkScan {
    hits: Vec<CompactHit>,
    /// This range's field values; `CompactHit::row` indexes them until the
    /// stitching offsets the rows into the file's columns
    columns: FieldColumns,
    /// Merged hit of the entry the chunk opened inside (its header line lives
    /// in an earlier chunk, or in the previous file when the range is a whole
    /// file). `line_number` holds the first matched line as a fallback anchor
    /// until stitching rewrites it to the real header line.
    orphan: Option<CompactHit>,
    lines: u64,
    /// Line of the last entry header (timestamped line) seen, if any
    last_header: Option<u64>,
    /// An exclusion hit the lines before this range's first header. The entry
    /// they continue was opened - and may already have been recorded as a hit -
    /// by an earlier chunk, so the stitching has to take that hit back out.
    head_excluded: bool,
    /// The entry still open at the end of the range was excluded, so the next
    /// chunk's leading lines must not bring it back
    tail_excluded: bool,
    /// Timestamp of the first header the range itself contains
    first_header_ts: Option<NaiveDateTime>,
    /// Timestamp state at the end of the range (its last header's, or the
    /// inherited one when the range contains no header)
    last_ts: Option<NaiveDateTime>,
}

/// The per-line body of a scan, shared by the in-memory and the streaming path.
/// It stores compact hits only - no line text, no context; those are fetched
/// back from the file when a page is displayed.
///
/// Matches group per entry: a timestamped line opens an entry, the continuation
/// lines that follow (SQL bodies, stack traces) belong to it, and however many
/// of its lines match, the entry is one hit anchored at its header line.
struct Scanner<'a> {
    file: &'a ScanFile,
    matcher: &'a Matcher,
    request: &'a SearchRequest,
    hits: Vec<CompactHit>,
    orphan: Option<CompactHit>,
    /// This range's extracted values; stitched into the file's, then the
    /// result's, with the row indices offset as they go
    columns: FieldColumns,
    /// Reused for every hit, so extraction allocates nothing per line
    row: FieldRow,
    last_timestamp: Option<NaiveDateTime>,
    first_header_ts: Option<NaiveDateTime>,
    /// Line number of the current entry's header; 0 before the first
    /// timestamped line of the scanned range.
    entry_line: u64,
    /// A line of the current entry hit an exclusion, so the entry is out
    /// however well its other lines match. Cleared by the next header.
    entry_excluded: bool,
    /// The same, for the lines before this range's first header - their entry
    /// opened in an earlier chunk, so the veto has to travel to the stitching.
    head_excluded: bool,
    line_number: u64,
}

impl<'a> Scanner<'a> {
    fn new(file: &'a ScanFile, matcher: &'a Matcher, request: &'a SearchRequest) -> Self {
        Scanner::resume(file, matcher, request, None)
    }

    /// A scanner picking up in the middle of a file, as a parallel chunk does:
    /// it starts with the timestamp its first lines inherit. Line numbers count
    /// from its own start.
    fn resume(
        file: &'a ScanFile,
        matcher: &'a Matcher,
        request: &'a SearchRequest,
        last_timestamp: Option<NaiveDateTime>,
    ) -> Self {
        Scanner {
            file,
            matcher,
            request,
            hits: Vec::new(),
            orphan: None,
            columns: FieldColumns::new(file.column_meta()),
            row: FieldRow::new(file.column_meta().len()),
            last_timestamp,
            first_header_ts: None,
            entry_line: 0,
            entry_excluded: false,
            head_excluded: false,
            line_number: 0,
        }
    }

    fn push(&mut self, line: &str) {
        let line = line.trim_end_matches(['\r', '\n']);
        self.line_number += 1;

        // Lines without a timestamp (stack traces, continuations) inherit the last one
        if let Some(ts) = parse_timestamp_with(line, &self.file.spec.timestamps) {
            self.last_timestamp = Some(ts);
            if self.entry_line == 0 {
                self.first_header_ts = Some(ts);
            }
            self.entry_line = self.line_number;
            self.entry_excluded = false;
        }

        // Lines before the range's first header cannot be dated yet - their
        // timestamp resolves later (previous chunk or file), and so does their
        // date filter; everything else is filtered here.
        let before_first_header = self.entry_line == 0 && self.last_timestamp.is_none();
        let date_ok = before_first_header
            || self.request.has_time_window()
            || !self.file.filter_lines_by_date
            || self.last_timestamp.is_some_and(|ts| {
                ts.date() >= self.request.date_from && ts.date() <= self.request.date_to
            });
        // The time window applies to every file, dated or not; entries that can
        // be dated here are filtered here, undatable ones wait for attach_heads.
        let time_ok = before_first_header
            || self
                .last_timestamp
                .is_some_and(|ts| {
                    self.request
                        .in_time_window(ts, self.file.log_offset_minutes)
                });
        if !date_ok || !time_ok {
            return;
        }

        // An exclusion rejects the entry, not just the line carrying it: the
        // "document" in an entry's URL must take its stack trace with it, or
        // the trace's own lines would bring the entry back as a hit.
        if self.matcher.excludes(line) {
            self.exclude_entry();
            return;
        }
        if self.entry_excluded {
            return;
        }

        if let Some(matched) = self.matcher.match_line_included(line) {
            self.row.clear();
            extract_into(line, &self.file.spec.fields, &mut self.row);
            let matched_groups = group_mask(self.matcher.group_names(), &matched);

            if self.entry_line == 0 {
                // Continuation of an entry that opened before this range - one
                // merged hit, anchored (and dated, if need be) afterwards. Every
                // orphan match precedes this range's first header, so the orphan's
                // row is always the last one pushed.
                match &mut self.orphan {
                    Some(orphan) => {
                        orphan.matched_groups |= matched_groups;
                        self.columns.merge_into_last(&self.row);
                    }
                    None => {
                        self.orphan = Some(CompactHit {
                            // The caller stamps the real file-table index on afterwards
                            file: 0,
                            line_number: self.line_number,
                            timestamp: self.last_timestamp,
                            matched_groups,
                            row: self.columns.push_row(&self.row),
                        });
                    }
                }
                return;
            }

            // A further match inside the entry already being recorded folds into
            // it, so no row is spent on a hit that will not be kept
            let same_entry = self
                .hits
                .last()
                .is_some_and(|last| last.line_number == self.entry_line);
            if same_entry {
                if let Some(last) = self.hits.last_mut() {
                    last.matched_groups |= matched_groups;
                }
                self.columns.merge_into_last(&self.row);
                return;
            }
            self.hits.push(CompactHit {
                file: 0,
                line_number: self.entry_line,
                timestamp: self.last_timestamp,
                matched_groups,
                row: self.columns.push_row(&self.row),
            });
        }
    }

    /// Drops the entry the current line belongs to. Its hit, if one was already
    /// recorded, is the last one pushed - a hit is only ever appended for the
    /// entry being scanned. The row behind it stays in the columns, unreferenced
    /// like the rows a capped scan leaves; a second pass to reclaim it would
    /// cost more than the few bytes it holds.
    fn exclude_entry(&mut self) {
        self.entry_excluded = true;
        if self.entry_line == 0 {
            self.head_excluded = true;
            self.orphan = None;
            return;
        }
        if self.hits.last().is_some_and(|last| last.line_number == self.entry_line) {
            self.hits.pop();
        }
    }

    fn finish(self) -> ChunkScan {
        ChunkScan {
            hits: self.hits,
            orphan: self.orphan,
            columns: self.columns,
            lines: self.line_number,
            last_header: (self.entry_line > 0).then_some(self.entry_line),
            head_excluded: self.head_excluded,
            tail_excluded: self.entry_excluded,
            first_header_ts: self.first_header_ts,
            last_ts: self.last_timestamp,
        }
    }
}

/// Every built-in format, tried in order. Used where no pack is in play - the
/// cache's log-clock detection and the growing-file segmenter read lines without
/// knowing which service they came from.
///
/// Continuation lines (SQL bodies, embedded HTML, stack traces) have no
/// timestamp of their own and yield None.
pub fn parse_timestamp(line: &str) -> Option<NaiveDateTime> {
    parse_timestamp_with(line, &[])
}

/// The formats a pack declared, in its order; an empty list means every
/// built-in. Runs on every line of every file, so the built-ins are hand-rolled
/// byte parsers and a `custom` regex - which cannot be - is the slow path a pack
/// opts into knowingly.
pub fn parse_timestamp_with(
    line: &str,
    formats: &[CompiledTimestamp],
) -> Option<NaiveDateTime> {
    if formats.is_empty() {
        let bytes = line.as_bytes();
        // Every built-in format starts with a digit; most continuation lines do
        // not, and this is the cheapest way to skip them
        if !bytes.first()?.is_ascii_digit() {
            return None;
        }
        return parse_iso(bytes)
            .or_else(|| parse_dotted(bytes))
            .or_else(|| parse_us(line));
    }

    for format in formats {
        let parsed = match format {
            CompiledTimestamp::Builtin(BuiltinTimestamp::Iso8601) => {
                first_digit(line).and_then(parse_iso)
            }
            CompiledTimestamp::Builtin(BuiltinTimestamp::DottedDmy) => {
                first_digit(line).and_then(parse_dotted)
            }
            CompiledTimestamp::Builtin(BuiltinTimestamp::UsMdy) => parse_us(line),
            CompiledTimestamp::Custom(regex) => parse_by_regex(line, regex),
        };
        if parsed.is_some() {
            return parsed;
        }
    }
    None
}

/// The line's bytes, but only when it opens with a digit as every built-in
/// format does.
fn first_digit(line: &str) -> Option<&[u8]> {
    let bytes = line.as_bytes();
    bytes.first()?.is_ascii_digit().then_some(bytes)
}

/// `7/14/2026 1:15:55 AM` - variable width, so the third space-separated token
/// bounds it rather than a fixed offset.
fn parse_us(line: &str) -> Option<NaiveDateTime> {
    if !line.as_bytes().first()?.is_ascii_digit() {
        return None;
    }
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
    if end == 0 {
        return None;
    }
    NaiveDateTime::parse_from_str(&line[..end], "%m/%d/%Y %I:%M:%S %p").ok()
}

/// A pack's own format, from named groups. `f` is fractional seconds of any
/// width, scaled to nanoseconds the same way the built-in parsers do.
fn parse_by_regex(line: &str, regex: &Regex) -> Option<NaiveDateTime> {
    let caps = regex.captures(line)?;
    let part = |name: &str| caps.name(name)?.as_str().parse::<u32>().ok();
    let nano = match caps.name("f") {
        Some(fraction) => {
            let digits = fraction.as_str();
            let mut nano = 0u32;
            let mut scale = 100_000_000u32;
            for byte in digits.bytes() {
                if !byte.is_ascii_digit() || scale == 0 {
                    break;
                }
                nano += u32::from(byte - b'0') * scale;
                scale /= 10;
            }
            nano
        }
        None => 0,
    };
    NaiveDate::from_ymd_opt(part("y")? as i32, part("m")?, part("d")?)?
        .and_hms_nano_opt(part("H")?, part("M")?, part("S")?, nano)
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

/// `dd.mm.yyyy hh:mm:ss` with optional `.mmm` - the shape both app log families
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

// Field extraction itself lives in `fields::extract_into`: which fields exist,
// what gates them and how their values are typed all come from the packs now.

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

    /// A pack whose fields match the fixture text, so scans actually extract
    /// something and the column stitching is under test rather than bypassed.
    fn test_spec() -> Arc<PackScanSpec> {
        let pack = crate::plugin::parse_and_compile(
            r#"{
                "id": "test", "name": "Test", "source": { "type": "local-folder" },
                "timestamps": ["dotted-dmy", "iso8601", "us-mdy"],
                "fields": [
                  { "key": "level", "label": "Level",
                    "regex": "\\b(?<v>INFO|WARN|ERROR)\\b" },
                  { "key": "id", "label": "Id", "gate": "id=", "type": "number",
                    "regex": "id=(?<v>\\d+)" }
                ]
            }"#,
            crate::plugin::PackOrigin::Bundled,
            false,
        )
        .expect("test pack must compile");
        let columns: Arc<Vec<FieldMeta>> =
            Arc::new(pack.fields.iter().map(FieldMeta::of).collect());
        Arc::new(PackScanSpec {
            columns,
            timestamps: pack.timestamps.clone(),
            fields: pack.fields.iter().cloned().enumerate().collect(),
        })
    }

    fn scan_file_of(name: &str) -> ScanFile {
        ScanFile {
            service_id: "test/Svc".into(),
            path: PathBuf::from(name),
            name: name.into(),
            uncompressed_size: 0,
            log_offset_minutes: None,
            filter_lines_by_date: false,
            date: None,
            instance: None,
            spec: test_spec(),
        }
    }

    /// Everything about two scans that must agree, `row` aside: a parallel scan
    /// numbers its rows differently, so the values behind them are compared
    /// rather than the indices.
    fn assert_same_scan(actual: &FileScan, expected: &FileScan) {
        let anchors = |scan: &FileScan| {
            scan.hits
                .iter()
                .map(|hit| (hit.file, hit.line_number, hit.timestamp, hit.matched_groups))
                .collect::<Vec<_>>()
        };
        assert_eq!(anchors(actual), anchors(expected), "same hits at the same anchors");
        assert_eq!(actual.lines, expected.lines, "same line count");
        assert_eq!(actual.first_header_ts, expected.first_header_ts);
        assert_eq!(actual.last_ts, expected.last_ts);
        assert_eq!(
            actual.head.map(|h| (h.line_number, h.timestamp, h.matched_groups)),
            expected.head.map(|h| (h.line_number, h.timestamp, h.matched_groups)),
            "same head block"
        );

        let width = expected.columns.width();
        for (a, b) in actual.hits.iter().zip(&expected.hits) {
            for column in 0..width {
                assert_eq!(
                    actual.columns.display(column, a.row),
                    expected.columns.display(column, b.row),
                    "field {column} of the hit at line {}",
                    a.line_number
                );
            }
        }
    }

    /// One pass down the whole text, shaped like `scan_file`'s streaming path.
    fn sequential_scan(
        file: &ScanFile,
        matcher: &Matcher,
        request: &SearchRequest,
        text: &[u8],
    ) -> FileScan {
        let mut scanner = Scanner::new(file, matcher, request);
        for raw in text.split_inclusive(|byte| *byte == b'\n') {
            scanner.push(&String::from_utf8_lossy(raw));
        }
        let scan = scanner.finish();
        FileScan {
            head: scan.orphan,
            hits: scan.hits,
            columns: scan.columns,
            lines: scan.lines,
            first_header_ts: scan.first_header_ts,
            last_ts: scan.last_ts,
        }
    }

    fn request_for(file: &ScanFile, query: &str) -> SearchRequest {
        SearchRequest {
            service_ids: vec![file.service_id.clone()],
            date_from: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
            date_to: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
            time_from: None,
            time_to: None,
            query: query.into(),
            is_regex: false,
            case_sensitive: true,
            context_lines: 2,
        }
    }

    /// The parallel scan splits the text at line boundaries and stitches the
    /// results back together. It must be indistinguishable from one pass down
    /// the file - same hits, same line numbers, same timestamps.
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
            time_from: None,
            time_to: None,
            query: "BOOM".into(),
            is_regex: false,
            case_sensitive: true,
            context_lines: 2,
        };

        let expected = sequential_scan(&file, &matcher, &request, &text);
        let actual = scan_memory(
            &file,
            &matcher,
            &request,
            &text,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            usize::MAX,
        )
        .unwrap();

        assert!(expected.hits.len() > 100, "a meaningful number of hits");
        assert_same_scan(&actual, &expected);
    }

    #[test]
    fn a_capped_scan_stops_early_and_keeps_line_totals_exact() {
        let text = parallel_text();
        let file = scan_file_of("log-2026-07-14.log");
        let matcher = Matcher::new("", false, true).unwrap();
        let request = SearchRequest {
            service_ids: vec![file.service_id.clone()],
            date_from: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
            date_to: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
            time_from: None,
            time_to: None,
            query: String::new(),
            is_regex: false,
            case_sensitive: true,
            context_lines: 0,
        };

        let total_lines = sequential_scan(&file, &matcher, &request, &text).lines;
        let capped = scan_memory(
            &file,
            &matcher,
            &request,
            &text,
            &AtomicBool::new(false),
            &AtomicBool::new(true), // the cap tripped elsewhere
            usize::MAX,
        )
        .unwrap();

        assert!(
            (capped.hits.len() as u64) < total_lines,
            "an empty query over capped chunks must not keep every line"
        );
        assert_eq!(capped.lines, total_lines, "line totals stay exact for numbering");
    }

    /// Only entries whose timestamp falls inside the time window survive, and a
    /// continuation line inherits its entry's timestamp for the check.
    #[test]
    fn a_time_window_filters_hits_by_entry_timestamp() {
        let text = b"14.07.2026 09:00:00.000 ERROR - BOOM early\n\
                     14.07.2026 12:00:00.000 ERROR - BOOM inside\n\
                     continuation BOOM\n\
                     14.07.2026 18:00:00.000 ERROR - BOOM late\n";
        let file = scan_file_of("log-2026-07-14.log");
        let matcher = Matcher::new("BOOM", false, true).unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        let mut request = request_for(&file, "BOOM");
        request.time_from = Some(day.and_hms_opt(11, 0, 0).unwrap());
        request.time_to = Some(day.and_hms_opt(13, 0, 0).unwrap());

        let scan = sequential_scan(&file, &matcher, &request, text);

        assert_eq!(
            scan.hits.iter().map(|hit| hit.line_number).collect::<Vec<_>>(),
            vec![2],
            "only the 12:00 entry is inside 11:00-13:00, merged with its continuation line"
        );
    }

    #[test]
    fn a_time_window_converts_each_services_log_clock_to_utc() {
        let text = b"14.07.2026 09:00:00.000 ERROR - early\n\
                     14.07.2026 12:00:00.000 ERROR - inside\n\
                     14.07.2026 18:00:00.000 ERROR - late\n";
        let mut file = scan_file_of("log-2026-07-14.log");
        file.log_offset_minutes = Some(120);
        let matcher = Matcher::new("ERROR", false, true).unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        let mut request = request_for(&file, "ERROR");
        request.time_from = Some(day.and_hms_opt(9, 59, 0).unwrap());
        request.time_to = Some(day.and_hms_opt(10, 1, 0).unwrap());

        let scan = sequential_scan(&file, &matcher, &request, text);

        assert_eq!(
            scan.hits.iter().map(|hit| hit.line_number).collect::<Vec<_>>(),
            vec![2],
            "12:00 on a UTC+2 log clock is inside the 09:59-10:01 UTC window"
        );
    }

    #[test]
    fn a_time_window_can_include_an_adjacent_raw_day_in_local_mode() {
        let text = b"13.07.2026 22:30:00.000 ERROR - local July 14\n\
                     14.07.2026 22:30:00.000 ERROR - local July 15\n";
        let mut file = scan_file_of("growing.log");
        file.filter_lines_by_date = true;
        file.log_offset_minutes = Some(0);
        let matcher = Matcher::new("ERROR", false, true).unwrap();
        let selected_day = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        let mut request = request_for(&file, "ERROR");
        request.date_from = selected_day;
        request.date_to = selected_day;
        request.time_from = Some("2026-07-13T22:00:00".parse().unwrap());
        request.time_to = Some("2026-07-14T21:59:59".parse().unwrap());

        let scan = sequential_scan(&file, &matcher, &request, text);

        assert_eq!(
            scan.hits.iter().map(|hit| hit.line_number).collect::<Vec<_>>(),
            vec![1],
            "the absolute window, rather than the raw log date, defines the displayed local day"
        );
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

    /// A bulk SQL insert can push the header tens of thousands of lines back;
    /// the lookback must still reach it (it was once capped at 4096 lines,
    /// which left such chunks timestampless and ungrouped).
    #[test]
    fn the_lookback_reaches_a_header_tens_of_thousands_of_lines_back() {
        let mut text = String::from("14.07.2026 09:00:00 info (sql) - insert into @Employees values\n");
        for k in 0..20_000 {
            text.push_str(&format!("({k},'code','some person'),\n"));
        }
        let ts = timestamp_before(text.as_bytes(), text.len()).unwrap();
        assert_eq!(ts.to_string(), "2026-07-14 09:00:00");
    }

    /// One entry can be bigger than a whole chunk, so consecutive chunks see no
    /// header at all; stitching must still anchor their hits to the entry.
    #[test]
    fn an_entry_larger_than_a_chunk_still_groups_into_one_hit() {
        let mut text = String::with_capacity(PARALLEL_MIN_BYTES * 3);
        let mut entries = 0;
        while text.len() < PARALLEL_MIN_BYTES * 2 {
            entries += 1;
            text.push_str(&format!(
                "14.07.2026 09:00:{:02} info (sql) - insert into @Employees values\n",
                entries % 60
            ));
            // Roughly 3 MB of continuation lines - wider than a chunk
            for k in 0..100_000 {
                text.push_str(&format!("({k},'code-{entries}','NEEDLE person'),\n"));
            }
        }
        let text = text.into_bytes();
        assert!(
            split_at_lines(&text, 8).len() > 2,
            "the text must be split into several chunks for this to test anything"
        );

        let file = scan_file_of("log-2026-07-14.log");
        let matcher = Matcher::new("NEEDLE", false, true).unwrap();
        let request = request_for(&file, "NEEDLE");

        let expected = sequential_scan(&file, &matcher, &request, &text);
        let actual = scan_memory(
            &file,
            &matcher,
            &request,
            &text,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            usize::MAX,
        )
        .unwrap();

        assert_eq!(expected.hits.len(), entries, "one hit per entry");
        assert!(
            expected.hits.iter().all(|hit| hit.timestamp.is_some()),
            "every hit carries its entry's timestamp"
        );
        assert_same_scan(&actual, &expected);
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

    /// A file that opens mid-entry (rotation cut the entry in half): the
    /// leading continuation lines collect into one undated head hit, which
    /// `attach_heads` dates from the previous file of the chain - or, when
    /// that file is not in the scan, from the file's own first header minus a
    /// millisecond so the block still sorts just before it.
    #[test]
    fn a_head_block_inherits_across_files_or_falls_back_to_the_first_header() {
        let text = b"(1,'19859','NEEDLE Amrita'),\n\
(2,'19860','NEEDLE Muna'),\n\
14.07.2026 00:00:05 info - first entry NEEDLE\n";
        let mut day14 = scan_file_of("log-2026-07-14.log");
        day14.date = Some(NaiveDate::from_ymd_opt(2026, 7, 14).unwrap());
        let matcher = Matcher::new("NEEDLE", false, true).unwrap();
        let request = request_for(&day14, "NEEDLE");

        let scan = sequential_scan(&day14, &matcher, &request, text);
        let head = scan.head.as_ref().expect("leading lines form one head block");
        assert_eq!(head.line_number, 1);
        assert!(head.timestamp.is_none(), "undated until attach_heads resolves it");
        assert_eq!(scan.hits.len(), 1, "the file's own entry is separate");

        // The previous day's file is in the scan: the head inherits its last
        // entry's timestamp - the header of the very entry it continues
        let mut day13 = scan_file_of("log-2026-07-13.log");
        day13.date = Some(NaiveDate::from_ymd_opt(2026, 7, 13).unwrap());
        let previous = FileScan {
            head: None,
            hits: Vec::new(),
            columns: FieldColumns::new(&[]),
            lines: 0,
            first_header_ts: None,
            last_ts: Some("2026-07-13T23:59:58".parse().unwrap()),
        };
        let mut scans = vec![previous, scan];
        attach_heads(&[day13, day14.clone()], &mut scans, &request);
        assert_eq!(scans[1].hits.len(), 2, "the head joins the file's hits");
        assert_eq!(
            scans[1].hits[0].timestamp.unwrap().to_string(),
            "2026-07-13 23:59:58"
        );

        // No previous file in the scan: the first own header minus a
        // millisecond stands in
        let mut scans = vec![sequential_scan(&day14, &matcher, &request, text)];
        attach_heads(std::slice::from_ref(&day14), &mut scans, &request);
        assert_eq!(
            scans[0].hits[0].timestamp.unwrap().to_string(),
            "2026-07-14 00:00:04.999"
        );
    }

    /// Hourly rotation (log-YYYY-MM-DD_HH.log) cuts entries mid-day: the head
    /// of the next hour's file inherits the previous hour's last header even
    /// though both files carry the same date.
    #[test]
    fn a_head_block_inherits_from_a_same_day_rotation() {
        let text = b"(1,'19859','NEEDLE Amrita'),\n\
14.07.2026 19:00:02 info - first entry NEEDLE\n";
        let day = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        let mut hour18 = scan_file_of("log-2026-07-14_18.log");
        hour18.date = Some(day);
        let mut hour19 = scan_file_of("log-2026-07-14_19.log");
        hour19.date = Some(day);
        let matcher = Matcher::new("NEEDLE", false, true).unwrap();
        let request = request_for(&hour19, "NEEDLE");

        let scan = sequential_scan(&hour19, &matcher, &request, text);
        assert!(scan.head.is_some(), "leading lines form a head block");
        let previous = FileScan {
            head: None,
            hits: Vec::new(),
            columns: FieldColumns::new(&[]),
            lines: 0,
            first_header_ts: Some("2026-07-14T18:00:01".parse().unwrap()),
            last_ts: Some("2026-07-14T18:59:59".parse().unwrap()),
        };
        let mut scans = vec![previous, scan];
        attach_heads(&[hour18, hour19], &mut scans, &request);
        assert_eq!(
            scans[1].hits[0].timestamp.unwrap().to_string(),
            "2026-07-14 18:59:59"
        );
    }

    #[test]
    fn fetch_hit_lines_returns_line_text_and_context() {
        let dir = std::env::temp_dir().join("loglooker-search-fetch");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let text = "14.07.2026 10:00:00 one\n\
14.07.2026 10:00:01 two\n\
14.07.2026 10:00:02 three\n\
14.07.2026 10:00:03 four\n\
14.07.2026 10:00:04 five\n";
        let path = dir.join("log.zst");
        std::fs::write(&path, zstd::stream::encode_all(text.as_bytes(), 3).unwrap()).unwrap();

        let file = ScanFile {
            service_id: "test/Svc".into(),
            path,
            name: "log".into(),
            uncompressed_size: text.len() as u64,
            log_offset_minutes: None,
            filter_lines_by_date: false,
            date: None,
            instance: None,
            spec: test_spec(),
        };
        let cache = MemCache::new(false, 2048);
        let mut got: Vec<(usize, FetchedLine)> = Vec::new();
        fetch_hit_lines(&[file], &[(0, 3, 0), (0, 5, 1)], 1, &cache, |slot, fetched| {
            got.push((slot, fetched));
        })
        .unwrap();
        got.sort_by_key(|(slot, _)| *slot);

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1.line, "14.07.2026 10:00:02 three");
        assert_eq!(got[0].1.context_before, ["14.07.2026 10:00:01 two"]);
        assert_eq!(got[0].1.context_after, ["14.07.2026 10:00:03 four"]);
        assert_eq!(got[1].1.line, "14.07.2026 10:00:04 five");
        assert_eq!(got[1].1.context_before, ["14.07.2026 10:00:03 four"]);
        assert!(got[1].1.context_after.is_empty(), "EOF cuts the after-context short");
    }

    /// However many lines of a multi-line entry match, the entry is one hit,
    /// anchored at its header line and carrying its timestamp.
    #[test]
    fn matches_across_a_multi_line_entry_collapse_into_one_hit() {
        let text = b"14.07.2026 10:00:00 info (sql) - insert into @Employees values\n\
(1,'19859','Adhikari Amrita Kc'),\n\
(2,'19860','Adhikari Muna'),\n\
14.07.2026 10:00:01 info - Adhikari logged in\n";
        let file = scan_file_of("Log.txt");
        let matcher = Matcher::new("Adhikari", false, true).unwrap();
        let request = request_for(&file, "Adhikari");

        let scan = sequential_scan(&file, &matcher, &request, text);
        assert_eq!(scan.lines, 4);
        assert!(scan.head.is_none(), "the text opens with a header, so no head block");
        assert_eq!(scan.hits.len(), 2, "one hit per entry, not per matching line");
        assert_eq!(scan.hits[0].line_number, 1, "anchored at the entry's header line");
        assert_eq!(scan.hits[0].timestamp.unwrap().to_string(), "2026-07-14 10:00:00");
        assert_eq!(scan.hits[1].line_number, 4);
        assert_eq!(scan.first_header_ts.unwrap().to_string(), "2026-07-14 10:00:00");
        assert_eq!(scan.last_ts.unwrap().to_string(), "2026-07-14 10:00:01");
    }

    /// Entries whose matching continuation lines straddle chunk boundaries:
    /// the stitched parallel scan must group them exactly like one pass does.
    #[test]
    fn a_parallel_scan_groups_entries_that_straddle_chunk_boundaries() {
        let mut text = String::with_capacity(PARALLEL_MIN_BYTES * 2);
        let mut entries = 0;
        while text.len() < PARALLEL_MIN_BYTES * 2 {
            entries += 1;
            let n = entries;
            let (s, m, h) = (n % 60, (n / 60) % 60, (n / 24) % 24);
            text.push_str(&format!(
                "14.07.2026 {h:02}:{m:02}:{s:02} info (sql) - insert into @Employees values\n"
            ));
            for k in 0..40 {
                text.push_str(&format!("({k},'code-{n}','NEEDLE person {n}'),\n"));
            }
        }
        let text = text.into_bytes();
        assert!(
            split_at_lines(&text, 8).len() > 1,
            "the text must actually be split for this to test anything"
        );

        let file = scan_file_of("log-2026-07-14.log");
        let matcher = Matcher::new("NEEDLE", false, true).unwrap();
        let request = request_for(&file, "NEEDLE");

        let expected = sequential_scan(&file, &matcher, &request, &text);
        let actual = scan_memory(
            &file,
            &matcher,
            &request,
            &text,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            usize::MAX,
        )
        .unwrap();

        assert_eq!(expected.hits.len(), entries, "one hit per entry");
        assert_same_scan(&actual, &expected);
    }

    #[test]
    fn fetch_returns_the_whole_entry_for_a_header_anchor() {
        let dir = std::env::temp_dir().join("loglooker-search-fetch-entry");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let text = "14.07.2026 10:00:00 info - first\n\
14.07.2026 10:00:01 info (sql) - insert into @T values\n\
(1,'a'),\n\
(2,'b')\n\
14.07.2026 10:00:02 info - after\n\
tail line\n";
        let path = dir.join("log.zst");
        std::fs::write(&path, zstd::stream::encode_all(text.as_bytes(), 3).unwrap()).unwrap();

        let file = ScanFile {
            service_id: "test/Svc".into(),
            path,
            name: "log".into(),
            uncompressed_size: text.len() as u64,
            log_offset_minutes: None,
            filter_lines_by_date: false,
            date: None,
            instance: None,
            spec: test_spec(),
        };
        let cache = MemCache::new(false, 2048);
        let mut got: Vec<(usize, FetchedLine)> = Vec::new();
        fetch_hit_lines(&[file], &[(0, 2, 0), (0, 6, 1)], 1, &cache, |slot, fetched| {
            got.push((slot, fetched));
        })
        .unwrap();
        got.sort_by_key(|(slot, _)| *slot);

        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].1.line,
            "14.07.2026 10:00:01 info (sql) - insert into @T values\n(1,'a'),\n(2,'b')",
            "a header anchor fetches the whole entry"
        );
        assert_eq!(got[0].1.context_before, ["14.07.2026 10:00:00 info - first"]);
        assert_eq!(
            got[0].1.context_after,
            ["14.07.2026 10:00:02 info - after"],
            "after-context starts at the next entry"
        );
        assert_eq!(got[1].1.line, "tail line", "an anchor with no entry after it runs to EOF");
        assert_eq!(got[1].1.context_before, ["14.07.2026 10:00:02 info - after"]);
    }

    /// Narrowing tests a whole entry, and it has to read one exactly as the
    /// scan does: any line may carry the match, and any line may veto it.
    #[test]
    fn matches_entry_tests_every_line_of_the_entry() {
        let entry = "14.07.2026 10:00:00 error - failed\n   at Repo.Save()\n   at Api.Post()";

        let on_header = Matcher::new("failed", false, false).unwrap();
        assert!(on_header.matches_entry(entry), "a match on the header line keeps the entry");

        let on_continuation = Matcher::new("Repo.Save", false, false).unwrap();
        assert!(
            on_continuation.matches_entry(entry),
            "a match on a stack-trace line keeps the entry too"
        );

        let anchored = Matcher::new("^   at Api", true, false).unwrap();
        assert!(anchored.matches_entry(entry), "^ anchors per line, not at the entry's start");

        let absent = Matcher::new("timeout", false, false).unwrap();
        assert!(!absent.matches_entry(entry), "a term no line carries drops the entry");
    }

    #[test]
    fn matches_entry_lets_an_exclusion_veto_the_whole_entry() {
        let entry = "14.07.2026 10:00:00 error - failed\n   at Repo.Save()";
        let excluded = Matcher::new("^(?!.*Repo\\.Save).*failed", true, false).unwrap();
        assert!(
            !excluded.matches_entry(entry),
            "a NOT term on a continuation line rejects the entry its header matched"
        );
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
    fn guids_round_trip_through_the_compact_form() {
        let bytes = parse_guid("3306928E-aaaa-bbbb-cccc-ddddeeeeffff").unwrap();
        assert_eq!(format_guid(&bytes), "3306928e-aaaa-bbbb-cccc-ddddeeeeffff");
        assert!(parse_guid("not-a-guid").is_none());
        assert!(parse_guid("3306928e-aaaa-bbbb-cccc-ddddeeeeffff00").is_none());
    }

    #[test]
    fn group_masks_round_trip_through_names() {
        let names = vec!["err".to_string(), "warn".to_string(), "info".to_string()];
        let mask = group_mask(&names, &["warn".into(), "err".into()]);
        assert_eq!(mask, 0b011);
        assert_eq!(mask_to_groups(&names, mask), ["err", "warn"]);
        assert_eq!(group_mask(&names, &[]), 0);
        assert!(mask_to_groups(&names, 0).is_empty());
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
        // empty alternative - it always succeeds, but captures when present -
        // behind an "at least one occurs" assertion
        let pattern = r"^(?!.*noise)(?=.*(?:ERROR|WARN))(?=.*(?<err>ERROR)|)(?=.*(?<warn>WARN)|)";
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

    /// The leading `(?!...)` run is lifted out of the pattern so the scanner can
    /// apply it to a whole entry; what is left must still be the same test.
    #[test]
    fn leading_negative_lookaheads_split_off_as_exclusions() {
        assert_eq!(
            split_leading_nots(r"^(?!.*a\)b)(?!.*[)])(?=.*?(?<hit>x))"),
            Some((
                r"^(?=.*?(?<hit>x))".to_string(),
                r"(?:.*a\)b)|(?:.*[)])".to_string()
            )),
            "escapes, nested parens and classes do not end a lookahead early"
        );
        // Nothing to lift: unanchored, or the run does not open the pattern
        assert_eq!(split_leading_nots(r"(?!.*a)b"), None);
        assert_eq!(split_leading_nots(r"^(?=.*a)(?!.*b)"), None);

        let matcher = Matcher::new(r"^(?!.*noise)(?=.*?(?<hit>ERROR))", true, true).unwrap();
        assert_eq!(matcher.group_names(), ["hit"], "the groups survive the split");
        assert!(matcher.excludes("noise here"));
        assert!(!matcher.excludes("an ERROR"));
        // Line-at-a-time callers keep the reading they always had
        assert_eq!(matcher.match_line("an ERROR"), Some(vec!["hit".into()]));
        assert_eq!(matcher.match_line("noise ERROR"), None);
        assert!(!matcher.matches("noise ERROR"));
    }

    /// A hit is an entry, so an exclusion must take the whole entry with it -
    /// header, SQL body and stack trace. Applying it to the matched line alone
    /// let an excluded entry back in through one of its own trace lines.
    #[test]
    fn an_exclusion_drops_the_whole_entry_not_only_its_own_line() {
        let text = b"14.07.2026 09:00:00.000 ERROR - deadlocked, URL: /v1/document/new\n\
                     System.Exception: Transaction was deadlocked. Rerun the transaction.\n\
                     14.07.2026 09:01:00.000 ERROR - deadlocked victim\n\
                     System.Exception: at Document.New() in /src/Document/New.cs\n\
                     14.07.2026 09:02:00.000 ERROR - deadlocked victim\n\
                     System.Exception: Transaction was deadlocked. Rerun the transaction.\n\
                     14.07.2026 09:03:00.000 INFO - document created\n\
                     System.Exception: Transaction was deadlocked. Rerun the transaction.\n";
        let file = scan_file_of("log-2026-07-14.log");
        // What the builder compiles for "contains deadlock, does not contain document"
        let matcher = Matcher::new(r"^(?!.*document)(?=.*?(?<name>deadlock))", true, false).unwrap();
        let request = request_for(&file, "");

        let scan = sequential_scan(&file, &matcher, &request, text);

        assert_eq!(
            scan.hits.iter().map(|hit| hit.line_number).collect::<Vec<_>>(),
            vec![5],
            "only the entry with no 'document' on any of its lines survives: the \
             first is excluded by its header, the second by its stack trace after \
             its header already matched, the last by the line the match sits on"
        );
    }

    /// Entries long enough to straddle chunk boundaries, every third one closing
    /// with its exclusion - so the line that vetoes an entry routinely lands in a
    /// different chunk than the header it vetoes, and the veto has to travel
    /// through the stitching.
    fn excluded_parallel_text() -> Vec<u8> {
        let mut text = String::with_capacity(PARALLEL_MIN_BYTES * 2);
        let mut n = 0;
        while text.len() < PARALLEL_MIN_BYTES * 2 {
            n += 1;
            let (s, m, h) = (n % 60, (n / 60) % 60, (n / 24) % 24);
            text.push_str(&format!(
                "14.07.2026 {h:02}:{m:02}:{s:02}.{:03} ERROR - BOOM at id={n}\n",
                n % 1000
            ));
            for _ in 0..400 {
                text.push_str("    at Example.Api.Handler.Handle()\n");
            }
            if n % 3 == 0 {
                text.push_str("    at Noise.Retry.Handle()\n");
            }
        }
        text.into_bytes()
    }

    #[test]
    fn an_exclusion_crossing_a_chunk_boundary_still_drops_its_entry() {
        let text = excluded_parallel_text();
        let chunks = split_at_lines(&text, rayon::current_num_threads());
        let starts: Vec<usize> = chunks.iter().skip(1).map(|chunk| chunk.start).collect();

        // Entries, exclusions, and the exclusions cut off from their own header
        let (mut entries, mut excluded, mut straddling) = (0, 0, 0);
        let (mut header_at, mut at) = (0usize, 0usize);
        for line in text.split_inclusive(|byte| *byte == b'\n') {
            if line.starts_with(b"14.") {
                entries += 1;
                header_at = at;
            }
            if line.starts_with(b"    at Noise") {
                excluded += 1;
                if starts.iter().any(|start| *start > header_at && *start <= at) {
                    straddling += 1;
                }
            }
            at += line.len();
        }
        assert!(
            chunks.len() == 1 || straddling > 0,
            "the split must actually cut an entry off from its exclusion"
        );

        let file = scan_file_of("log-2026-07-14.log");
        let matcher = Matcher::new(r"^(?!.*Noise)(?=.*?(?<hit>BOOM))", true, true).unwrap();
        let request = request_for(&file, "");

        let expected = sequential_scan(&file, &matcher, &request, &text);
        assert_eq!(
            expected.hits.len(),
            entries - excluded,
            "every entry but the excluded ones is a hit"
        );

        let actual = scan_memory(
            &file,
            &matcher,
            &request,
            &text,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            usize::MAX,
        )
        .unwrap();
        assert_same_scan(&actual, &expected);
    }

    #[test]
    fn invalid_pattern_reports_an_error_from_the_richer_engine() {
        assert!(Matcher::new(r"(?<broken", true, true).is_err());
    }

    #[test]
    fn extract_pulls_out_the_single_group() {
        let matcher = Matcher::new(r"@ID_Login='(?<id>[^']+)'", true, true).unwrap();
        let line = "exec SF_Page_Login @ID_Login='38146d1d-2d3e-46c7-92d6-566b8387f378', @ID_Page=null";
        assert_eq!(
            matcher.extract(line).as_deref(),
            Some("38146d1d-2d3e-46c7-92d6-566b8387f378")
        );
        assert_eq!(matcher.extract("no login here"), None);
    }

    #[test]
    fn extract_space_joins_every_group_that_participated() {
        let matcher = Matcher::new(r"(?<d>\d+)-(?<t>\d+)", true, true).unwrap();
        assert_eq!(matcher.extract("at 2026-07 done").as_deref(), Some("2026 07"));

        // ANY alternation: only the side that matched participates, so a single
        // value comes back
        let matcher = Matcher::new(r"(?<err>ERROR)|(?<warn>WARN)", true, true).unwrap();
        assert_eq!(matcher.extract("a WARN line").as_deref(), Some("WARN"));
        assert_eq!(matcher.extract("an ERROR line").as_deref(), Some("ERROR"));
    }

    #[test]
    fn extract_reaches_into_the_builder_all_form() {
        // Groups live inside look-aheads; group 0 is the zero-width anchor, so
        // every real group is space-joined in pattern order
        let matcher = Matcher::new(r"^(?=.*(?<a>foo))(?=.*(?<b>bar))", true, false).unwrap();
        assert_eq!(matcher.extract("bar before Foo").as_deref(), Some("Foo bar"));
    }

    #[test]
    fn extract_without_a_group_returns_the_whole_match() {
        let matcher = Matcher::new(r"[0-9a-f]{8}", true, true).unwrap();
        assert_eq!(matcher.extract("id 38146d1d done").as_deref(), Some("38146d1d"));
    }

    #[test]
    fn extract_treats_unnamed_groups_as_plain_grouping() {
        // Only named groups isolate; a bare (...) keeps the whole match, so a
        // duration pattern does not lose its integer part to the fraction group
        let matcher = Matcher::new(r"\d+(\.\d+)?ms", true, true).unwrap();
        assert_eq!(matcher.extract("took 10.2848ms").as_deref(), Some("10.2848ms"));
        assert_eq!(matcher.extract("took 7ms").as_deref(), Some("7ms"));
    }

    /// A hit's line is the whole entry, and the scanner matched its lines one
    /// by one - so the isolated value comes from whichever line matched, not
    /// only from the header line the entry opens with.
    #[test]
    fn extract_reaches_a_match_on_a_continuation_line() {
        let entry = "14.07.2026 10:00:00 info (sql) - insert into @Employees values\n\
(1,'19859','Adhikari Amrita Kc'),\n\
(2,'19860','Adhikari Muna'),";

        // Plain pattern: the header line carries no match at all
        let matcher = Matcher::new(r"'(?<name>Adhikari [^']+)'", true, true).unwrap();
        assert_eq!(matcher.extract(entry).as_deref(), Some("Adhikari Amrita Kc"));

        // The builder's ALL form, whose `^` and `.` both stop at the first
        // newline when the entry is matched as one string
        let matcher = Matcher::new(r"^(?=.*(?<a>Adhikari))(?=.*(?<b>19860))", true, false).unwrap();
        assert_eq!(matcher.extract(entry).as_deref(), Some("Adhikari 19860"));
    }

    #[test]
    fn extract_ignores_empty_and_substring_queries() {
        assert_eq!(Matcher::new("", false, false).unwrap().extract("anything"), None);
        assert_eq!(
            Matcher::new("error", false, true).unwrap().extract("an error"),
            None
        );
    }
}

use crate::config::ConfigState;
use crate::fields::{FieldColumns, FieldMeta};
use crate::logger::LogState;
use crate::memcache::MemCache;
use crate::plugin::PluginState;
use crate::search::{self, CompactHit, Matcher, ScanFile, SearchHit, SearchRequest, SearchSpec};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

/// The last search's full result in compact form: no line text is held - pages,
/// exports and custom charts re-read it from the cached files on demand.
pub struct StoredSearch {
    /// The files the hits point into, indexed by `CompactHit::file`
    pub files: Vec<ScanFile>,
    pub hits: Vec<CompactHit>,
    /// Extracted values, indexed by `CompactHit::row`
    pub columns: FieldColumns,
    /// The columns of this result, in column order
    pub field_meta: Vec<FieldMeta>,
    pub group_names: Vec<String>,
    /// Context width of the search request, applied when pages are hydrated
    pub context_lines: usize,
}

impl Default for StoredSearch {
    fn default() -> Self {
        StoredSearch {
            files: Vec::new(),
            hits: Vec::new(),
            columns: FieldColumns::new(&[]),
            field_meta: Vec::new(),
            group_names: Vec::new(),
            context_lines: 0,
        }
    }
}

/// Holds the last search so the UI can lazy-load pages instead of receiving
/// (and rendering) everything at once.
pub struct SearchState(pub Mutex<StoredSearch>);

impl SearchState {
    pub fn new() -> Self {
        SearchState(Mutex::new(StoredSearch::default()))
    }
}

/// Set by `cancel_search`, read by the running scan between files and every few
/// thousand lines. `search_logs` rearms it at the start of each search.
pub struct SearchCancel(pub Arc<AtomicBool>);

impl SearchCancel {
    pub fn new() -> Self {
        SearchCancel(Arc::new(AtomicBool::new(false)))
    }
}

#[tauri::command]
pub fn cancel_search(cancel: tauri::State<'_, SearchCancel>) {
    cancel.0.store(true, Ordering::Relaxed);
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMeta {
    pub total_hits: usize,
    pub files_scanned: usize,
    pub lines_scanned: u64,
    pub duration_ms: u64,
    /// Named capture groups of the query, in pattern order
    pub group_names: Vec<String>,
    /// The columns this result carries, in the order hits report them. Which
    /// fields exist depends on the packs in play, so the UI takes its result
    /// columns from here rather than assuming any.
    pub fields: Vec<FieldMeta>,
    /// The configured hit cap cut the result short
    pub truncated: bool,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn search_logs(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    search_state: tauri::State<'_, SearchState>,
    log_state: tauri::State<'_, LogState>,
    cache: tauri::State<'_, Arc<MemCache>>,
    cancel: tauri::State<'_, SearchCancel>,
    request: SearchRequest,
    preview_limit: Option<usize>,
) -> Result<SearchMeta, String> {
    let (services, max_hits) = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        (guard.services.clone(), guard.search.max_hits)
    };
    // 0 in the config means unlimited
    let max_hits = match max_hits {
        0 => usize::MAX,
        hits => hits,
    };
    // A preview stops at the first few hits so the query can be refined cheaply;
    // the config cap still applies if it is somehow smaller
    let max_hits = match preview_limit {
        Some(limit) if limit > 0 => max_hits.min(limit),
        _ => max_hits,
    };
    let cache = Arc::clone(&cache);
    let cancel = Arc::clone(&cancel.0);
    cancel.store(false, Ordering::Relaxed);
    let context_lines = request.context_lines;
    // Snapshotted before the scan so a plugin reload mid-search cannot change
    // the columns the result is being built against
    let spec = SearchSpec::from_registry(&plugin_state.snapshot());

    // An invalid regex must fail before the stored result is given up below -
    // a typo in the query would otherwise cost the result it is refining.
    Matcher::new(&request.query, request.is_regex, request.case_sensitive)?;

    // Free the previous result before scanning, not when storing the new one:
    // dropping millions of hit strings takes seconds (it froze the UI at
    // "Sorting..."), and keeping the old hits until then doubles peak memory.
    // The UI blocks paging/sorting while a search runs, so nothing reads it.
    let old = {
        let mut guard = search_state.0.lock().map_err(|e| e.to_string())?;
        std::mem::take(&mut *guard)
    };
    tokio::task::spawn_blocking(move || drop(old));

    // Throttled: scans of many small files finish faster than the UI can paint.
    // Phase changes and the last file always get through.
    let progress_app = app.clone();
    let last_emit = std::sync::Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1));
    let result = tokio::task::spawn_blocking(move || {
        search::run(&request, &services, &spec, &cache, &cancel, max_hits, |progress| {
            let Ok(mut last) = last_emit.lock() else {
                return;
            };
            let pass = progress.phase != "scanning"
                || progress.files_done >= progress.files_total
                || last.elapsed().as_millis() >= 80;
            if pass {
                *last = std::time::Instant::now();
                progress_app.emit("search-progress", &progress).ok();
            }
        })
    })
    .await
    .map_err(|e| format!("Search task failed: {e}"))??;

    let meta = SearchMeta {
        total_hits: result.hits.len(),
        files_scanned: result.files_scanned,
        lines_scanned: result.lines_scanned,
        duration_ms: result.duration_ms,
        group_names: result.group_names.clone(),
        fields: result.field_meta.clone(),
        truncated: result.truncated,
    };

    let mut guard = search_state.0.lock().map_err(|e| e.to_string())?;
    *guard = StoredSearch {
        files: result.files,
        hits: result.hits,
        columns: result.columns,
        field_meta: result.field_meta,
        group_names: result.group_names,
        context_lines,
    };

    log_state.info(
        "search",
        &format!(
            "{} hits in {} files ({} lines) in {} ms{}",
            meta.total_hits,
            meta.files_scanned,
            meta.lines_scanned,
            meta.duration_ms,
            if preview_limit.is_some() { " (preview)" } else { "" }
        ),
    );

    // Chart windows read the same SearchState; without this they would keep
    // showing the previous search until reopened.
    let _ = app.emit("search-updated", &meta);
    Ok(meta)
}

/// Re-sorts the stored result in place; the UI drops its loaded pages and
/// re-fetches. Sorting backend-side keeps it correct across lazy loading -
/// only pages of the full ordering ever cross IPC.
#[tauri::command]
pub async fn sort_search_hits(
    search_state: tauri::State<'_, SearchState>,
    field: String,
    ascending: bool,
) -> Result<(), String> {
    let mut guard = search_state.0.lock().map_err(|e| e.to_string())?;
    let StoredSearch {
        files,
        hits,
        columns,
        field_meta,
        ..
    } = &mut *guard;
    match field.as_str() {
        "time" => hits.sort_by(|a, b| option_order(a.timestamp, b.timestamp)),
        "service" => hits.sort_by(|a, b| {
            files[a.file as usize]
                .service_id
                .cmp(&files[b.file as usize].service_id)
        }),
        "file" => hits.sort_by(|a, b| {
            files[a.file as usize]
                .name
                .cmp(&files[b.file as usize].name)
                .then(a.line_number.cmp(&b.line_number))
        }),
        // Anything else names one of the result's own columns, sorted as the kind
        // the pack declared - a number column ordered as numbers, not as text
        key => {
            let column = field_meta
                .iter()
                .position(|meta| meta.key == key)
                .ok_or_else(|| format!("Unknown sort field: {key}"))?;
            if field_meta[column].is_number() {
                hits.sort_by(|a, b| {
                    option_order_by(
                        columns.number(column, a.row),
                        columns.number(column, b.row),
                        |x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal),
                    )
                });
            } else {
                hits.sort_by(|a, b| {
                    option_order(columns.text(column, a.row), columns.text(column, b.row))
                });
            }
        }
    }
    if !ascending {
        hits.reverse();
    }
    Ok(())
}

/// Ascending, absent values last. A column a hit has no value for is not a small
/// value, so it sorts after everything that has one.
fn option_order<T: Ord>(a: Option<T>, b: Option<T>) -> std::cmp::Ordering {
    option_order_by(a, b, |x, y| x.cmp(y))
}

fn option_order_by<T>(
    a: Option<T>,
    b: Option<T>,
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => compare(&x, &y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// One page of hits, hydrated with line text and context re-read from the
/// cached files. Only the files this page touches are streamed, and nothing
/// stays in memory afterwards.
#[tauri::command]
pub async fn get_search_hits(
    search_state: tauri::State<'_, SearchState>,
    cache: tauri::State<'_, Arc<MemCache>>,
    offset: usize,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let guard = search_state.0.lock().map_err(|e| e.to_string())?;
    let end = (offset + limit).min(guard.hits.len());
    let start = offset.min(end);
    let page = &guard.hits[start..end];

    // Skeletons carry everything the index holds; the fetch fills in the text
    let mut out: Vec<SearchHit> = page
        .iter()
        .map(|hit| {
            let file = &guard.files[hit.file as usize];
            SearchHit {
                service_id: file.service_id.clone(),
                file: file.name.clone(),
                line_number: hit.line_number,
                timestamp: hit.timestamp,
                line: String::new(),
                context_before: Vec::new(),
                context_after: Vec::new(),
                fields: guard
                    .field_meta
                    .iter()
                    .enumerate()
                    .filter_map(|(column, meta)| {
                        let value = guard.columns.display(column, hit.row)?;
                        Some((meta.key.clone(), value))
                    })
                    .collect(),
                matched_groups: search::mask_to_groups(&guard.group_names, hit.matched_groups),
            }
        })
        .collect();

    let wanted: Vec<(u32, u64, usize)> = page
        .iter()
        .enumerate()
        .map(|(slot, hit)| (hit.file, hit.line_number, slot))
        .collect();
    search::fetch_hit_lines(
        &guard.files,
        &wanted,
        guard.context_lines,
        &cache,
        |slot, fetched| {
            out[slot].line = fetched.line;
            out[slot].context_before = fetched.context_before;
            out[slot].context_after = fetched.context_after;
        },
    )?;
    Ok(out)
}

/// One line of export text per stored hit: the whole line, or, with `extract`,
/// only the part the query isolates (the first capture group). `path` writes the
/// full result to that file the frontend picked; without it the (capped) text
/// comes back for the clipboard. The cap only guards the clipboard - a file gets
/// everything.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    /// Output only the matched part (first capture group) instead of the line
    pub extract: bool,
    /// Path to write to; `None` returns the text for the clipboard instead
    pub path: Option<String>,
    /// Clipboard line cap; ignored when writing a file
    pub max_lines: Option<usize>,
    /// Named groups the user toggled off in the legend; left out of `extract`
    #[serde(default)]
    pub exclude_groups: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    /// The joined text, for the clipboard path; `None` when a file was written
    pub text: Option<String>,
    /// The file that was written, if any
    pub saved_path: Option<String>,
    /// Lines actually in the output
    pub exported: usize,
    /// Lines the query produced before the cap
    pub total: usize,
    /// Whether the cap dropped some
    pub truncated: bool,
}

/// Joins one line of text per hit, in the stored (current sort) order.
/// `extract` skips hits whose line carries nothing to isolate; `max_lines` caps
/// the count but `total` still reports the full tally so the UI can say how
/// many were left out.
fn build_export(
    lines: &[Option<String>],
    matcher: &Matcher,
    extract: bool,
    exclude_groups: &[String],
    max_lines: Option<usize>,
) -> (String, usize, usize, bool) {
    let mut out = String::new();
    let mut exported = 0usize;
    let mut total = 0usize;
    for line in lines.iter().flatten() {
        let value = if extract {
            match matcher.extract_excluding(line, exclude_groups) {
                Some(v) => v,
                None => continue,
            }
        } else {
            line.clone()
        };
        total += 1;
        if max_lines.is_some_and(|cap| exported >= cap) {
            continue;
        }
        if exported > 0 {
            out.push('\n');
        }
        out.push_str(&value);
        exported += 1;
    }
    let truncated = max_lines.is_some_and(|cap| total > cap);
    (out, exported, total, truncated)
}

#[tauri::command]
pub async fn export_matches(
    search_state: tauri::State<'_, SearchState>,
    cache: tauri::State<'_, Arc<MemCache>>,
    request: ExportRequest,
) -> Result<ExportResult, String> {
    let guard = search_state.0.lock().map_err(|e| e.to_string())?;
    let matcher = Matcher::new(&request.query, request.is_regex, request.case_sensitive)?;

    // Re-read every hit's line, streaming one file at a time; memory holds only
    // the lines being exported, which the output would contain anyway
    let mut lines: Vec<Option<String>> = vec![None; guard.hits.len()];
    let wanted: Vec<(u32, u64, usize)> = guard
        .hits
        .iter()
        .enumerate()
        .map(|(slot, hit)| (hit.file, hit.line_number, slot))
        .collect();
    search::fetch_hit_lines(&guard.files, &wanted, 0, &cache, |slot, fetched| {
        lines[slot] = Some(fetched.line);
    })?;

    // A file gets every match; only the clipboard is capped
    let cap = if request.path.is_some() {
        None
    } else {
        request.max_lines
    };
    let (text, exported, total, truncated) =
        build_export(&lines, &matcher, request.extract, &request.exclude_groups, cap);

    if let Some(path) = request.path {
        std::fs::write(&path, text).map_err(|e| format!("Failed to write {path}: {e}"))?;
        Ok(ExportResult {
            text: None,
            saved_path: Some(path),
            exported,
            total,
            truncated: false,
        })
    } else {
        Ok(ExportResult {
            text: Some(text),
            saved_path: None,
            exported,
            total,
            truncated,
        })
    }
}

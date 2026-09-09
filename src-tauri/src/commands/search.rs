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
    /// What the UI (and the chart windows) were last told about this result.
    /// Narrowing rewrites its hit count and hands it back, so every reader ends
    /// up on the same numbers without the scan being re-described.
    pub meta: SearchMeta,
    /// Hit sets the narrowing steps replaced, oldest first. Undoing a step pops
    /// one back; the files and columns are never touched, so only the hit
    /// vector has to be kept.
    pub narrowed_from: Vec<Vec<CompactHit>>,
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
            meta: SearchMeta::default(),
            narrowed_from: Vec::new(),
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

#[derive(Debug, Clone, Default, Serialize)]
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

/// The result's columns as the UI should show them: a field none of the hits
/// carried a value for would be a column of blanks, so it is left out and the
/// table never renders it. Which fields a search fills in depends on the packs
/// and the services it touched, so this is per result rather than per pack.
fn visible_fields(
    hits: &[CompactHit],
    columns: &FieldColumns,
    metas: &[FieldMeta],
) -> Vec<FieldMeta> {
    let populated = columns.populated(hits.iter().map(|hit| hit.row));
    metas
        .iter()
        .enumerate()
        .filter(|(column, _)| populated.get(*column) == Some(&true))
        .map(|(_, meta)| meta.clone())
        .collect()
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
        fields: visible_fields(&result.hits, &result.columns, &result.field_meta),
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
        meta: meta.clone(),
        narrowed_from: Vec::new(),
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

/// One narrowing step: a second query run over the result already on screen
/// instead of over the cached files. Only the entries the current hits point at
/// are re-read, so refining a finished search costs a fraction of repeating it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NarrowRequest {
    pub query: String,
    pub is_regex: bool,
    pub case_sensitive: bool,
    /// Keep the hits that do *not* match instead of the ones that do
    #[serde(default)]
    pub invert: bool,
}

/// Filters the stored result down to the hits whose entry matches (or, inverted,
/// does not match) a second query, and remembers what it dropped so the step can
/// be undone. The hits keep their current order, their columns and their group
/// masks - this only removes rows, so pages, sorting, charts and exports all
/// carry on against the narrowed result without knowing about it.
#[tauri::command]
pub async fn narrow_search(
    app: tauri::AppHandle,
    search_state: tauri::State<'_, SearchState>,
    cache: tauri::State<'_, Arc<MemCache>>,
    log_state: tauri::State<'_, LogState>,
    cancel: tauri::State<'_, SearchCancel>,
    request: NarrowRequest,
) -> Result<SearchMeta, String> {
    if request.query.is_empty() {
        return Err("Type something to narrow the results with".into());
    }
    let matcher = Matcher::new(&request.query, request.is_regex, request.case_sensitive)?;
    let cancel = Arc::clone(&cancel.0);
    cancel.store(false, Ordering::Relaxed);

    let started = std::time::Instant::now();
    let mut guard = search_state.0.lock().map_err(|e| e.to_string())?;
    let before = guard.hits.len();

    // Every stored hit's entry is re-read (from RAM where the memory cache holds
    // the file, streamed from the zstd copy otherwise) and tested exactly as the
    // scan tested it. A hit whose line the file no longer has - the service was
    // re-synced shorter meanwhile - is never delivered, and an untestable hit
    // drops out rather than being kept on trust.
    let wanted: Vec<(u32, u64, usize)> = guard
        .hits
        .iter()
        .enumerate()
        .map(|(slot, hit)| (hit.file, hit.line_number, slot))
        .collect();
    let mut keep = vec![false; before];
    let mut done = 0usize;
    let mut kept = 0usize;
    let mut last_emit = std::time::Instant::now() - std::time::Duration::from_secs(1);
    search::fetch_hit_lines_cancellable(&guard.files, &wanted, 0, &cache, &cancel, |slot, entry| {
        let matched = matcher.matches_entry(&entry.line) != request.invert;
        keep[slot] = matched;
        done += 1;
        kept += usize::from(matched);
        // Throttled the same way the scan's progress is: re-reading is fast
        // enough that every hit would emit more events than the UI can paint
        if last_emit.elapsed().as_millis() >= 80 || done == before {
            last_emit = std::time::Instant::now();
            app.emit(
                "search-progress",
                &search::SearchProgress {
                    phase: "narrowing".into(),
                    files_done: done,
                    files_total: before,
                    hits: kept,
                    lines_scanned: 0,
                    current_file: String::new(),
                },
            )
            .ok();
        }
    })?;

    let previous = std::mem::take(&mut guard.hits);
    let mut narrowed = Vec::with_capacity(kept);
    narrowed.extend(
        previous
            .iter()
            .enumerate()
            .filter(|(slot, _)| keep[*slot])
            .map(|(_, hit)| *hit),
    );
    guard.hits = narrowed;
    guard.narrowed_from.push(previous);
    guard.meta.total_hits = guard.hits.len();
    // Narrowing can empty a column outright, and the columns keep the values of
    // the dropped hits, so what is still filled in has to be recounted
    guard.meta.fields = visible_fields(&guard.hits, &guard.columns, &guard.field_meta);
    let meta = guard.meta.clone();
    drop(guard);

    log_state.info(
        "search",
        &format!(
            "narrowed {} to {} hits ({}{}) in {} ms",
            before,
            meta.total_hits,
            if request.invert { "excluding " } else { "" },
            request.query,
            started.elapsed().as_millis()
        ),
    );
    let _ = app.emit("search-updated", &meta);
    Ok(meta)
}

/// Takes back the last narrowing step, restoring the hits it dropped.
#[tauri::command]
pub async fn undo_narrow(
    app: tauri::AppHandle,
    search_state: tauri::State<'_, SearchState>,
) -> Result<SearchMeta, String> {
    let mut guard = search_state.0.lock().map_err(|e| e.to_string())?;
    let previous = guard
        .narrowed_from
        .pop()
        .ok_or("These results have not been narrowed")?;
    guard.hits = previous;
    guard.meta.total_hits = guard.hits.len();
    // The restored hits bring their columns back with them
    guard.meta.fields = visible_fields(&guard.hits, &guard.columns, &guard.field_meta);
    let meta = guard.meta.clone();
    drop(guard);

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
    /// Named groups the user toggled off in the legend: left out of `extract`,
    /// and hits that only those groups matched are left out of the output
    /// entirely, matching what the results table shows
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

/// Joins one line of text per fetched hit, in the stored (current sort) order.
/// Hits the caller left unfetched (an empty slot) are already out of the export.
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

    // A hit whose every matched group is switched off is dropped whole, the same
    // way the results table hides it. Hits from a query without named groups
    // carry an empty mask and always stay.
    let excluded = search::group_mask(&guard.group_names, &request.exclude_groups);
    let kept = |mask: u32| mask == 0 || mask & !excluded != 0;

    // Re-read every kept hit's line, streaming one file at a time; memory holds
    // only the lines being exported, which the output would contain anyway.
    // Slots left unfetched stay `None` and fall out of the export.
    let mut lines: Vec<Option<String>> = vec![None; guard.hits.len()];
    let wanted: Vec<(u32, u64, usize)> = guard
        .hits
        .iter()
        .enumerate()
        .filter(|(_, hit)| kept(hit.matched_groups))
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

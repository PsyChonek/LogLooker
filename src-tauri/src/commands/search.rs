use crate::config::ConfigState;
use crate::logger::LogState;
use crate::memcache::MemCache;
use crate::search::{self, SearchHit, SearchRequest};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::Emitter;

/// The last search's full result: the hit list the UI lazy-loads pages of,
/// plus the query's capture group names, which charts group series by.
#[derive(Default)]
pub struct StoredSearch {
    pub hits: Vec<SearchHit>,
    pub group_names: Vec<String>,
}

/// Holds the last search so the UI can lazy-load pages instead of receiving
/// (and rendering) everything at once.
pub struct SearchState(pub Mutex<StoredSearch>);

impl SearchState {
    pub fn new() -> Self {
        SearchState(Mutex::new(StoredSearch::default()))
    }
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
}

#[tauri::command]
pub async fn search_logs(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, ConfigState>,
    search_state: tauri::State<'_, SearchState>,
    log_state: tauri::State<'_, LogState>,
    cache: tauri::State<'_, Arc<MemCache>>,
    request: SearchRequest,
) -> Result<SearchMeta, String> {
    let services = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard.services.clone()
    };
    let cache = Arc::clone(&cache);

    // Throttled: scans of many small files finish faster than the UI can paint.
    // Phase changes and the last file always get through.
    let progress_app = app.clone();
    let last_emit = std::sync::Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1));
    let result = tokio::task::spawn_blocking(move || {
        search::run(&request, &services, &cache, |progress| {
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
    };

    let mut guard = search_state.0.lock().map_err(|e| e.to_string())?;
    *guard = StoredSearch {
        hits: result.hits,
        group_names: result.group_names,
    };

    log_state.info(
        "search",
        &format!(
            "{} hits in {} files ({} lines) in {} ms",
            meta.total_hits, meta.files_scanned, meta.lines_scanned, meta.duration_ms
        ),
    );

    // Chart windows read the same SearchState; without this they would keep
    // showing the previous search until reopened.
    let _ = app.emit("search-updated", &meta);
    Ok(meta)
}

/// Re-sorts the stored result in place; the UI drops its loaded pages and
/// re-fetches. Sorting backend-side keeps it correct across lazy loading —
/// only pages of the full ordering ever cross IPC.
#[tauri::command]
pub fn sort_search_hits(
    search_state: tauri::State<'_, SearchState>,
    field: String,
    ascending: bool,
) -> Result<(), String> {
    let mut guard = search_state.0.lock().map_err(|e| e.to_string())?;
    let hits = &mut guard.hits;
    match field.as_str() {
        "time" => hits.sort_by(|a, b| match (a.timestamp, b.timestamp) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }),
        "service" => hits.sort_by(|a, b| a.service_id.cmp(&b.service_id)),
        "operation" => hits.sort_by(|a, b| match (&a.fields.operation, &b.fields.operation) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }),
        "file" => hits.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_number.cmp(&b.line_number))),
        other => return Err(format!("Unknown sort field: {other}")),
    }
    if !ascending {
        hits.reverse();
    }
    Ok(())
}

#[tauri::command]
pub fn get_search_hits(
    search_state: tauri::State<'_, SearchState>,
    offset: usize,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let guard = search_state.0.lock().map_err(|e| e.to_string())?;
    let end = (offset + limit).min(guard.hits.len());
    let start = offset.min(end);
    Ok(guard.hits[start..end].to_vec())
}

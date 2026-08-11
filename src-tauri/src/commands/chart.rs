use crate::cache;
use crate::chart::{self, ChartData, ChartRequest, CustomCapture};
use crate::commands::search::StoredSearch;
use crate::commands::SearchState;
use crate::config::{ConfigState, ServiceConfig};
use crate::memcache::MemCache;
use crate::search;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Aggregates the last search's full hit list. Charts therefore describe every
/// hit, not just the pages the table has lazy-loaded. Chart windows share this
/// state with the main window, so they chart the same search.
#[tauri::command]
pub async fn aggregate_search_hits(
    config_state: tauri::State<'_, ConfigState>,
    search_state: tauri::State<'_, SearchState>,
    cache: tauri::State<'_, Arc<MemCache>>,
    request: ChartRequest,
) -> Result<ChartData, String> {
    let services: Vec<ServiceConfig> = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard.services.clone()
    };
    let service_names: HashMap<String, String> = services
        .iter()
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    let stored = search_state.0.lock().map_err(|e| e.to_string())?;

    // A custom key/value comes from the hit lines, which the compact hits no
    // longer carry - re-read them, one file stream at a time, keeping only the
    // small (key, value) pair per hit
    let custom: Option<Vec<Option<CustomCapture>>> = if chart::needs_lines(&request) {
        let regex = chart::compile_custom(&request)?.ok_or("Custom grouping needs a regex")?;
        let mut captures: Vec<Option<CustomCapture>> = vec![None; stored.hits.len()];
        let wanted: Vec<(u32, u64, usize)> = stored
            .hits
            .iter()
            .enumerate()
            .map(|(slot, hit)| (hit.file, hit.line_number, slot))
            .collect();
        search::fetch_hit_lines(&stored.files, &wanted, 0, &cache, |slot, fetched| {
            captures[slot] = Some(chart::custom_capture(&regex, &fetched.line).unwrap_or((None, None)));
        })?;
        Some(captures)
    } else {
        None
    };

    let mut data = chart::aggregate(
        &stored.hits,
        &stored.files,
        &stored.columns,
        &stored.field_meta,
        custom.as_deref(),
        &request,
        &service_names,
        &stored.group_names,
    )?;
    data.log_offset_minutes = common_log_offset(&stored, &services);
    Ok(data)
}

/// The one offset the charted buckets can be labelled with. Hits from services on
/// different log clocks share a bucket by their naive time, so no single offset
/// describes the axis and the chart must stay in log time.
fn common_log_offset(stored: &StoredSearch, services: &[ServiceConfig]) -> Option<i32> {
    let mut used = vec![false; stored.files.len()];
    for hit in &stored.hits {
        used[hit.file as usize] = true;
    }
    let charted: HashSet<&str> = stored
        .files
        .iter()
        .zip(&used)
        .filter(|(_, used)| **used)
        .map(|(file, _)| file.service_id.as_str())
        .collect();
    let mut offsets = charted.into_iter().map(|id| {
        services
            .iter()
            .find(|service| service.id == id)
            .and_then(cache::service_offset_minutes)
    });
    // A service whose offset is unknown makes the whole axis unlabellable, and so
    // reads as a disagreement - which `None == Some(_)` already says.
    let first = offsets.next()?;
    if offsets.all(|offset| offset == first) {
        first
    } else {
        None
    }
}

/// Opens the chart of the current search in its own window. Like the raw
/// viewer, the parameters travel via an init script instead of the URL.
#[tauri::command]
pub async fn open_chart_window(
    app: tauri::AppHandle,
    query: String,
    request: ChartRequest,
) -> Result<(), String> {
    static NEXT_WINDOW: AtomicU64 = AtomicU64::new(1);

    let label = format!("chart-{}", NEXT_WINDOW.fetch_add(1, Ordering::Relaxed));
    let params = serde_json::json!({ "query": query, "request": request });
    let title = if query.is_empty() {
        "Chart - all lines - LogLooker".to_string()
    } else {
        format!("Chart - {query} - LogLooker")
    };
    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App("index.html".into()))
        .title(title)
        .inner_size(1000.0, 640.0)
        // Keep drag-drop behavior consistent with the main window (HTML5 drag works)
        .disable_drag_drop_handler()
        .additional_browser_args(crate::commands::WEBVIEW_BROWSER_ARGS)
        .initialization_script(format!("window.__CHART_VIEW__ = {params};"))
        .build()
        .map_err(|e| format!("Cannot open window: {e}"))?;
    Ok(())
}

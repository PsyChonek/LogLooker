use crate::cache;
use crate::chart::{self, ChartData, ChartRequest};
use crate::commands::SearchState;
use crate::config::{ConfigState, ServiceConfig};
use crate::search::SearchHit;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

/// Aggregates the last search's full hit list. Charts therefore describe every
/// hit, not just the pages the table has lazy-loaded. Chart windows share this
/// state with the main window, so they chart the same search.
#[tauri::command]
pub fn aggregate_search_hits(
    config_state: tauri::State<'_, ConfigState>,
    search_state: tauri::State<'_, SearchState>,
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
    let mut data = chart::aggregate(&stored.hits, &request, &service_names, &stored.group_names)?;
    data.log_offset_minutes = common_log_offset(&stored.hits, &services);
    Ok(data)
}

/// The one offset the charted buckets can be labelled with. Hits from services on
/// different log clocks share a bucket by their naive time, so no single offset
/// describes the axis and the chart must stay in log time.
fn common_log_offset(hits: &[SearchHit], services: &[ServiceConfig]) -> Option<i32> {
    let charted: HashSet<&str> = hits.iter().map(|hit| hit.service_id.as_str()).collect();
    let mut offsets = charted.into_iter().map(|id| {
        services
            .iter()
            .find(|service| service.id == id)
            .and_then(cache::service_offset_minutes)
    });
    // A service whose offset is unknown makes the whole axis unlabellable, and so
    // reads as a disagreement — which `None == Some(_)` already says.
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
        "Chart — all lines — LogLooker".to_string()
    } else {
        format!("Chart — {query} — LogLooker")
    };
    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App("index.html".into()))
        .title(title)
        .inner_size(1000.0, 640.0)
        .additional_browser_args(crate::commands::WEBVIEW_BROWSER_ARGS)
        .initialization_script(format!("window.__CHART_VIEW__ = {params};"))
        .build()
        .map_err(|e| format!("Cannot open window: {e}"))?;
    Ok(())
}

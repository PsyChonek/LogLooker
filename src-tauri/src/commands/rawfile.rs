use crate::cache::{self, CachedFileInfo};
use crate::config::{ConfigState, ServiceConfig};
use crate::logger::LogState;
use crate::memcache::MemCache;
use crate::rawfile::{self, OpenFile, RawFileInfo, RawSearchResult};
use crate::search::Matcher;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// At most this many matches cross IPC per raw-file search; the total is
/// still counted so the UI can report how many were left out.
const SEARCH_MATCH_LIMIT: usize = 20_000;

/// Files open in raw viewers, keyed by handle. Each viewer (modal or separate
/// window) owns one handle and closes it when it goes away. Entries are Arcs
/// so reads and searches run outside the lock.
pub struct RawFileState(pub Mutex<HashMap<u64, Arc<OpenFile>>>);

impl RawFileState {
    pub fn new() -> Self {
        RawFileState(Mutex::new(HashMap::new()))
    }
}

fn find_service(config_state: &ConfigState, service_id: &str) -> Result<ServiceConfig, String> {
    let guard = config_state.0.lock().map_err(|e| e.to_string())?;
    guard
        .services
        .iter()
        .find(|s| s.id == service_id)
        .cloned()
        .ok_or_else(|| format!("Unknown service: {service_id}"))
}

/// Every cached file of the given services, so the UI can offer them for raw
/// viewing without a search. Empty `service_ids` means all configured services.
#[tauri::command]
pub fn list_cached_files(
    config_state: tauri::State<'_, ConfigState>,
    service_ids: Vec<String>,
) -> Result<Vec<CachedFileInfo>, String> {
    let services = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard.services.clone()
    };

    let mut files = Vec::new();
    for service in services
        .iter()
        .filter(|s| service_ids.is_empty() || service_ids.contains(&s.id))
    {
        files.extend(cache::cached_files(service)?);
    }
    Ok(files)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedRawFile {
    pub handle: u64,
    #[serde(flatten)]
    pub info: RawFileInfo,
}

#[tauri::command]
pub async fn open_raw_file(
    config_state: tauri::State<'_, ConfigState>,
    raw_state: tauri::State<'_, RawFileState>,
    mem_cache: tauri::State<'_, Arc<MemCache>>,
    log_state: tauri::State<'_, LogState>,
    service_id: String,
    file: String,
) -> Result<OpenedRawFile, String> {
    static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

    let service = find_service(&config_state, &service_id)?;
    let cache = Arc::clone(&mem_cache);
    let opened = {
        let file = file.clone();
        tokio::task::spawn_blocking(move || rawfile::open(&service, &file, &cache))
            .await
            .map_err(|e| format!("Open task failed: {e}"))??
    };
    let info = opened.info();

    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    raw_state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .insert(handle, Arc::new(opened));
    log_state.info(
        "rawfile",
        &format!("opened {} ({} lines)", info.file, info.total_lines),
    );
    Ok(OpenedRawFile { handle, info })
}

fn open_file(raw_state: &RawFileState, handle: u64) -> Result<Arc<OpenFile>, String> {
    let guard = raw_state.0.lock().map_err(|e| e.to_string())?;
    guard.get(&handle).cloned().ok_or("No file is open".into())
}

#[tauri::command]
pub fn get_raw_lines(
    raw_state: tauri::State<'_, RawFileState>,
    handle: u64,
    offset: usize,
    limit: usize,
) -> Result<Vec<String>, String> {
    open_file(&raw_state, handle)?.lines(offset, limit)
}

/// Full-file search inside an open raw file. Blocking read of the whole
/// decompressed file; run off the main thread.
#[tauri::command]
pub async fn search_raw_file(
    raw_state: tauri::State<'_, RawFileState>,
    handle: u64,
    query: String,
    is_regex: bool,
    case_sensitive: bool,
) -> Result<RawSearchResult, String> {
    if query.is_empty() {
        return Ok(RawSearchResult::default());
    }
    let matcher = Matcher::new(&query, is_regex, case_sensitive)?;
    let file = open_file(&raw_state, handle)?;
    tokio::task::spawn_blocking(move || file.search(&matcher, SEARCH_MATCH_LIMIT))
        .await
        .map_err(|e| format!("Search task failed: {e}"))?
}

#[tauri::command]
pub fn close_raw_file(
    raw_state: tauri::State<'_, RawFileState>,
    handle: u64,
) -> Result<(), String> {
    let mut guard = raw_state.0.lock().map_err(|e| e.to_string())?;
    if let Some(open) = guard.remove(&handle) {
        open.cleanup();
    }
    Ok(())
}

#[tauri::command]
pub fn delete_cached_file(
    config_state: tauri::State<'_, ConfigState>,
    mem_cache: tauri::State<'_, Arc<MemCache>>,
    log_state: tauri::State<'_, LogState>,
    service_id: String,
    file: String,
) -> Result<(), String> {
    let service = find_service(&config_state, &service_id)?;
    let dir = cache::service_dir_path(&service.environment, &service.name)?;
    cache::delete_cached_file(&service, &file)?;
    // The RAM copy would otherwise sit there until something evicted it
    mem_cache.forget(&dir.join(format!("{file}.zst")))?;
    log_state.info("cache", &format!("deleted {file} of {}", service.name));
    Ok(())
}

/// Decompresses a cached file to a path the user picked (via a save dialog on the
/// frontend) and returns the bytes written. Runs the decompression off the main
/// thread so a large file does not block the UI.
#[tauri::command]
pub async fn export_cached_file(
    config_state: tauri::State<'_, ConfigState>,
    log_state: tauri::State<'_, LogState>,
    service_id: String,
    file: String,
    target_path: String,
) -> Result<u64, String> {
    let service = find_service(&config_state, &service_id)?;
    let bytes = {
        let file = file.clone();
        let target_path = target_path.clone();
        tokio::task::spawn_blocking(move || {
            cache::export_cached_file(&service, &file, std::path::Path::new(&target_path))
        })
        .await
        .map_err(|e| format!("Export task failed: {e}"))??
    };
    log_state.info("cache", &format!("exported {file} to {target_path}"));
    Ok(bytes)
}

/// Opens Windows Explorer with the cached file selected.
#[tauri::command]
pub fn reveal_cached_file(
    config_state: tauri::State<'_, ConfigState>,
    service_id: String,
    file: String,
) -> Result<(), String> {
    let service = find_service(&config_state, &service_id)?;
    let dir = cache::service_dir_path(&service.environment, &service.name)?;
    let path = dir.join(format!("{file}.zst"));
    if !path.exists() {
        return Err(format!("{file} is not cached for {}", service.name));
    }
    tauri_plugin_opener::reveal_item_in_dir(&path).map_err(|e| e.to_string())
}

/// Opens the cache directory (of one service, or the whole cache root) in the
/// system file manager.
#[tauri::command]
pub fn open_cache_dir(
    config_state: tauri::State<'_, ConfigState>,
    service_id: Option<String>,
) -> Result<(), String> {
    let dir = match service_id {
        Some(id) => {
            let service = find_service(&config_state, &id)?;
            cache::service_dir_path(&service.environment, &service.name)?
        }
        None => cache::cache_root()?,
    };
    if !dir.exists() {
        return Err("Cache directory does not exist yet".into());
    }
    tauri_plugin_opener::open_path(dir, None::<&str>).map_err(|e| e.to_string())
}

/// Opens the raw viewer for a file in its own window. The parameters travel
/// via an init script (`window.__RAW_VIEW__`) instead of the URL, so the SPA
/// needs no extra route.
#[tauri::command]
pub async fn open_raw_window(
    app: tauri::AppHandle,
    service_id: String,
    service_name: String,
    file: String,
    line: Option<u64>,
    query: Option<String>,
    is_regex: bool,
    case_sensitive: bool,
) -> Result<(), String> {
    static NEXT_WINDOW: AtomicU64 = AtomicU64::new(1);

    let label = format!("raw-{}", NEXT_WINDOW.fetch_add(1, Ordering::Relaxed));
    let params = serde_json::json!({
        "serviceId": service_id,
        "serviceName": service_name,
        "file": file,
        "line": line,
        "query": query,
        "isRegex": is_regex,
        "caseSensitive": case_sensitive,
    });
    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App("index.html".into()))
        .title(format!("{file} - {service_name} - LogLooker"))
        .inner_size(1100.0, 700.0)
        // Tauri's native drag-drop handler swallows HTML5 drag events on Windows,
        // which breaks the column drag-to-reorder in DataTable
        .disable_drag_drop_handler()
        .additional_browser_args(crate::commands::WEBVIEW_BROWSER_ARGS)
        .initialization_script(format!("window.__RAW_VIEW__ = {params};"))
        .build()
        .map_err(|e| format!("Cannot open window: {e}"))?;
    Ok(())
}

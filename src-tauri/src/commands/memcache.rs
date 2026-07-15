use crate::config::{self, ConfigState, MemoryCacheConfig};
use crate::logger::LogState;
use crate::memcache::{MemCache, MemCacheStats};
use std::sync::Arc;

#[tauri::command]
pub fn memory_cache_stats(
    cache: tauri::State<'_, Arc<MemCache>>,
) -> Result<MemCacheStats, String> {
    cache.stats()
}

/// Persists the setting and applies it to the live cache: turning it off, or
/// shrinking it, frees the memory immediately rather than at the next search.
#[tauri::command]
pub fn set_memory_cache(
    config_state: tauri::State<'_, ConfigState>,
    cache: tauri::State<'_, Arc<MemCache>>,
    log_state: tauri::State<'_, LogState>,
    enabled: bool,
    max_mb: u64,
) -> Result<MemCacheStats, String> {
    let settings = MemoryCacheConfig { enabled, max_mb }.normalized();
    let updated = {
        let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard.memory_cache = settings;
        guard.clone()
    };
    config::save_config(&updated)?;
    cache.configure(settings.enabled, settings.max_mb)?;

    log_state.info(
        "memcache",
        &match settings.enabled {
            true => format!("enabled, cap {} MB", settings.max_mb),
            false => "disabled".to_string(),
        },
    );
    cache.stats()
}

#[tauri::command]
pub fn clear_memory_cache(cache: tauri::State<'_, MemCache>) -> Result<MemCacheStats, String> {
    cache.clear()?;
    cache.stats()
}

use crate::config::{self, AppConfig, ConfigState, SavedQuery};
use crate::memcache::MemCache;
use std::sync::Arc;

#[tauri::command]
pub fn get_config(state: tauri::State<'_, ConfigState>) -> Result<AppConfig, String> {
    state
        .0
        .lock()
        .map(|config| config.clone())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn update_config(
    state: tauri::State<'_, ConfigState>,
    cache: tauri::State<'_, Arc<MemCache>>,
    mut config: AppConfig,
) -> Result<(), String> {
    config.memory_cache = config.memory_cache.normalized();
    config::save_config(&config)?;

    // Saving a query also writes back whatever memory-cache settings the caller
    // held; re-applying them here keeps the live cache and the config in step.
    let settings = config.memory_cache;
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    *guard = config;
    drop(guard);
    cache.configure(settings.enabled, settings.max_mb)
}

#[tauri::command]
pub fn get_preset_queries() -> Vec<SavedQuery> {
    config::preset_queries()
}

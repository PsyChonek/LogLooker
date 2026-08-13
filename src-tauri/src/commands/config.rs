use crate::config::{self, AppConfig, ConfigState, SavedQuery};
use crate::logger::LogState;
use crate::memcache::MemCache;
use crate::plugin::{EnvironmentDef, PluginState};
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
    config.search = config.search.normalized();
    config::save_config(&config)?;

    // Saving a query also writes back whatever memory-cache settings the caller
    // held; re-applying them here keeps the live cache and the config in step.
    let settings = config.memory_cache;
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    *guard = config;
    drop(guard);
    cache.configure(settings.enabled, settings.max_mb)
}

/// Opens the directory containing config.json and other user-owned app files.
#[tauri::command]
pub fn open_config_dir(app: tauri::AppHandle) -> Result<(), String> {
    let dir = config::config_dir()?;
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| format!("Cannot open settings folder: {e}"))
}

/// Adds an environment of the user's own, named however they like. Environments
/// are not a plugin's property: a service of any plugin can be put in any of
/// them. Returns the user-created list, the packs' own being fixed.
#[tauri::command]
pub fn add_environment(
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    log_state: tauri::State<'_, LogState>,
    name: String,
) -> Result<Vec<EnvironmentDef>, String> {
    let label = name.trim().to_string();
    if label.is_empty() {
        return Err("Environment name cannot be empty".into());
    }
    let id = config::environment_id(&label)?;

    let registry = plugin_state.snapshot();
    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    if registry.has_environment(&id) || guard.environments.iter().any(|e| e.id == id) {
        return Err(format!("Environment \"{id}\" already exists"));
    }

    guard.environments.push(EnvironmentDef {
        id: id.clone(),
        label,
    });
    config::save_config(&guard)?;
    log_state.info("config", &format!("Added environment {id}"));
    Ok(guard.environments.clone())
}

/// Removes an environment the user added. A pack's own environment is not the
/// user's to delete, and one still holding services is kept so its cached logs
/// stay reachable.
#[tauri::command]
pub fn remove_environment(
    config_state: tauri::State<'_, ConfigState>,
    log_state: tauri::State<'_, LogState>,
    environment_id: String,
) -> Result<Vec<EnvironmentDef>, String> {
    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    if !guard.environments.iter().any(|e| e.id == environment_id) {
        return Err(format!(
            "\"{environment_id}\" comes from a plugin and cannot be removed"
        ));
    }
    let used = guard
        .services
        .iter()
        .filter(|s| s.environment == environment_id)
        .count();
    if used > 0 {
        return Err(format!(
            "{environment_id} still holds {used} service(s) - remove them first"
        ));
    }

    guard.environments.retain(|e| e.id != environment_id);
    config::save_config(&guard)?;
    log_state.info("config", &format!("Removed environment {environment_id}"));
    Ok(guard.environments.clone())
}

/// Presets come from the loaded packs - what is worth searching for depends on
/// what the logs look like, which only a pack knows.
#[tauri::command]
pub fn get_preset_queries(plugin_state: tauri::State<'_, PluginState>) -> Vec<SavedQuery> {
    config::preset_queries(&plugin_state.snapshot())
}

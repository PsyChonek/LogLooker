pub mod auth;
pub mod cache;
pub mod chart;
mod commands;
pub mod config;
pub mod discovery;
pub mod fields;
pub mod growing;
pub mod kudu;
pub mod locations;
mod logger;
pub mod memcache;
mod models;
pub mod plugin;
pub mod rawfile;
pub mod search;
pub mod source;

use auth::TokenState;
use config::ConfigState;
use logger::LogState;
use memcache::MemCache;
use plugin::PluginState;
use std::sync::{Arc, Mutex};

#[tauri::command]
fn get_logs(log_state: tauri::State<'_, LogState>) -> Vec<models::LogEntry> {
    log_state.0.lock().map(|l| l.clone()).unwrap_or_default()
}

#[tauri::command]
fn clear_logs(log_state: tauri::State<'_, LogState>) {
    if let Ok(mut logs) = log_state.0.lock() {
        logs.clear();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The registry is built from the config's disabled list, so config is read
    // first and only then reconciled against the packs that actually loaded.
    let mut config = config::load_config();
    let registry = plugin::load(&config.disabled_packs);
    if config::migrate_for_packs(&mut config, &registry) {
        config::save_config(&config).ok();
    }
    let mem_cache = Arc::new(MemCache::new(
        config.memory_cache.enabled,
        config.memory_cache.max_mb,
    ));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|_app| {
            // Leftovers from raw-view windows that never got to clean up
            rawfile::clear_temp_dir();
            Ok(())
        })
        .manage(LogState::new())
        .manage(TokenState::new())
        .manage(commands::SearchState::new())
        .manage(commands::SearchCancel::new())
        .manage(commands::SyncCancel::new())
        .manage(commands::RawFileState::new())
        .manage(mem_cache)
        .manage(PluginState::new(registry))
        .manage(ConfigState(Mutex::new(config)))
        .invoke_handler(tauri::generate_handler![
            get_logs,
            clear_logs,
            commands::get_config,
            commands::update_config,
            commands::open_config_dir,
            commands::get_preset_queries,
            commands::add_environment,
            commands::remove_environment,
            commands::get_plugins,
            commands::get_environments,
            commands::get_fields,
            commands::reload_plugins,
            commands::set_pack_enabled,
            commands::open_plugins_dir,
            commands::refresh_services,
            commands::add_service,
            commands::remove_service,
            commands::set_endpoint,
            commands::set_location,
            commands::detect_location,
            commands::sync_services,
            commands::download_services,
            commands::cancel_sync,
            commands::cache_status_all,
            commands::search_logs,
            commands::cancel_search,
            commands::get_search_hits,
            commands::export_matches,
            commands::sort_search_hits,
            commands::aggregate_search_hits,
            commands::open_chart_window,
            commands::list_cached_files,
            commands::open_raw_file,
            commands::get_raw_lines,
            commands::search_raw_file,
            commands::close_raw_file,
            commands::delete_cached_file,
            commands::export_cached_file,
            commands::reveal_cached_file,
            commands::open_cache_dir,
            commands::open_raw_window,
            commands::memory_cache_stats,
            commands::set_memory_cache,
            commands::clear_memory_cache,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

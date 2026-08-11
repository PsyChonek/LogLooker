use crate::config::{self, ConfigState};
use crate::logger::LogState;
use crate::plugin::{self, EnvironmentDef, FieldInfo, PluginState, PluginsInfo};

/// Everything that is loaded, plus what failed to load and why - a pack with a
/// typo has to be visible as a broken pack, not as a pack that is not there.
#[tauri::command]
pub fn get_plugins(
    plugin_state: tauri::State<'_, PluginState>,
    config_state: tauri::State<'_, ConfigState>,
) -> Result<PluginsInfo, String> {
    let registry = plugin_state.snapshot();
    let disabled = config_state
        .0
        .lock()
        .map(|c| c.disabled_packs.clone())
        .map_err(|e| e.to_string())?;

    Ok(PluginsInfo {
        packs: registry.packs.iter().map(|p| p.info()).collect(),
        errors: registry.errors.clone(),
        disabled,
        plugins_dir: plugin::plugins_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_default(),
        bundled: plugin::bundled_ids(),
    })
}

/// Every loaded pack's environments plus the user's own - what the environment
/// selector shows.
#[tauri::command]
pub fn get_environments(
    plugin_state: tauri::State<'_, PluginState>,
    config_state: tauri::State<'_, ConfigState>,
) -> Result<Vec<EnvironmentDef>, String> {
    let registry = plugin_state.snapshot();
    let guard = config_state.0.lock().map_err(|e| e.to_string())?;
    Ok(config::environments(&guard, &registry))
}

/// Every field the loaded packs extract, in the order search results column them.
#[tauri::command]
pub fn get_fields(plugin_state: tauri::State<'_, PluginState>) -> Vec<FieldInfo> {
    let registry = plugin_state.snapshot();
    registry
        .fields()
        .into_iter()
        .map(|f| FieldInfo {
            key: f.key.clone(),
            label: f.label.clone(),
            pack_id: f
                .key
                .split_once('.')
                .map(|(pack, _)| pack.to_string())
                .unwrap_or_default(),
            kind: f.kind,
        })
        .collect()
}

/// Re-reads the plugins directory. Cheap enough to offer as a button, which
/// beats restarting the app after every edit to a pack being written.
#[tauri::command]
pub fn reload_plugins(
    plugin_state: tauri::State<'_, PluginState>,
    config_state: tauri::State<'_, ConfigState>,
    log_state: tauri::State<'_, LogState>,
) -> Result<PluginsInfo, String> {
    let disabled = config_state
        .0
        .lock()
        .map(|c| c.disabled_packs.clone())
        .map_err(|e| e.to_string())?;

    let registry = plugin::load(&disabled);
    let (loaded, failed) = (registry.packs.len(), registry.errors.len());

    // Services may have been written before the packs they belong to existed, and
    // a reload can be what finally brings that pack in
    {
        let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
        if config::migrate_for_packs(&mut guard, &registry) {
            config::save_config(&guard)?;
        }
    }
    plugin_state.replace(registry);

    match failed {
        0 => log_state.info("plugins", &format!("Reloaded {loaded} plugin(s)")),
        _ => log_state.warn(
            "plugins",
            &format!("Reloaded {loaded} plugin(s), {failed} failed to load"),
        ),
    }
    get_plugins(plugin_state, config_state)
}

/// Turns a pack on or off. A disabled pack is not loaded at all, so it costs
/// nothing at scan time; services that belong to it stay in config and simply
/// report that their plugin is missing.
#[tauri::command]
pub fn set_pack_enabled(
    plugin_state: tauri::State<'_, PluginState>,
    config_state: tauri::State<'_, ConfigState>,
    log_state: tauri::State<'_, LogState>,
    pack_id: String,
    enabled: bool,
) -> Result<PluginsInfo, String> {
    {
        let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard.disabled_packs.retain(|id| id != &pack_id);
        if !enabled {
            guard.disabled_packs.push(pack_id.clone());
        }
        config::save_config(&guard)?;
    }
    log_state.info(
        "plugins",
        &format!("{pack_id} {}", if enabled { "enabled" } else { "disabled" }),
    );
    reload_plugins(plugin_state, config_state, log_state)
}

/// Opens the plugins directory in the file manager, so "where do I put a pack"
/// has an answer that does not involve typing a path.
#[tauri::command]
pub fn open_plugins_dir(app: tauri::AppHandle) -> Result<(), String> {
    let dir = plugin::plugins_dir()?;
    tauri_plugin_opener::OpenerExt::opener(&app)
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| format!("Cannot open plugins folder: {e}"))
}

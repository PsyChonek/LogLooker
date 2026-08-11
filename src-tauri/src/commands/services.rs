use crate::auth::{self, TokenState};
use crate::config::{self, ConfigState, ServiceConfig, ServiceSource};
use crate::discovery;
use crate::locations;
use crate::logger::LogState;
use crate::plugin::{Pack, PluginState};
use crate::source::LogSource;
use std::sync::Arc;

/// Looks up the pack a service belongs to, with a message that names what is
/// missing - a service whose pack was removed or disabled is kept in config, so
/// this is a state the user can actually be in.
fn pack_for(registry: &crate::plugin::Registry, service: &ServiceConfig) -> Result<Arc<Pack>, String> {
    registry.pack(&service.pack_id).cloned().ok_or_else(|| {
        format!(
            "{}: its plugin \"{}\" is not loaded - enable or reinstall it in Plugins",
            service.id, service.pack_id
        )
    })
}

/// Builds the transport for one service, acquiring an Azure token only if the
/// pack's source actually needs one.
pub(crate) async fn source_for(
    pack: &Pack,
    service: &ServiceConfig,
    token_state: &TokenState,
) -> Result<LogSource, String> {
    let endpoint = service.endpoint.clone().ok_or_else(|| {
        format!(
            "{}: no endpoint set - set it in the Services table",
            service.id
        )
    })?;
    let token = match LogSource::needs_token(&pack.manifest.source) {
        true => Some(auth::get_token(token_state).await?),
        false => None,
    };
    LogSource::new(&pack.manifest.source, &endpoint, token)
}

/// Runs one pack's discovery and merges the result into the configured service
/// list. Existing entries keep a hand-edited endpoint and a detected location;
/// services the user added manually are never touched.
#[tauri::command]
pub async fn refresh_services(
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    log_state: tauri::State<'_, LogState>,
    pack_id: String,
) -> Result<Vec<ServiceConfig>, String> {
    let registry = plugin_state.snapshot();
    let pack = registry
        .pack(&pack_id)
        .cloned()
        .ok_or_else(|| format!("Unknown plugin: {pack_id}"))?;

    let discovered = discovery::discover(&pack).await?;
    let services: Vec<ServiceConfig> = discovered
        .into_iter()
        .map(|s| ServiceConfig {
            id: config::service_id(&s.environment, &s.name),
            name: s.name,
            environment: s.environment,
            pack_id: pack_id.clone(),
            endpoint: s.endpoint,
            endpoint_manual: false,
            location: s.location,
            source: ServiceSource::Scraped,
        })
        .collect();

    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    for service in services {
        if let Some(existing) = guard.services.iter_mut().find(|s| s.id == service.id) {
            // Leave a hand-edited endpoint alone; otherwise take discovery's
            // value, but never wipe an existing one with a missing result
            if !existing.endpoint_manual && service.endpoint.is_some() {
                existing.endpoint = service.endpoint;
            }
            // Fill in the location discovery now points at, but leave a detected
            // or user-overridden one alone
            if existing.location.is_none() {
                existing.location = service.location;
            }
        } else {
            guard.services.push(service);
        }
    }
    config::sort_services(&mut guard);
    config::save_config(&guard)?;
    log_state.info(
        "services",
        &format!(
            "{}: service list refreshed, {} services configured",
            pack.manifest.name,
            guard.services.len()
        ),
    );
    Ok(guard.services.clone())
}

/// Adds a service discovery does not list. Returns the full list so the UI does
/// not have to re-fetch it.
#[tauri::command]
pub fn add_service(
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    log_state: tauri::State<'_, LogState>,
    name: String,
    environment: String,
    pack_id: String,
    endpoint: String,
) -> Result<Vec<ServiceConfig>, String> {
    let registry = plugin_state.snapshot();
    let pack = registry
        .pack(&pack_id)
        .cloned()
        .ok_or_else(|| format!("Unknown plugin: {pack_id}"))?;

    let name = config::validate_service_name(&name)?;
    let endpoint = config::normalize_endpoint(&pack.manifest.source, &endpoint)?;
    let id = config::service_id(&environment, &name);

    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    // Environments are the app's, not a plugin's: any service can go in any of
    // them, so all that is checked is that the environment exists at all
    if !registry.has_environment(&environment)
        && !guard.environments.iter().any(|e| e.id == environment)
    {
        return Err(format!("Unknown environment: \"{environment}\""));
    }
    if guard.services.iter().any(|s| s.id == id) {
        return Err(format!("Service \"{id}\" already exists"));
    }
    guard.services.push(ServiceConfig {
        id: id.clone(),
        name,
        environment,
        pack_id,
        endpoint: Some(endpoint),
        endpoint_manual: true,
        location: None,
        source: ServiceSource::Manual,
    });
    config::sort_services(&mut guard);
    config::save_config(&guard)?;
    log_state.info("services", &format!("Added service {id}"));
    Ok(guard.services.clone())
}

/// Removes a service the user added or a pack seeded. Discovered services are
/// owned by their pack's refresh, so they would come back on the next one.
/// The cached logs are left on disk.
#[tauri::command]
pub fn remove_service(
    config_state: tauri::State<'_, ConfigState>,
    log_state: tauri::State<'_, LogState>,
    service_id: String,
) -> Result<Vec<ServiceConfig>, String> {
    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    let service = guard
        .services
        .iter()
        .find(|s| s.id == service_id)
        .ok_or_else(|| format!("Unknown service: {service_id}"))?;
    if service.source == ServiceSource::Scraped {
        return Err(format!(
            "{service_id} comes from its plugin's discovery and cannot be removed"
        ));
    }

    guard.services.retain(|s| s.id != service_id);
    config::save_config(&guard)?;
    log_state.info("services", &format!("Removed service {service_id}"));
    Ok(guard.services.clone())
}

/// Sets or clears (empty string) a service's endpoint, normalized the way its
/// pack's transport wants it.
#[tauri::command]
pub fn set_endpoint(
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    service_id: String,
    endpoint: String,
) -> Result<Vec<ServiceConfig>, String> {
    let registry = plugin_state.snapshot();
    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    let service = guard
        .services
        .iter()
        .find(|s| s.id == service_id)
        .ok_or_else(|| format!("Unknown service: {service_id}"))?;
    let pack = pack_for(&registry, service)?;

    let endpoint = match endpoint.trim() {
        "" => None,
        value => Some(config::normalize_endpoint(&pack.manifest.source, value)?),
    };

    let service = guard
        .services
        .iter_mut()
        .find(|s| s.id == service_id)
        .ok_or_else(|| format!("Unknown service: {service_id}"))?;
    service.endpoint = endpoint;
    // A hand-set (or cleared) endpoint is the user's; a refresh must not overwrite it
    service.endpoint_manual = true;
    config::save_config(&guard)?;
    Ok(guard.services.clone())
}

/// Overrides which of the pack's log locations a service reads, or clears the
/// choice (empty string) so the next sync detects it again.
#[tauri::command]
pub fn set_location(
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    service_id: String,
    location: String,
) -> Result<Vec<ServiceConfig>, String> {
    let registry = plugin_state.snapshot();
    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    let service = guard
        .services
        .iter()
        .find(|s| s.id == service_id)
        .ok_or_else(|| format!("Unknown service: {service_id}"))?;
    let pack = pack_for(&registry, service)?;

    let location = match location.trim() {
        "" => None,
        id => {
            if pack.location(id).is_none() {
                return Err(format!(
                    "{} has no log location \"{id}\"",
                    pack.manifest.name
                ));
            }
            Some(id.to_string())
        }
    };

    let service = guard
        .services
        .iter_mut()
        .find(|s| s.id == service_id)
        .ok_or_else(|| format!("Unknown service: {service_id}"))?;
    service.location = location;
    config::save_config(&guard)?;
    Ok(guard.services.clone())
}

/// Probes the service's source for the pack's log locations and stores the one
/// that matched.
#[tauri::command]
pub async fn detect_location(
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    token_state: tauri::State<'_, TokenState>,
    log_state: tauri::State<'_, LogState>,
    service_id: String,
) -> Result<Option<String>, String> {
    let registry = plugin_state.snapshot();
    let (service, pack) = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        let service = guard
            .services
            .iter()
            .find(|s| s.id == service_id)
            .ok_or_else(|| format!("Unknown service: {service_id}"))?
            .clone();
        let pack = pack_for(&registry, &service)?;
        (service, pack)
    };

    let source = source_for(&pack, &service, &token_state).await?;
    let location = locations::detect(&source, &pack).await?;

    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    if let Some(found) = guard.services.iter_mut().find(|s| s.id == service_id) {
        found.location = location.clone();
    }
    config::save_config(&guard)?;

    match &location {
        Some(id) => log_state.info(
            "locations",
            &format!("{service_id}: reads its logs from \"{id}\""),
        ),
        None => log_state.warn(
            "locations",
            &format!(
                "{service_id}: none of {}'s log locations matched",
                pack.manifest.name
            ),
        ),
    }
    Ok(location)
}

use crate::auth::{self, TokenState};
use crate::config::{self, ConfigState, ServiceConfig, ServiceSource};
use crate::kudu::KuduClient;
use crate::logger::LogState;
use crate::profiles::{self, ProfileKind};
use crate::scraper::{self, Environment};

/// Scrapes status.example.com and merges the result into the configured service
/// list. Existing entries keep their detected/overridden profile; services the
/// user added manually are never removed.
#[tauri::command]
pub async fn refresh_services(
    config_state: tauri::State<'_, ConfigState>,
    log_state: tauri::State<'_, LogState>,
) -> Result<Vec<ServiceConfig>, String> {
    let scraped = scraper::scrape_services().await?;
    let services = scraped
        .into_iter()
        .map(|s| ServiceConfig {
            id: config::service_id(s.environment, &s.name),
            name: s.name,
            environment: s.environment,
            kudu_url: s.kudu_url,
            kudu_url_manual: false,
            profile: s.profile,
            source: ServiceSource::Scraped,
        })
        .collect::<Vec<_>>();

    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    for service in services {
        if let Some(existing) = guard.services.iter_mut().find(|s| s.id == service.id) {
            // Leave a hand-edited URL alone; otherwise take the scrape's value,
            // but never wipe an existing one with a missing scrape result
            if !existing.kudu_url_manual && service.kudu_url.is_some() {
                existing.kudu_url = service.kudu_url;
            }
            // Fill in the profile the status page now tells us, but leave a
            // detected or user-overridden one alone
            if existing.profile.is_none() {
                existing.profile = service.profile;
            }
        } else {
            guard.services.push(service);
        }
    }
    config::sort_services(&mut guard);
    config::save_config(&guard)?;
    log_state.info(
        "services",
        &format!("Service list refreshed: {} services", guard.services.len()),
    );
    Ok(guard.services.clone())
}

/// Adds a service the status page does not list. Returns the full list so the
/// UI does not have to re-fetch it.
#[tauri::command]
pub fn add_service(
    config_state: tauri::State<'_, ConfigState>,
    log_state: tauri::State<'_, LogState>,
    name: String,
    environment: Environment,
    kudu_url: String,
) -> Result<Vec<ServiceConfig>, String> {
    let name = config::validate_service_name(&name)?;
    let kudu_url = config::normalize_kudu_url(&kudu_url)?;
    let id = config::service_id(environment, &name);

    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    if guard.services.iter().any(|s| s.id == id) {
        return Err(format!("Service \"{id}\" already exists"));
    }
    guard.services.push(ServiceConfig {
        id: id.clone(),
        name,
        environment,
        kudu_url: Some(kudu_url),
        kudu_url_manual: true,
        profile: None,
        source: ServiceSource::Manual,
    });
    config::sort_services(&mut guard);
    config::save_config(&guard)?;
    log_state.info("services", &format!("Added service {id}"));
    Ok(guard.services.clone())
}

/// Removes a service the user added or a built-in. Scraped services are owned by
/// the status page, so they would come back on the next refresh anyway.
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
            "{service_id} comes from the status page and cannot be removed"
        ));
    }

    guard.services.retain(|s| s.id != service_id);
    config::save_config(&guard)?;
    log_state.info("services", &format!("Removed service {service_id}"));
    Ok(guard.services.clone())
}

/// Sets or clears (empty string) a service's Kudu URL.
#[tauri::command]
pub fn set_kudu_url(
    config_state: tauri::State<'_, ConfigState>,
    service_id: String,
    kudu_url: String,
) -> Result<Vec<ServiceConfig>, String> {
    let kudu_url = match kudu_url.trim() {
        "" => None,
        url => Some(config::normalize_kudu_url(url)?),
    };

    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    let service = guard
        .services
        .iter_mut()
        .find(|s| s.id == service_id)
        .ok_or_else(|| format!("Unknown service: {service_id}"))?;
    service.kudu_url = kudu_url;
    // A hand-set (or cleared) URL is the user's; refresh must not overwrite it
    service.kudu_url_manual = true;
    config::save_config(&guard)?;
    Ok(guard.services.clone())
}

/// Probes the service's Kudu VFS and stores the detected log profile in config.
#[tauri::command]
pub async fn detect_profile(
    config_state: tauri::State<'_, ConfigState>,
    token_state: tauri::State<'_, TokenState>,
    log_state: tauri::State<'_, LogState>,
    service_id: String,
) -> Result<Option<ProfileKind>, String> {
    let kudu_url = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard
            .services
            .iter()
            .find(|s| s.id == service_id)
            .ok_or_else(|| format!("Unknown service: {service_id}"))?
            .kudu_url
            .clone()
            .ok_or_else(|| format!("{service_id}: Kudu URL not set — set it in the table"))?
    };

    let token = auth::get_token(&token_state).await?;
    let client = KuduClient::new(&kudu_url, token)?;
    let profile = profiles::detect(&client).await?;

    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
    if let Some(service) = guard.services.iter_mut().find(|s| s.id == service_id) {
        service.profile = profile;
    }
    config::save_config(&guard)?;

    match profile {
        Some(kind) => log_state.info(
            "profiles",
            &format!("{service_id}: detected profile {kind:?}"),
        ),
        None => log_state.warn(
            "profiles",
            &format!("{service_id}: no known log location found"),
        ),
    }
    Ok(profile)
}

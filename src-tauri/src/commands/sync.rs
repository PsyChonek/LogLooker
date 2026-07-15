use crate::auth::{self, TokenState};
use crate::cache::{self, CacheStatus, SyncSummary};
use crate::config::ConfigState;
use crate::kudu::KuduClient;
use crate::logger::LogState;
use crate::profiles;
use chrono::NaiveDate;
use tauri::Emitter;

/// Syncs the selected services' logs for a date range into the local cache.
/// Emits "sync-progress" events (cache::SyncProgress payload), throttled so
/// the UI is not flooded by download chunks, and "cache-status" events
/// (cache::CacheStatus payload) after each finished file so the cached size
/// and coverage columns update while the sync is still running.
#[tauri::command]
pub async fn sync_services(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, ConfigState>,
    token_state: tauri::State<'_, TokenState>,
    log_state: tauri::State<'_, LogState>,
    service_ids: Vec<String>,
    date_from: NaiveDate,
    date_to: NaiveDate,
) -> Result<Vec<SyncSummary>, String> {
    let services = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard.services.clone()
    };

    let token = auth::get_token(&token_state).await?;
    let mut summaries = Vec::new();

    for id in &service_ids {
        // Each service is synced independently: one that fails (missing Kudu URL,
        // undetectable profile, download error) is skipped and reported rather
        // than aborting the whole batch, so the remaining services still sync.
        let result: Result<cache::SyncSummary, String> = async {
            let service = services
                .iter()
                .find(|s| &s.id == id)
                .ok_or_else(|| format!("Unknown service: {id}"))?;
            let kudu_url = service
                .kudu_url
                .as_deref()
                .ok_or_else(|| format!("{id}: Kudu URL not set — set it in the services table"))?;
            let client = KuduClient::new(kudu_url, token.clone())?;

            let profile = match service.profile {
                Some(profile) => profile,
                None => {
                    let detected = profiles::detect(&client)
                        .await?
                        .ok_or_else(|| format!("{id}: no known log location found"))?;
                    let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
                    if let Some(entry) = guard.services.iter_mut().find(|s| &s.id == id) {
                        entry.profile = Some(detected);
                    }
                    crate::config::save_config(&guard)?;
                    detected
                }
            };

            let mut last_emitted: u64 = 0;
            cache::sync_service(&client, service, profile, date_from, date_to, |p| {
                let boundary = p.bytes_downloaded / 524_288;
                if p.state != "downloading" || boundary > last_emitted {
                    last_emitted = boundary;
                    app.emit("sync-progress", &p).ok();
                }
                // The manifest is saved before "done"/"skipped" is reported, so the
                // status is already up to date for this file
                if p.state != "downloading" {
                    if let Ok(status) = cache::cache_status(service) {
                        app.emit("cache-status", &status).ok();
                    }
                }
            })
            .await
        }
        .await;

        match result {
            Ok(summary) => {
                log_state.info(
                    "sync",
                    &format!(
                        "{id}: {} downloaded, {} skipped, {:.1} MB",
                        summary.files_downloaded,
                        summary.files_skipped,
                        summary.bytes_downloaded as f64 / 1_048_576.0
                    ),
                );
                for warning in &summary.warnings {
                    log_state.warn("sync", &format!("{id}: {warning}"));
                }
                summaries.push(summary);
            }
            Err(e) => {
                log_state.error("sync", &format!("{id}: sync failed — {e}"));
                summaries.push(SyncSummary {
                    service_id: id.clone(),
                    files_total: 0,
                    files_downloaded: 0,
                    files_skipped: 0,
                    bytes_downloaded: 0,
                    warnings: vec![format!("sync failed: {e}")],
                });
            }
        }
    }

    Ok(summaries)
}

#[tauri::command]
pub fn cache_status_all(
    config_state: tauri::State<'_, ConfigState>,
) -> Result<Vec<CacheStatus>, String> {
    let guard = config_state.0.lock().map_err(|e| e.to_string())?;
    guard.services.iter().map(cache::cache_status).collect()
}

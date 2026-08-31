use crate::auth::{self, TokenState};
use crate::cache::{self, CacheStatus, SyncSummary};
use crate::config::{ConfigState, ServiceConfig};
use crate::locations;
use crate::logger::LogState;
use crate::plugin::PluginState;
use chrono::NaiveDate;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;

/// Set by `cancel_sync`, read by a running sync per download chunk, between
/// files, and between services in the batch - a large file stops mid-stream and
/// its partial is kept as a resumable prefix. `sync_services` and
/// `download_services` rearm it at the start of each run. Shared by both since
/// the UI runs them one at a time.
pub struct SyncCancel(pub Arc<AtomicBool>);

impl SyncCancel {
    pub fn new() -> Self {
        SyncCancel(Arc::new(AtomicBool::new(false)))
    }
}

#[tauri::command]
pub fn cancel_sync(cancel: tauri::State<'_, SyncCancel>) {
    cancel.0.store(true, Ordering::Relaxed);
}

/// Syncs one service's logs into the local cache: resolves the pack that owns
/// it, builds its transport, ensures a log location (detecting and persisting one
/// if absent), and downloads the missing/changed files for the range while
/// emitting "sync-progress" (throttled) and "cache-status" events. Shared by
/// `sync_services` and `download_services`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn sync_one(
    app: &tauri::AppHandle,
    config_state: &tauri::State<'_, ConfigState>,
    plugin_state: &tauri::State<'_, PluginState>,
    token_state: &tauri::State<'_, TokenState>,
    service: &ServiceConfig,
    date_from: NaiveDate,
    date_to: NaiveDate,
    cancel: &AtomicBool,
) -> Result<SyncSummary, String> {
    let first = sync_one_attempt(
        app,
        config_state,
        plugin_state,
        token_state,
        service,
        date_from,
        date_to,
        cancel,
    )
    .await;
    if !should_refresh_token(&first) || cancel.load(Ordering::Relaxed) {
        return first;
    }

    // A token may be revoked before its advertised expiry. Clear it and repeat
    // the whole service once so the user does not have to run Sync or Download
    // a second time. Sync is resumable, so files completed by the first attempt
    // are safely skipped by the retry.
    auth::invalidate_token(token_state);
    sync_one_attempt(
        app,
        config_state,
        plugin_state,
        token_state,
        service,
        date_from,
        date_to,
        cancel,
    )
    .await
}

fn should_refresh_token(result: &Result<SyncSummary, String>) -> bool {
    match result {
        Err(error) => auth::is_access_denied(error),
        Ok(summary) => summary
            .warnings
            .iter()
            .any(|warning| auth::is_access_denied(warning)),
    }
}

#[allow(clippy::too_many_arguments)]
async fn sync_one_attempt(
    app: &tauri::AppHandle,
    config_state: &tauri::State<'_, ConfigState>,
    plugin_state: &tauri::State<'_, PluginState>,
    token_state: &tauri::State<'_, TokenState>,
    service: &ServiceConfig,
    date_from: NaiveDate,
    date_to: NaiveDate,
    cancel: &AtomicBool,
) -> Result<SyncSummary, String> {
    let id = &service.id;
    let registry = plugin_state.snapshot();
    let pack = registry.pack(&service.pack_id).cloned().ok_or_else(|| {
        format!(
            "{id}: its plugin \"{}\" is not loaded - enable or reinstall it in Plugins",
            service.pack_id
        )
    })?;
    let source = super::services::source_for(&pack, service, token_state).await?;

    let location_id = match &service.location {
        Some(id) => id.clone(),
        None => {
            let detected = locations::detect(&source, &pack).await?.ok_or_else(|| {
                format!(
                    "{id}: none of {}'s log locations matched - set one in the services table",
                    pack.manifest.name
                )
            })?;
            let mut guard = config_state.0.lock().map_err(|e| e.to_string())?;
            if let Some(entry) = guard.services.iter_mut().find(|s| &s.id == id) {
                entry.location = Some(detected.clone());
            }
            crate::config::save_config(&guard)?;
            detected
        }
    };
    let location = pack.location(&location_id).ok_or_else(|| {
        format!(
            "{id}: {} has no log location \"{location_id}\" - pick another in the services table",
            pack.manifest.name
        )
    })?;

    let mut last_emitted: u64 = 0;
    cache::sync_service(
        &source,
        service,
        location,
        date_from,
        date_to,
        cancel,
        |p| {
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
        },
    )
    .await
}

/// Syncs the selected services' logs for a date range into the local cache.
/// Emits "sync-progress" events (cache::SyncProgress payload), throttled so
/// the UI is not flooded by download chunks, and "cache-status" events
/// (cache::CacheStatus payload) after each finished file so the cached size
/// and coverage columns update while the sync is still running.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn sync_services(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    token_state: tauri::State<'_, TokenState>,
    log_state: tauri::State<'_, LogState>,
    cancel: tauri::State<'_, SyncCancel>,
    service_ids: Vec<String>,
    date_from: NaiveDate,
    date_to: NaiveDate,
) -> Result<Vec<SyncSummary>, String> {
    let services = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard.services.clone()
    };

    let cancel = Arc::clone(&cancel.0);
    cancel.store(false, Ordering::Relaxed);
    let mut summaries = Vec::new();

    for id in &service_ids {
        // The user cancelled: stop before starting the next service. Whatever
        // already synced stays cached and is reported in the summaries so far.
        if cancel.load(Ordering::Relaxed) {
            log_state.info("sync", "cancelled by user");
            break;
        }
        // Each service is synced independently: one that fails (missing endpoint,
        // undetectable location, download error) is skipped and reported rather
        // than aborting the whole batch, so the remaining services still sync.
        let result: Result<cache::SyncSummary, String> = match services.iter().find(|s| &s.id == id)
        {
            Some(service) => {
                sync_one(
                    &app,
                    &config_state,
                    &plugin_state,
                    &token_state,
                    service,
                    date_from,
                    date_to,
                    &cancel,
                )
                .await
            }
            None => Err(format!("Unknown service: {id}")),
        };

        match result {
            Ok(summary) => {
                let failed = if summary.files_failed > 0 {
                    format!(", {} failed", summary.files_failed)
                } else {
                    String::new()
                };
                log_state.info(
                    "sync",
                    &format!(
                        "{id}: {} downloaded, {} skipped{failed} of {}, {:.1} MB",
                        summary.files_downloaded,
                        summary.files_skipped,
                        summary.files_total,
                        summary.bytes_downloaded as f64 / 1_048_576.0
                    ),
                );
                for warning in &summary.warnings {
                    log_state.warn("sync", &format!("{id}: {warning}"));
                }
                summaries.push(summary);
            }
            Err(e) => {
                log_state.error("sync", &format!("{id}: sync failed - {e}"));
                summaries.push(SyncSummary {
                    service_id: id.clone(),
                    files_total: 0,
                    files_downloaded: 0,
                    files_skipped: 0,
                    files_failed: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(warnings: &[&str]) -> SyncSummary {
        SyncSummary {
            service_id: "test/public-api".into(),
            files_total: 1,
            files_downloaded: 0,
            files_skipped: 0,
            files_failed: warnings.len(),
            bytes_downloaded: 0,
            warnings: warnings.iter().map(|warning| warning.to_string()).collect(),
        }
    }

    #[test]
    fn refreshes_token_when_the_directory_listing_is_denied() {
        let result =
            Err("Access denied (401 Unauthorized) for https://app.scm/api/vfs/applogs/".into());

        assert!(should_refresh_token(&result));
    }

    #[test]
    fn refreshes_token_when_a_file_download_is_denied() {
        let result = Ok(summary(&[
            "log-2026-08-31.log: download failed - Access denied (401 Unauthorized) for https://app.scm/api/vfs/applogs/log-2026-08-31.log",
        ]));

        assert!(should_refresh_token(&result));
    }

    #[test]
    fn does_not_refresh_token_for_unrelated_failures() {
        let result = Ok(summary(&[
            "log-2026-08-31.log: download failed - connection reset",
        ]));

        assert!(!should_refresh_token(&result));
    }
}

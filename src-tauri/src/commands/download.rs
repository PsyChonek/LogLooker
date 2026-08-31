use super::sync::SyncCancel;
use crate::auth::TokenState;
use crate::cache;
use crate::config::ConfigState;
use crate::logger::LogState;
use crate::plugin::PluginState;
use chrono::NaiveDate;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Result of downloading one service to a chosen folder: the sync counts (what
/// was fetched from Kudu, or skipped as already cached) plus the export counts
/// (files decompressed into the folder).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSummary {
    pub service_id: String,
    pub files_downloaded: usize,
    pub files_skipped: usize,
    /// Files whose download failed; each has a matching entry in `warnings`
    pub files_failed: usize,
    pub bytes_downloaded: u64,
    pub files_exported: usize,
    pub bytes_exported: u64,
    /// The per-service folder the decompressed files were written to
    pub target_dir: String,
    pub warnings: Vec<String>,
}

/// Downloads the selected services' logs for a date range into `target_dir`,
/// decompressed. Each service first syncs into the local cache - which only
/// fetches files that are missing or have changed - then its cached files for
/// the range are decompressed into `target_dir/<env>/<service>/`. Services are
/// processed independently: one that fails is reported as a warning rather than
/// aborting the batch. Reuses the sync-progress / cache-status events, so the UI
/// shows the same live progress as a plain sync.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn download_services(
    app: tauri::AppHandle,
    config_state: tauri::State<'_, ConfigState>,
    plugin_state: tauri::State<'_, PluginState>,
    token_state: tauri::State<'_, TokenState>,
    log_state: tauri::State<'_, LogState>,
    cancel: tauri::State<'_, SyncCancel>,
    service_ids: Vec<String>,
    date_from: NaiveDate,
    date_to: NaiveDate,
    target_dir: String,
) -> Result<Vec<DownloadSummary>, String> {
    let services = {
        let guard = config_state.0.lock().map_err(|e| e.to_string())?;
        guard.services.clone()
    };

    let cancel = Arc::clone(&cancel.0);
    cancel.store(false, Ordering::Relaxed);
    let root = PathBuf::from(&target_dir);
    let mut summaries = Vec::new();

    for id in &service_ids {
        // The user cancelled: stop before starting the next service. Files
        // already fetched and exported stay on disk and in the summaries.
        if cancel.load(Ordering::Relaxed) {
            log_state.info("download", "cancelled by user");
            break;
        }
        let Some(service) = services.iter().find(|s| &s.id == id).cloned() else {
            summaries.push(failed_summary(id, format!("Unknown service: {id}")));
            continue;
        };

        let result: Result<DownloadSummary, String> = async {
            let sync = super::sync::sync_one(
                &app,
                &config_state,
                &plugin_state,
                &token_state,
                &service,
                date_from,
                date_to,
                &cancel,
            )
            .await?;
            let export = {
                let service = service.clone();
                let root = root.clone();
                tokio::task::spawn_blocking(move || {
                    cache::export_service(&service, date_from, date_to, &root)
                })
                .await
                .map_err(|e| format!("Export task failed: {e}"))??
            };
            let no_remote_files = sync.files_total == 0;
            let mut warnings = sync.warnings;
            warnings.extend(export.warnings);
            if no_remote_files && export.files_written == 0 {
                warnings.push(format!(
                    "No log files matched {date_from} through {date_to}"
                ));
            }
            Ok(DownloadSummary {
                service_id: id.clone(),
                files_downloaded: sync.files_downloaded,
                files_skipped: sync.files_skipped,
                files_failed: sync.files_failed,
                bytes_downloaded: sync.bytes_downloaded,
                files_exported: export.files_written,
                bytes_exported: export.bytes_written,
                target_dir: export.target_dir,
                warnings,
            })
        }
        .await;

        match result {
            Ok(summary) => {
                log_state.info(
                    "download",
                    &format!(
                        "{id}: {} exported ({:.1} MB) to {}",
                        summary.files_exported,
                        summary.bytes_exported as f64 / 1_048_576.0,
                        summary.target_dir
                    ),
                );
                for warning in &summary.warnings {
                    log_state.warn("download", &format!("{id}: {warning}"));
                }
                summaries.push(summary);
            }
            Err(e) => {
                log_state.error("download", &format!("{id}: download failed - {e}"));
                summaries.push(failed_summary(id, format!("download failed: {e}")));
            }
        }
    }

    Ok(summaries)
}

fn failed_summary(id: &str, warning: String) -> DownloadSummary {
    DownloadSummary {
        service_id: id.to_string(),
        files_downloaded: 0,
        files_skipped: 0,
        files_failed: 0,
        bytes_downloaded: 0,
        files_exported: 0,
        bytes_exported: 0,
        target_dir: String::new(),
        warnings: vec![warning],
    }
}

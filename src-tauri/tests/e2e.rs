//! End-to-end test against a real source, driving the same path the app does:
//! pick a pack, build its transport, detect the log location, sync a day, search
//! the cache, then search it again from RAM.
//!
//! Kudu (needs network and `az login`):
//!   $env:LOGLOOKER_E2E_KUDU_URL = "https://<app>.scm.azurewebsites.net"
//!   cargo test --test e2e -- --ignored --nocapture
//!
//! A local folder (needs nothing):
//!   $env:LOGLOOKER_E2E_FOLDER = "C:\some\logs"
//!   cargo test --test e2e -- --ignored --nocapture
//!
//! Optional: LOGLOOKER_E2E_QUERY (default "error"), LOGLOOKER_E2E_DAYS_AGO
//! (default 1). The assertions are about the pipeline running end to end, not
//! about any particular line - the test has no idea what the target logs.

use log_looker_lib::{auth, cache, config, locations, memcache, plugin, search, source};
use log_looker_lib::search::SearchSpec;

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The bundled pack matching whichever target was configured.
fn target() -> Option<(&'static str, String)> {
    if let Some(url) = env_var("LOGLOOKER_E2E_KUDU_URL") {
        return Some(("azure-kudu", config::normalize_kudu_url(&url).expect("bad Kudu URL")));
    }
    let folder = env_var("LOGLOOKER_E2E_FOLDER")?;
    Some(("local-folder", config::normalize_folder_path(&folder).expect("bad folder")))
}

#[tokio::test]
#[ignore]
async fn detect_sync_search() {
    let Some((pack_id, endpoint)) = target() else {
        println!(
            "Neither LOGLOOKER_E2E_KUDU_URL nor LOGLOOKER_E2E_FOLDER is set - \
             nothing to test against, skipping"
        );
        return;
    };
    let query = env_var("LOGLOOKER_E2E_QUERY").unwrap_or_else(|| "error".into());
    let days_ago: u64 = env_var("LOGLOOKER_E2E_DAYS_AGO")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    // Keep test downloads out of the real app cache
    let cache_dir = std::env::temp_dir().join("loglooker-e2e-cache");
    std::env::set_var("LOGLOOKER_CACHE_DIR", &cache_dir);

    // 1. The pack, and the transport it declares
    let registry = plugin::load(&[]);
    let pack = registry
        .pack(pack_id)
        .unwrap_or_else(|| panic!("bundled pack {pack_id} must load"));
    let environment = pack.manifest.environments[0].id.clone();
    println!("pack: {} against {endpoint}", pack.manifest.name);

    let token_state = auth::TokenState::new();
    let token = match source::LogSource::needs_token(&pack.manifest.source) {
        true => Some(auth::get_token(&token_state).await.expect("az token failed")),
        false => None,
    };
    let src = source::LogSource::new(&pack.manifest.source, &endpoint, token).expect("bad endpoint");

    // 2. Where this service keeps its logs
    let location_id = locations::detect(&src, pack)
        .await
        .expect("detect failed")
        .expect("none of the pack's log locations matched the target");
    println!("detected log location: {location_id}");
    let location = pack.location(&location_id).expect("detected id must resolve");

    // 3. Sync one day into the cache
    let day = chrono::Utc::now().date_naive() - chrono::Days::new(days_ago);
    let service = config::ServiceConfig {
        id: config::service_id(&environment, "E2E"),
        name: "E2E".into(),
        environment,
        pack_id: pack_id.to_string(),
        endpoint: Some(endpoint),
        endpoint_manual: true,
        location: Some(location_id),
        source: config::ServiceSource::Manual,
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let summary = cache::sync_service(&src, &service, location, day, day, &cancel, |_| {})
        .await
        .expect("sync failed");
    println!(
        "synced: {} of {} files downloaded, {} skipped, {} bytes",
        summary.files_downloaded, summary.files_total, summary.files_skipped, summary.bytes_downloaded
    );
    assert!(
        summary.files_total > 0,
        "the target logged nothing on {day} - try LOGLOOKER_E2E_DAYS_AGO"
    );

    // 4. Search the cache, then again from the memory cache
    let request = search::SearchRequest {
        service_ids: vec![service.id.clone()],
        date_from: day,
        date_to: day,
        time_from: None,
        time_to: None,
        query,
        is_regex: false,
        case_sensitive: false,
        context_lines: 2,
    };
    let mem = memcache::MemCache::new(true, memcache::DEFAULT_MAX_MB);
    let spec = SearchSpec::from_registry(&registry);
    let result = search::run(
        &request,
        std::slice::from_ref(&service),
        &spec,
        &mem,
        &cancel,
        usize::MAX,
        |_| {},
    )
    .expect("search failed");
    println!(
        "search: {} hits over {} lines in {} ms",
        result.hits.len(),
        result.lines_scanned,
        result.duration_ms
    );
    assert!(result.lines_scanned > 0, "the synced file scanned as empty");

    let cached = search::run(&request, &[service], &spec, &mem, &cancel, usize::MAX, |_| {})
        .expect("cached search failed");
    println!(
        "cached search: {} hits in {} ms ({} MB held)",
        cached.hits.len(),
        cached.duration_ms,
        mem.stats().unwrap().used_bytes / 1_048_576
    );
    assert_eq!(cached.hits.len(), result.hits.len(), "same hits from RAM");
    assert!(mem.stats().unwrap().files > 0, "nothing was held in RAM");
}

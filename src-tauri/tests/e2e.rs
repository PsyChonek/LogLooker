//! End-to-end test against the real test environment. Read-only (Kudu GETs)
//! but needs network + `az login`, so it is ignored by default:
//!
//!   cargo test --test e2e -- --ignored --nocapture

use log_looker_lib::{auth, cache, config, kudu, memcache, profiles, scraper, search};

#[tokio::test]
#[ignore]
async fn scrape_detect_sync_search() {
    // Keep test downloads out of the real app cache
    let cache_dir = std::env::temp_dir().join("loglooker-e2e-cache");
    std::env::set_var("LOGLOOKER_CACHE_DIR", &cache_dir);

    // 1. Scrape the status page
    let services = scraper::scrape_services().await.expect("scrape failed");
    println!("scraped {} services", services.len());
    assert!(services.len() >= 20, "expected both sections to be listed");

    let client_api = services
        .iter()
        .find(|s| s.name == "ClientApi" && s.environment == scraper::Environment::Test)
        .expect("test ClientApi not found on status page");
    let kudu_url = client_api.kudu_url.clone().expect("ClientApi has no Kudu link");

    // 2. Authenticate and detect the log profile
    let token_state = auth::TokenState::new();
    let token = auth::get_token(&token_state).await.expect("az token failed");
    let client = kudu::KuduClient::new(&kudu_url, token).expect("bad kudu url");

    let profile = profiles::detect(&client)
        .await
        .expect("detect failed")
        .expect("no profile detected");
    println!("detected profile: {profile:?}");
    assert_eq!(profile, profiles::ProfileKind::CoreApplogs);

    // 3. Sync yesterday's log into the cache
    let yesterday = chrono::Utc::now().date_naive() - chrono::Days::new(1);
    let service = config::ServiceConfig {
        id: config::service_id(client_api.environment, &client_api.name),
        name: client_api.name.clone(),
        environment: client_api.environment,
        kudu_url: Some(kudu_url),
        kudu_url_manual: false,
        profile: Some(profile),
        source: config::ServiceSource::Scraped,
    };
    let summary = cache::sync_service(&client, &service, profile, yesterday, yesterday, |_| {})
        .await
        .expect("sync failed");
    println!(
        "synced: {} downloaded, {} skipped, {} bytes",
        summary.files_downloaded, summary.files_skipped, summary.bytes_downloaded
    );
    assert_eq!(summary.files_total, 1);

    // 4. Search the cache
    let request = search::SearchRequest {
        service_ids: vec![service.id.clone()],
        date_from: yesterday,
        date_to: yesterday,
        query: "Request finished".into(),
        is_regex: false,
        case_sensitive: true,
        context_lines: 2,
    };
    let cache = memcache::MemCache::new(true, memcache::DEFAULT_MAX_MB);
    let result = search::run(&request, &[service.clone()], &cache, |_| {}).expect("search failed");
    println!(
        "search: {} hits, {} lines in {} ms",
        result.hits.len(),
        result.lines_scanned,
        result.duration_ms
    );
    assert!(!result.hits.is_empty(), "expected hits for 'Request finished'");
    let hit = &result.hits[0];
    assert!(hit.timestamp.is_some(), "hit should carry a parsed timestamp");
    assert!(hit.fields.duration_ms.is_some(), "'Request finished' lines carry a duration");

    // 5. The same search again, now served from the memory cache
    let cached = search::run(&request, &[service], &cache, |_| {}).expect("cached search failed");
    println!(
        "cached search: {} hits in {} ms ({} MB held)",
        cached.hits.len(),
        cached.duration_ms,
        cache.stats().unwrap().used_bytes / 1_048_576
    );
    assert_eq!(cached.hits.len(), result.hits.len(), "same hits from RAM");
    assert_eq!(cache.stats().unwrap().files, 1);
}

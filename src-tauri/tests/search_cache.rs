//! Drives `search::run` over a hand-built cache directory — the same code path
//! the app uses, without Kudu or the network. Covers the memory cache: a search
//! must return the same hits whether the files come from RAM or from disk.

use log_looker_lib::cache::{CachedFile, Manifest};
use log_looker_lib::config::{ServiceConfig, ServiceSource};
use log_looker_lib::memcache::MemCache;
use log_looker_lib::scraper::Environment;
use log_looker_lib::search::{self, SearchRequest, SearchResult};
use std::collections::HashMap;

const DAY: &str = "2026-07-14";

/// Two matching lines among filler, plus a continuation line that carries no
/// timestamp of its own — the scan must date it by the entry that opened it.
fn log_text() -> String {
    let mut text = String::new();
    for minute in 0..200 {
        text.push_str(&format!(
            "14.07.2026 09:{minute:02}:00.100 INFO - Request starting HTTP/1.1 GET http://x\n"
        ));
    }
    text.push_str("14.07.2026 09:30:00.200 INFO - (ID Command: 7391c08f-1111-2222-3333-444455556666) Handling UpdateReadDetailPostCommand ID_Login:3306928E-aaaa-bbbb-cccc-ddddeeeeffff\n");
    text.push_str("    at Example.Api.Handler.Handle()\n");
    text.push_str("14.07.2026 09:31:00.300 ERROR - Boom: NullReferenceException\n");
    text
}

fn service() -> ServiceConfig {
    ServiceConfig {
        id: "test/Svc".into(),
        name: "Svc".into(),
        environment: Environment::Test,
        kudu_url: None,
        kudu_url_manual: false,
        profile: None,
        source: ServiceSource::Manual,
    }
}

/// Writes the zstd file and the manifest entry a synced service would leave behind.
fn build_cache(service: &ServiceConfig) -> u64 {
    let dir = log_looker_lib::cache::service_dir(service.environment, &service.name).unwrap();
    let text = log_text();
    let local_name = format!("log-{DAY}.log.zst");
    std::fs::write(
        dir.join(&local_name),
        zstd::stream::encode_all(text.as_bytes(), 3).unwrap(),
    )
    .unwrap();

    let manifest = Manifest {
        entries: HashMap::from([(
            format!("applogs/log-{DAY}.log"),
            CachedFile {
                remote_size: text.len() as u64,
                remote_mtime: "2026-07-14T23:59:00Z".into(),
                local_name,
                date: Some(DAY.parse().unwrap()),
                instance: None,
                first_ts: None,
                last_ts: None,
            },
        )]),
    };
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    text.len() as u64
}

fn search_for(query: &str, cache: &MemCache, service: &ServiceConfig) -> SearchResult {
    let request = SearchRequest {
        service_ids: vec![service.id.clone()],
        date_from: DAY.parse().unwrap(),
        date_to: DAY.parse().unwrap(),
        query: query.into(),
        is_regex: false,
        case_sensitive: false,
        context_lines: 2,
    };
    search::run(&request, std::slice::from_ref(service), cache, |_| {}).expect("search failed")
}

/// The hits a search must produce, whatever tier served the file.
fn assert_expected(result: &SearchResult) {
    assert_eq!(result.files_scanned, 1);
    assert_eq!(result.lines_scanned, 203);
    assert_eq!(result.hits.len(), 1, "one 'Handling' line");

    let hit = &result.hits[0];
    assert_eq!(hit.line_number, 201);
    assert_eq!(
        hit.fields.operation.as_deref(),
        Some("UpdateReadDetailPostCommand")
    );
    assert_eq!(
        hit.fields.id_login.as_deref(),
        Some("3306928e-aaaa-bbbb-cccc-ddddeeeeffff")
    );
    assert_eq!(
        hit.timestamp.map(|ts| ts.to_string()),
        Some("2026-07-14 09:30:00.200".to_string())
    );
    // Context is collected across the tiers the same way
    assert_eq!(hit.context_before.len(), 2);
    assert_eq!(hit.context_after.len(), 2);
    assert!(hit.context_after[0].starts_with("    at Example.Api.Handler.Handle()"));
}

#[test]
fn a_search_returns_the_same_hits_from_disk_from_ram_and_when_the_budget_runs_out() {
    let dir = std::env::temp_dir().join("loglooker-search-cache-test");
    std::fs::remove_dir_all(&dir).ok();
    std::env::set_var("LOGLOOKER_CACHE_DIR", &dir);

    let service = service();
    let text_bytes = build_cache(&service);

    // Cache off — the file is read from disk and nothing is held
    let off = MemCache::new(false, 2048);
    assert_expected(&search_for("Handling", &off, &service));
    assert_eq!(off.stats().unwrap().files, 0);

    // Cache on — the first search loads the text, the second reuses it
    let on = MemCache::new(true, 2048);
    assert_expected(&search_for("Handling", &on, &service));
    let stats = on.stats().unwrap();
    assert_eq!(stats.files, 1);
    assert_eq!(stats.used_bytes, text_bytes, "the text is held decompressed");

    assert_expected(&search_for("Handling", &on, &service));
    assert_eq!(
        on.stats().unwrap().used_bytes,
        text_bytes,
        "the second search reuses the entry rather than reloading it"
    );

    // A file larger than the budget streams instead of being held; the smallest
    // cap the UI allows still dwarfs this test's file, so that path is covered
    // by memcache's own tests. The disabled cache above covers streaming here.

    // A re-synced (grown) file is not served from a stale RAM copy
    let dir = log_looker_lib::cache::service_dir(service.environment, &service.name).unwrap();
    let grown = format!("{}14.07.2026 09:32:00.400 INFO - Handling AgainCommand\n", log_text());
    std::fs::write(
        dir.join(format!("log-{DAY}.log.zst")),
        zstd::stream::encode_all(grown.as_bytes(), 3).unwrap(),
    )
    .unwrap();
    let result = search_for("Handling", &on, &service);
    assert_eq!(result.hits.len(), 2, "the new line is searched, not the cached bytes");
}

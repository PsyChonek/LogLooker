use crate::memcache::{self, DEFAULT_MAX_MB};
use crate::plugin::{DiscoveryDef, EnvironmentDef, Registry, SourceDef};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct ConfigState(pub Mutex<AppConfig>);

/// An environment id as some loaded pack declares it, e.g. `test`. Also a cache
/// directory segment, which is why packs validate it as a path-safe slug.
pub type EnvId = String;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    /// Environments the user created, on top of whatever the packs declare. Any
    /// service can live in any of them, whichever plugin it uses.
    #[serde(default)]
    pub environments: Vec<EnvironmentDef>,
    #[serde(default)]
    pub saved_queries: Vec<SavedQuery>,
    /// Ids of the pack-provided services already offered once, so deleting one
    /// sticks instead of it reappearing on the next start
    #[serde(default)]
    pub seeded_builtins: Vec<String>,
    /// Packs the user switched off. A disabled pack is not loaded at all, so it
    /// costs nothing at scan time.
    #[serde(default)]
    pub disabled_packs: Vec<String>,
    #[serde(default)]
    pub memory_cache: MemoryCacheConfig,
    #[serde(default)]
    pub search: SearchConfig,
}

pub const DEFAULT_MAX_HITS: usize = 2_000_000;
/// Below the minimum the cap would cut everyday searches. 0 means unlimited;
/// anything above the minimum is a custom cap taken as intended.
pub const MIN_MAX_HITS: usize = 100_000;

/// Limits on what a search keeps. Hits are stored compactly (about a hundred
/// bytes each), so the cap bounds the index memory: 2M hits is roughly 200 MB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchConfig {
    /// A search stops collecting past this many hits and reports the result as
    /// truncated. 0 means unlimited.
    pub max_hits: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            max_hits: DEFAULT_MAX_HITS,
        }
    }
}

impl SearchConfig {
    /// A hand-edited config.json can carry any number; what is in force is the
    /// clamped value, and it is what the UI reports back.
    pub fn normalized(self) -> Self {
        SearchConfig {
            max_hits: match self.max_hits {
                0 => 0,
                hits => hits.max(MIN_MAX_HITS),
            },
        }
    }
}

/// Keeping cached log files in RAM between searches. Off until asked for - it
/// is the one setting that spends the user's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCacheConfig {
    pub enabled: bool,
    /// Cap on everything the cache holds: the decompressed text plus the
    /// compressed blobs it keeps so it can demote instead of re-reading
    pub max_mb: u64,
}

impl Default for MemoryCacheConfig {
    fn default() -> Self {
        MemoryCacheConfig {
            enabled: false,
            max_mb: DEFAULT_MAX_MB,
        }
    }
}

impl MemoryCacheConfig {
    /// A hand-edited config.json can carry any number; the cache is built from
    /// the clamped one, so what the UI reports back is what is in force.
    pub fn normalized(self) -> Self {
        MemoryCacheConfig {
            enabled: self.enabled,
            max_mb: memcache::clamp_max_mb(self.max_mb),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceSource {
    /// Came from the pack's discovery page; a refresh owns it, so it is not deletable
    #[default]
    Scraped,
    /// Listed in a pack's static discovery, seeded once and deletable afterwards
    Builtin,
    /// Added by the user in the Services table
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceConfig {
    /// Stable key: "<environment>/<name>", e.g. "test/ClientApi"
    pub id: String,
    pub name: String,
    pub environment: EnvId,
    /// The pack that owns this service's transport and log locations. Empty in
    /// configs written before packs existed; `migrate_for_packs` fills it in.
    #[serde(default)]
    pub pack_id: String,
    /// Where the logs are: a Kudu base URL, a folder path - whatever the pack's
    /// source takes. None when discovery found no link and the user has not set
    /// one, in which case the service simply cannot sync yet.
    #[serde(default, alias = "kuduUrl")]
    pub endpoint: Option<String>,
    /// Set once the user edits the endpoint by hand (or adds the service), so a
    /// later discovery refresh leaves their value alone instead of overwriting it
    #[serde(default, alias = "kuduUrlManual")]
    pub endpoint_manual: bool,
    /// Id of the pack log location to read. None until detection ran; the user
    /// can override it afterwards.
    #[serde(default, alias = "profile")]
    pub location: Option<String>,
    /// Defaulted for configs written before this field existed - those entries
    /// all came from the scrape
    #[serde(default)]
    pub source: ServiceSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedQuery {
    pub name: String,
    pub query: String,
    pub is_regex: bool,
}

pub fn service_id(environment: &str, name: &str) -> String {
    format!("{environment}/{name}")
}

/// The id of an environment the user named. Ids end up as cache directory
/// segments, so a free-form name is reduced to the same lowercase slug packs are
/// held to - "Staging EU" becomes "staging-eu".
pub fn environment_id(label: &str) -> Result<String, String> {
    let mut id = String::new();
    for c in label.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c);
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_matches('-').to_string();
    if id.is_empty() {
        return Err(format!(
            "Environment name \"{}\" has no letters or digits to build an id from",
            label.trim()
        ));
    }
    Ok(id)
}

/// Every environment a service can be put in: what the packs declare plus what
/// the user added, pack order first so the familiar ones stay leftmost.
pub fn environments(config: &AppConfig, registry: &Registry) -> Vec<EnvironmentDef> {
    let mut out = registry.environments();
    for env in &config.environments {
        if !out.iter().any(|e| e.id == env.id) {
            out.push(env.clone());
        }
    }
    out
}

/// Services the loaded packs list in their static discovery - hosts no status
/// page knows about, shipped with the pack instead. Seeded once per id (see
/// `AppConfig::seeded_builtins`) and deletable afterwards.
pub fn pack_services(registry: &Registry) -> Vec<ServiceConfig> {
    let mut out = Vec::new();
    for pack in &registry.packs {
        let DiscoveryDef::Static { services } = &pack.manifest.discovery else {
            continue;
        };
        for service in services {
            out.push(ServiceConfig {
                id: service_id(&service.environment, &service.name),
                name: service.name.clone(),
                environment: service.environment.clone(),
                pack_id: pack.manifest.id.clone(),
                endpoint: Some(service.endpoint.clone()),
                endpoint_manual: false,
                location: service.location.clone(),
                source: ServiceSource::Builtin,
            });
        }
    }
    out
}

/// Adds pack services that have never been seeded. Returns whether anything
/// changed. A service the user deleted stays deleted: its id is remembered.
fn seed_pack_services(config: &mut AppConfig, registry: &Registry) -> bool {
    let mut changed = false;
    for service in pack_services(registry) {
        if config.seeded_builtins.contains(&service.id) {
            continue;
        }
        config.seeded_builtins.push(service.id.clone());
        changed = true;
        if !config.services.iter().any(|s| s.id == service.id) {
            config.services.push(service);
        }
    }
    if changed {
        sort_services(config);
    }
    changed
}

/// Binds services written before packs existed to a pack. Nothing is ever
/// dropped: a service whose pack is missing or disabled keeps its config and the
/// UI reports it as unusable, because deleting the user's entry over a pack that
/// may come back is not ours to do.
pub fn migrate_for_packs(config: &mut AppConfig, registry: &Registry) -> bool {
    let mut changed = false;
    if let Some(default_pack) = registry.default_pack_id() {
        for service in &mut config.services {
            if service.pack_id.is_empty() {
                service.pack_id = default_pack.clone();
                changed = true;
            }
        }
    }
    changed |= seed_pack_services(config, registry);
    changed
}

pub fn sort_services(config: &mut AppConfig) {
    config
        .services
        .sort_by(|a, b| a.id.to_lowercase().cmp(&b.id.to_lowercase()));
}

/// Canonical form for a service's endpoint, per the owning pack's transport.
/// Display shortening and duplicate checks assume it.
pub fn normalize_endpoint(source: &SourceDef, endpoint: &str) -> Result<String, String> {
    match source {
        SourceDef::Kudu { .. } => normalize_kudu_url(endpoint),
        SourceDef::LocalFolder { .. } => normalize_folder_path(endpoint),
    }
}

/// https, no trailing slash. Plain http is refused: the token goes in a header,
/// so an unencrypted endpoint would put a bearer credential on the wire.
pub fn normalize_kudu_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Kudu URL cannot be empty".into());
    }
    if !trimmed.starts_with("https://") {
        return Err(format!(
            "Kudu URL must start with https:// - got \"{trimmed}\""
        ));
    }
    Ok(trimmed.to_string())
}

/// A folder path, trailing separator trimmed. Existence is not checked here: a
/// share that is offline right now is still the right configuration.
pub fn normalize_folder_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Err("Folder path cannot be empty".into());
    }
    Ok(trimmed.to_string())
}

/// The name is also the cache directory name, so it has to stay a plain path segment.
pub fn validate_service_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Service name cannot be empty".into());
    }
    if trimmed.contains(|c: char| c.is_control() || r#"/\<>:"|?*"#.contains(c)) {
        return Err(format!(
            "Service name cannot contain any of / \\ < > : \" | ? * - got \"{trimmed}\""
        ));
    }
    Ok(trimmed.to_string())
}

pub fn config_dir() -> Result<PathBuf, String> {
    let dir = dirs::config_dir()
        .ok_or("Cannot resolve config directory")?
        .join("LogLooker");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create config dir: {e}"))?;
    Ok(dir)
}

fn config_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("config.json"))
}

/// Reads config without touching packs - the pack registry itself is built from
/// `disabled_packs`, so it cannot exist yet. Follow with `migrate_for_packs`.
pub fn load_config() -> AppConfig {
    let Ok(path) = config_path() else {
        return AppConfig::default();
    };
    let mut config: AppConfig = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    };
    config.memory_cache = config.memory_cache.normalized();
    config.search = config.search.normalized();
    config
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let path = config_path()?;
    let text = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("Cannot write config: {e}"))
}

/// Search presets the loaded packs offer, shown alongside the user's saved
/// queries. Which presets make sense depends entirely on what the logs look
/// like, so they come from the packs rather than from the app.
pub fn preset_queries(registry: &Registry) -> Vec<SavedQuery> {
    registry
        .presets()
        .into_iter()
        .map(|(name, preset)| SavedQuery {
            name,
            query: preset.query.clone(),
            is_regex: preset.is_regex,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{self, PackOrigin};
    use std::sync::Arc;

    /// A pack that lists two services statically, standing in for hosts no
    /// discovery page knows about.
    fn static_pack() -> Registry {
        let pack = plugin::parse_and_compile(
            r#"{
                "id": "hosts", "name": "Hosts", "source": { "type": "kudu" },
                "environments": [ { "id": "test", "label": "TEST" },
                                  { "id": "production", "label": "PROD" } ],
                "discovery": { "type": "static", "services": [
                  { "name": "WebJobs", "environment": "test",
                    "endpoint": "https://test-webjobs.scm.azurewebsites.net" },
                  { "name": "WebJobs", "environment": "production",
                    "endpoint": "https://prod-webjobs.scm.azurewebsites.net" }
                ] }
            }"#,
            PackOrigin::Bundled,
            false,
        )
        .expect("test pack must compile");
        Registry {
            packs: vec![Arc::new(pack)],
            errors: Vec::new(),
        }
    }

    #[test]
    fn seeds_pack_services_into_an_empty_config() {
        let registry = static_pack();
        let mut config = AppConfig::default();
        assert!(migrate_for_packs(&mut config, &registry));

        let seeded: Vec<_> = config
            .services
            .iter()
            .map(|s| (s.id.as_str(), s.endpoint.as_deref().unwrap(), s.pack_id.as_str()))
            .collect();
        assert_eq!(
            seeded,
            vec![
                ("production/WebJobs", "https://prod-webjobs.scm.azurewebsites.net", "hosts"),
                ("test/WebJobs", "https://test-webjobs.scm.azurewebsites.net", "hosts"),
            ]
        );
        assert!(config
            .services
            .iter()
            .all(|s| s.source == ServiceSource::Builtin));
    }

    #[test]
    fn does_not_reseed_a_deleted_pack_service() {
        let registry = static_pack();
        let mut config = AppConfig::default();
        migrate_for_packs(&mut config, &registry);
        config.services.retain(|s| s.id != "test/WebJobs");

        assert!(!migrate_for_packs(&mut config, &registry));
        assert!(!config.services.iter().any(|s| s.id == "test/WebJobs"));
    }

    #[test]
    fn seeding_keeps_an_existing_service_with_the_same_id() {
        let mut config = AppConfig {
            services: vec![ServiceConfig {
                id: "test/WebJobs".into(),
                name: "WebJobs".into(),
                environment: "test".into(),
                pack_id: "hosts".into(),
                endpoint: Some("https://custom.scm.azurewebsites.net".into()),
                endpoint_manual: true,
                location: Some("core-applogs".into()),
                source: ServiceSource::Manual,
            }],
            ..AppConfig::default()
        };
        migrate_for_packs(&mut config, &static_pack());

        let existing: Vec<_> = config
            .services
            .iter()
            .filter(|s| s.id == "test/WebJobs")
            .collect();
        assert_eq!(existing.len(), 1);
        assert_eq!(
            existing[0].endpoint.as_deref(),
            Some("https://custom.scm.azurewebsites.net")
        );
    }

    /// A config written before packs existed carries no packId and the old
    /// `kuduUrl`/`profile` names. It must come back bound to a pack rather than
    /// as a service the app cannot place.
    #[test]
    fn migrates_a_pre_plugin_config() {
        let text = r#"{
            "services": [ {
                "id": "test/ClientApi", "name": "ClientApi", "environment": "test",
                "kuduUrl": "https://example.scm.azurewebsites.net",
                "kuduUrlManual": false, "profile": "core-applogs", "source": "scraped"
            } ]
        }"#;
        let mut config: AppConfig = serde_json::from_str(text).expect("old config must still read");
        assert_eq!(
            config.services[0].endpoint.as_deref(),
            Some("https://example.scm.azurewebsites.net")
        );
        assert_eq!(config.services[0].location.as_deref(), Some("core-applogs"));

        assert!(migrate_for_packs(&mut config, &static_pack()));
        assert_eq!(config.services[0].pack_id, "hosts");
    }

    /// Services whose pack is gone are kept: the pack may come back, and
    /// throwing away the user's entry over it is not the app's call.
    #[test]
    fn keeps_services_whose_pack_is_missing() {
        let mut config = AppConfig {
            services: vec![ServiceConfig {
                id: "test/Orphan".into(),
                name: "Orphan".into(),
                environment: "test".into(),
                pack_id: "removed-pack".into(),
                endpoint: Some("https://example.scm.azurewebsites.net".into()),
                endpoint_manual: true,
                location: None,
                source: ServiceSource::Manual,
            }],
            ..AppConfig::default()
        };
        migrate_for_packs(&mut config, &static_pack());
        assert!(config.services.iter().any(|s| s.id == "test/Orphan"));
    }

    #[test]
    fn normalizes_and_rejects_kudu_urls() {
        assert_eq!(
            normalize_kudu_url("  https://example.scm.azurewebsites.net/  ").unwrap(),
            "https://example.scm.azurewebsites.net"
        );
        assert!(normalize_kudu_url("http://insecure.example").is_err());
        assert!(normalize_kudu_url("   ").is_err());
    }

    #[test]
    fn normalizes_folder_paths_without_requiring_them_to_exist() {
        assert_eq!(
            normalize_folder_path("  C:\\logs\\app\\  ").unwrap(),
            "C:\\logs\\app"
        );
        assert!(normalize_folder_path("   ").is_err());
    }

    #[test]
    fn builds_path_safe_ids_from_environment_names() {
        assert_eq!(environment_id("  Staging EU  ").unwrap(), "staging-eu");
        assert_eq!(environment_id("QA/2").unwrap(), "qa-2");
        assert!(environment_id("  ***  ").is_err());
    }

    #[test]
    fn rejects_service_names_that_are_not_path_segments() {
        assert_eq!(validate_service_name("  WebJobs  ").unwrap(), "WebJobs");
        assert!(validate_service_name("web/jobs").is_err());
        assert!(validate_service_name("").is_err());
    }
}

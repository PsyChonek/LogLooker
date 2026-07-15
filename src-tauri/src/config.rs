use crate::memcache::{self, DEFAULT_MAX_MB};
use crate::profiles::ProfileKind;
use crate::scraper::Environment;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct ConfigState(pub Mutex<AppConfig>);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    #[serde(default)]
    pub saved_queries: Vec<SavedQuery>,
    /// Ids of the built-ins already offered once, so deleting one sticks
    /// instead of it reappearing on the next start
    #[serde(default)]
    pub seeded_builtins: Vec<String>,
    #[serde(default)]
    pub memory_cache: MemoryCacheConfig,
}

/// Keeping cached log files in RAM between searches. Off until asked for — it
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
    /// Came from the status.example.com status page; refresh owns it, so it is not deletable
    #[default]
    Scraped,
    /// Shipped with the app (see `builtin_services`)
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
    pub environment: Environment,
    /// None when the status page had no link; the user can set it manually in the UI
    pub kudu_url: Option<String>,
    /// Set once the user edits the Kudu URL by hand (or adds the service), so a
    /// later status-page refresh leaves their value alone instead of overwriting it
    #[serde(default)]
    pub kudu_url_manual: bool,
    /// None until detection ran; user can override afterwards
    pub profile: Option<ProfileKind>,
    /// Defaulted for configs written before this field existed — those entries
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

pub fn service_id(environment: Environment, name: &str) -> String {
    let env = match environment {
        Environment::Test => "test",
        Environment::Production => "production",
    };
    format!("{env}/{name}")
}

/// WebJobs hosts are not listed on the status page, so ship them as built-ins.
/// Seeded once per id (see `AppConfig::seeded_builtins`) and deletable afterwards.
pub fn builtin_services() -> Vec<ServiceConfig> {
    let hosts = [
        (Environment::Test, "WebJobs", "https://Example-test-webjobs"),
        (
            Environment::Test,
            "WebJobsCompany",
            "https://Example-test-webjobs-company",
        ),
        (
            Environment::Production,
            "WebJobs",
            "https://Example-production-webjobs",
        ),
        (
            Environment::Production,
            "WebJobsCompany",
            "https://Example-production-webjobs-company",
        ),
    ];
    hosts
        .into_iter()
        .map(|(environment, name, host)| ServiceConfig {
            id: service_id(environment, name),
            name: name.to_string(),
            environment,
            kudu_url: Some(format!("{host}.scm.azurewebsites.net")),
            kudu_url_manual: false,
            profile: None,
            source: ServiceSource::Builtin,
        })
        .collect()
}

/// Adds built-ins that have never been seeded. Returns whether anything changed.
fn seed_builtin_services(config: &mut AppConfig) -> bool {
    let mut changed = false;
    for service in builtin_services() {
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

pub fn sort_services(config: &mut AppConfig) {
    config
        .services
        .sort_by(|a, b| a.id.to_lowercase().cmp(&b.id.to_lowercase()));
}

/// Canonical form for stored Kudu URLs: https, no trailing slash. Display
/// shortening and duplicate checks assume it.
pub fn normalize_kudu_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Kudu URL cannot be empty".into());
    }
    if !trimmed.starts_with("https://") {
        return Err(format!(
            "Kudu URL must start with https:// — got \"{trimmed}\""
        ));
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
            "Service name cannot contain any of / \\ < > : \" | ? * — got \"{trimmed}\""
        ));
    }
    Ok(trimmed.to_string())
}

fn config_path() -> Result<PathBuf, String> {
    let dir = dirs::config_dir()
        .ok_or("Cannot resolve config directory")?
        .join("LogLooker");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create config dir: {e}"))?;
    Ok(dir.join("config.json"))
}

pub fn load_config() -> AppConfig {
    let Ok(path) = config_path() else {
        return AppConfig::default();
    };
    let mut config: AppConfig = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    };
    config.memory_cache = config.memory_cache.normalized();
    if seed_builtin_services(&mut config) {
        save_config(&config).ok();
    }
    config
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let path = config_path()?;
    let text = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("Cannot write config: {e}"))
}

/// Built-in search presets, shown alongside the user's saved queries.
pub fn preset_queries() -> Vec<SavedQuery> {
    vec![
        SavedQuery {
            name: "Post operations by login".into(),
            query: r"Handling \w*Post\w* ID_Login:".into(),
            is_regex: true,
        },
        SavedQuery {
            name: "All MediatR operations".into(),
            query: r"Handling \w+ ID_Login:".into(),
            is_regex: true,
        },
        SavedQuery {
            name: "Errors".into(),
            query: r"ERROR|Exception".into(),
            is_regex: true,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_builtins_into_an_empty_config() {
        let mut config = AppConfig::default();
        assert!(seed_builtin_services(&mut config));
        assert_eq!(config.seeded_builtins.len(), 4);
        assert!(config
            .services
            .iter()
            .all(|s| s.source == ServiceSource::Builtin));

        let seeded: Vec<_> = config
            .services
            .iter()
            .map(|s| (s.id.as_str(), s.kudu_url.as_deref().unwrap()))
            .collect();
        assert_eq!(
            seeded,
            vec![
                (
                    "production/WebJobs",
                    "https://Example-production-webjobs.scm.azurewebsites.net"
                ),
                (
                    "production/WebJobsCompany",
                    "https://Example-production-webjobs-company.scm.azurewebsites.net"
                ),
                (
                    "test/WebJobs",
                    "https://Example-test-webjobs.scm.azurewebsites.net"
                ),
                (
                    "test/WebJobsCompany",
                    "https://Example-test-webjobs-company.scm.azurewebsites.net"
                ),
            ]
        );
    }

    #[test]
    fn does_not_reseed_a_deleted_builtin() {
        let mut config = AppConfig::default();
        seed_builtin_services(&mut config);
        config.services.retain(|s| s.id != "test/WebJobs");

        assert!(!seed_builtin_services(&mut config));
        assert!(!config.services.iter().any(|s| s.id == "test/WebJobs"));
    }

    #[test]
    fn seeding_keeps_an_existing_service_with_the_same_id() {
        let mut config = AppConfig {
            services: vec![ServiceConfig {
                id: "test/WebJobs".into(),
                name: "WebJobs".into(),
                environment: Environment::Test,
                kudu_url: Some("https://custom.scm.azurewebsites.net".into()),
                kudu_url_manual: true,
                profile: Some(ProfileKind::CoreApplogs),
                source: ServiceSource::Manual,
            }],
            ..AppConfig::default()
        };
        seed_builtin_services(&mut config);

        let existing: Vec<_> = config
            .services
            .iter()
            .filter(|s| s.id == "test/WebJobs")
            .collect();
        assert_eq!(existing.len(), 1);
        assert_eq!(
            existing[0].kudu_url.as_deref(),
            Some("https://custom.scm.azurewebsites.net")
        );
    }

    #[test]
    fn normalizes_and_rejects_kudu_urls() {
        assert_eq!(
            normalize_kudu_url("  https://Example-test-webjobs.scm.azurewebsites.net/  ").unwrap(),
            "https://Example-test-webjobs.scm.azurewebsites.net"
        );
        assert!(normalize_kudu_url("http://insecure.example").is_err());
        assert!(normalize_kudu_url("   ").is_err());
    }

    #[test]
    fn rejects_service_names_that_are_not_path_segments() {
        assert_eq!(validate_service_name("  WebJobs  ").unwrap(), "WebJobs");
        assert!(validate_service_name("web/jobs").is_err());
        assert!(validate_service_name("").is_err());
    }
}

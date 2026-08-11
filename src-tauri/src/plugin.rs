//! Plugin packs - the declarative description of a family of log sources.
//!
//! A pack is one JSON file saying where services come from, where each one
//! keeps its logs, how its lines parse, and what to search and chart by
//! default. Nothing in a pack executes: every extension point is data that
//! compiles to a regex or an enum at load, which is what keeps third-party
//! packs safe to install and keeps the per-line scan running at native speed.
//!
//! Packs load from two places, bundled first and user second: `BUNDLED`,
//! embedded from `src-tauri/plugins/*.json`, then `*.json` in
//! `<config_dir>/LogLooker/plugins/`. A user pack with the id of a bundled one
//! replaces it, so a shipped pack can be corrected locally without a release.
//!
//! Several packs are active at once and their capabilities union. Field keys are
//! therefore namespaced as `<packId>.<fieldKey>`: two packs may both extract
//! something called `operation` without one silently standing in for the other.
//! Environment ids are *not* namespaced - a TEST toggle is expected to show
//! every pack's test services together.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Hits record their fields in one column per declared field, so the count is a
/// per-hit memory cost paid by every search. High enough for any real log shape,
/// low enough that a careless pack cannot blow up the hit budget.
pub const MAX_FIELDS_PER_PACK: usize = 32;

/// Bundled packs, embedded so a fresh install has working defaults with no
/// files to place. Add new ones here; there is no compile-time directory glob.
const BUNDLED: &[(&str, &str)] = &[
    ("azure-kudu", include_str!("../plugins/azure-kudu.json")),
    ("local-folder", include_str!("../plugins/local-folder.json")),
];

// --- Manifest: the JSON as authored -----------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackManifest {
    /// Namespace for this pack's field keys and the id services bind to
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Defaults to a single `default` environment when omitted
    #[serde(default)]
    pub environments: Vec<EnvironmentDef>,
    pub source: SourceDef,
    #[serde(default)]
    pub discovery: DiscoveryDef,
    #[serde(default)]
    pub log_locations: Vec<LogLocationDef>,
    #[serde(default)]
    pub timestamps: Vec<TimestampDef>,
    #[serde(default)]
    pub fields: Vec<FieldDef>,
    #[serde(default)]
    pub presets: Vec<PresetDef>,
    #[serde(default)]
    pub charts: Vec<ChartPresetDef>,
    /// Ignored by the app; lets a pack carry a `$schema` for editor completion
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentDef {
    /// Also a cache directory segment, hence the restricted character set
    pub id: String,
    pub label: String,
}

/// How the app reaches the log files. The transport list is closed - a pack
/// picks one and configures it, it cannot supply code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SourceDef {
    /// Azure App Service Kudu VFS over HTTPS, read-only GETs
    Kudu {
        #[serde(default)]
        auth: KuduAuth,
    },
    /// A directory on this machine or a mounted share
    LocalFolder {
        /// Prepended to a service's relative path; absolute paths ignore it
        #[serde(default)]
        root: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KuduAuth {
    /// Bearer token from `az account get-access-token`; needs `az login`
    #[default]
    AzCli,
}

/// Where the service list comes from. Discovery only ever fills in an editable
/// list - a pack that cannot discover anything is perfectly usable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DiscoveryDef {
    /// Services are added by hand
    #[default]
    None,
    /// Services listed in the pack itself, seeded once and deletable afterwards
    Static { services: Vec<StaticServiceDef> },
    /// Parsed out of an HTML page: one `container` match per service, then the
    /// per-service regexes are run over the block up to the next container
    Scrape(ScrapeDef),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StaticServiceDef {
    pub name: String,
    pub environment: String,
    /// Kudu base URL or filesystem path, depending on the pack's source
    pub endpoint: String,
    #[serde(default)]
    pub location: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScrapeDef {
    pub url: String,
    /// Button text, e.g. "Refresh from status.example.com"
    #[serde(default)]
    pub label: Option<String>,
    /// Run over the whole page; needs a `name` group, may have a `section` one
    pub container: String,
    /// Run over one service's block; group `endpoint` or group 1 is the URL
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Run over one service's block; group `dir` or group 1 is matched against
    /// the log locations' `dir` to preselect one
    #[serde(default)]
    pub log_dir: Option<String>,
    /// First rule whose `contains` appears in the container's `section` group
    /// wins; nothing matching falls back to `defaultEnvironment`
    #[serde(default)]
    pub environments: Vec<ScrapeEnvRule>,
    #[serde(default)]
    pub default_environment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScrapeEnvRule {
    /// Matched case-insensitively against the section text
    pub contains: String,
    pub environment: String,
}

/// Where one service keeps its logs, and how the file names carry their dates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogLocationDef {
    pub id: String,
    pub label: String,
    /// Directory relative to the source root, e.g. `applogs`. `.` is the root.
    pub dir: String,
    /// File name regex. Prefix `(?i)` for a case-insensitive name - Windows
    /// hosts are case-insensitive about theirs. An optional `instance` group
    /// marks a scale-out instance id.
    pub file: String,
    /// Whether the file name carries the day it covers. When true (the default)
    /// `file` needs `y`/`m`/`d` groups and a date range picks files by name. When
    /// false the files are undated and growing, so everything matching is
    /// fetched and the range can only be applied to the line timestamps.
    #[serde(default = "default_dated")]
    pub dated: bool,
}

fn default_dated() -> bool {
    true
}

/// Line-start timestamp formats to try, in order. The named ones are hand-rolled
/// byte parsers; `custom` runs a regex on every line and is measurably slower.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TimestampDef {
    Builtin(BuiltinTimestamp),
    Custom {
        /// Needs groups `y`,`m`,`d`,`H`,`M`,`S`; `f` (fractional) is optional
        regex: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuiltinTimestamp {
    /// `2026-07-15T08:08:09.9533870Z`, also accepts a space instead of the T
    Iso8601,
    /// `13.07.2026 00:00:02.611`, fractional part optional
    DottedDmy,
    /// `7/14/2026 1:15:55 AM`
    UsMdy,
}

/// One value pulled out of a matching line. Extraction runs on every hit, so a
/// field with a `gate` costs a substring test on the lines that lack it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FieldDef {
    /// Local key; the app addresses it as `<packId>.<key>`
    pub key: String,
    pub label: String,
    /// Literal the line must contain before the regex runs
    #[serde(default)]
    pub gate: Option<String>,
    /// Value comes from group `v`, or group 1 when there is no `v`
    pub regex: String,
    #[serde(rename = "type", default)]
    pub kind: FieldType,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldType {
    /// Stored as 16 bytes rather than text; a value that is not a GUID is dropped
    Guid,
    #[default]
    String,
    /// Chartable as a metric; a value that will not parse is dropped
    Number,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PresetDef {
    pub name: String,
    pub query: String,
    #[serde(default)]
    pub is_regex: bool,
}

/// A chart the pack offers ready-made. `groupBy` and `value` take `field:<key>`
/// to name one of the pack's own fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChartPresetDef {
    pub name: String,
    /// `timeline` or `category`
    #[serde(default = "default_chart_mode")]
    pub mode: String,
    /// `auto`, `second`, `minute`, `fiveMinutes`, `fifteenMinutes`, `hour`,
    /// `sixHours`, `day`
    #[serde(default = "default_bucket")]
    pub bucket: String,
    /// `none`, `service`, `file`, `matchedGroup`, `custom`, or `field:<key>`
    #[serde(default = "default_group_by")]
    pub group_by: String,
    /// Required by `groupBy: custom` and `value: custom`: the category comes
    /// from the `key` group, the number from the `value` group
    #[serde(default)]
    pub custom_regex: Option<String>,
    /// `count`, `sum`, `avg`, `min`, `max`, `p50`, `p95`, `p99`
    #[serde(default = "default_metric")]
    pub metric: String,
    /// Where a non-count metric's number comes from: `field:<key>` or `custom`
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub top_n: Option<usize>,
}

fn default_chart_mode() -> String {
    "timeline".into()
}
fn default_bucket() -> String {
    "auto".into()
}
fn default_group_by() -> String {
    "none".into()
}
fn default_metric() -> String {
    "count".into()
}

// --- Compiled form: what the app runs against -------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PackOrigin {
    /// Shipped with the app
    Bundled,
    /// Loaded from the user's plugins directory
    User { path: String },
}

/// A validated pack: the manifest as authored plus everything that had to
/// compile. Held behind an `Arc` so a search can take a snapshot and scan
/// without holding a lock.
#[derive(Debug)]
pub struct Pack {
    pub manifest: PackManifest,
    pub origin: PackOrigin,
    /// A bundled pack this one replaced
    pub overrides_bundled: bool,
    pub fields: Vec<CompiledField>,
    pub locations: Vec<CompiledLocation>,
    pub timestamps: Vec<CompiledTimestamp>,
    pub scrape: Option<CompiledScrape>,
}

#[derive(Debug, Clone)]
pub struct CompiledField {
    /// `<packId>.<key>` - what chart requests, column settings and hits use
    pub key: String,
    pub local_key: String,
    pub label: String,
    pub gate: Option<String>,
    pub regex: Regex,
    pub kind: FieldType,
}

#[derive(Debug)]
pub struct CompiledLocation {
    pub id: String,
    pub label: String,
    pub dir: String,
    pub file: Regex,
    pub dated: bool,
}

#[derive(Debug, Clone)]
pub enum CompiledTimestamp {
    Builtin(BuiltinTimestamp),
    Custom(Regex),
}

#[derive(Debug)]
pub struct CompiledScrape {
    pub def: ScrapeDef,
    pub container: Regex,
    pub endpoint: Option<Regex>,
    pub log_dir: Option<Regex>,
}

impl Pack {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }

    pub fn location(&self, id: &str) -> Option<&CompiledLocation> {
        self.locations.iter().find(|l| l.id == id)
    }

    /// The location whose `dir` matches a VFS path a scrape turned up, so a
    /// status page that links straight at the log folder saves a probe.
    pub fn location_by_dir(&self, dir: &str) -> Option<&CompiledLocation> {
        let wanted = dir.trim_matches('/').to_lowercase();
        self.locations
            .iter()
            .find(|l| l.dir.trim_matches('/').to_lowercase() == wanted)
    }
}

/// A pack that would not load, kept so the UI can say which file and why
/// instead of the pack silently not being there.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackError {
    /// File name or embedded pack id
    pub source: String,
    pub message: String,
}

#[derive(Default)]
pub struct Registry {
    pub packs: Vec<Arc<Pack>>,
    pub errors: Vec<PackError>,
}

impl Registry {
    pub fn pack(&self, id: &str) -> Option<&Arc<Pack>> {
        self.packs.iter().find(|p| p.id() == id)
    }

    /// Environments across all packs, first label per id winning. Ids are shared
    /// on purpose: one TEST toggle covers every pack's test services.
    pub fn environments(&self) -> Vec<EnvironmentDef> {
        let mut out: Vec<EnvironmentDef> = Vec::new();
        for pack in &self.packs {
            for env in &pack.manifest.environments {
                if !out.iter().any(|e| e.id == env.id) {
                    out.push(env.clone());
                }
            }
        }
        out
    }

    pub fn has_environment(&self, id: &str) -> bool {
        self.packs
            .iter()
            .any(|p| p.manifest.environments.iter().any(|e| e.id == id))
    }

    /// Every declared field, namespaced, in pack then manifest order. This order
    /// is the column order of a search result, so it has to be stable.
    pub fn fields(&self) -> Vec<&CompiledField> {
        self.packs.iter().flat_map(|p| p.fields.iter()).collect()
    }

    pub fn field(&self, key: &str) -> Option<&CompiledField> {
        self.fields().into_iter().find(|f| f.key == key)
    }

    /// Presets from every pack, prefixed with the pack name when more than one
    /// pack is loaded so identically named presets stay tellable apart.
    pub fn presets(&self) -> Vec<(String, &PresetDef)> {
        let prefix = self.packs.len() > 1;
        self.packs
            .iter()
            .flat_map(|p| {
                p.manifest.presets.iter().map(move |preset| {
                    let name = if prefix {
                        format!("{}: {}", p.manifest.name, preset.name)
                    } else {
                        preset.name.clone()
                    };
                    (name, preset)
                })
            })
            .collect()
    }

    pub fn charts(&self) -> Vec<(&Arc<Pack>, &ChartPresetDef)> {
        self.packs
            .iter()
            .flat_map(|p| p.manifest.charts.iter().map(move |c| (p, c)))
            .collect()
    }

    /// The pack a service without a recorded `packId` belongs to. Configs
    /// written before packs existed only ever held Kudu services, so the first
    /// Kudu pack is the right home; any pack beats dropping the service.
    pub fn default_pack_id(&self) -> Option<String> {
        self.packs
            .iter()
            .find(|p| matches!(p.manifest.source, SourceDef::Kudu { .. }))
            .or_else(|| self.packs.first())
            .map(|p| p.manifest.id.clone())
    }
}

/// Snapshot handed to searches and commands. Swapping the `Arc` on reload means
/// a scan already in flight keeps the specs it started with.
pub struct PluginState(pub Mutex<Arc<Registry>>);

impl PluginState {
    pub fn new(registry: Registry) -> Self {
        PluginState(Mutex::new(Arc::new(registry)))
    }

    pub fn snapshot(&self) -> Arc<Registry> {
        self.0
            .lock()
            .map(|r| Arc::clone(&r))
            .unwrap_or_else(|_| Arc::new(Registry::default()))
    }

    pub fn replace(&self, registry: Registry) {
        if let Ok(mut held) = self.0.lock() {
            *held = Arc::new(registry);
        }
    }
}

// --- Loading ----------------------------------------------------------------

pub fn plugins_dir() -> Result<PathBuf, String> {
    let dir = dirs::config_dir()
        .ok_or("Cannot resolve config directory")?
        .join("LogLooker")
        .join("plugins");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create plugins dir: {e}"))?;
    Ok(dir)
}

/// Loads bundled then user packs. `disabled` ids are skipped entirely - a pack
/// the user switched off costs nothing at scan time.
pub fn load(disabled: &[String]) -> Registry {
    let mut registry = Registry::default();

    for (id, text) in BUNDLED {
        if disabled.iter().any(|d| d == id) {
            continue;
        }
        match parse_and_compile(text, PackOrigin::Bundled, false) {
            // A broken bundled pack is our bug, not the user's, but it still has
            // to surface rather than vanish
            Ok(pack) => registry.packs.push(Arc::new(pack)),
            Err(message) => registry.errors.push(PackError {
                source: format!("{id} (bundled)"),
                message,
            }),
        }
    }

    let dir = match plugins_dir() {
        Ok(dir) => dir,
        Err(message) => {
            registry.errors.push(PackError {
                source: "plugins directory".into(),
                message,
            });
            return registry;
        }
    };
    for path in json_files(&dir) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                registry.errors.push(PackError {
                    source: name,
                    message: format!("Cannot read: {e}"),
                });
                continue;
            }
        };
        let origin = PackOrigin::User {
            path: path.display().to_string(),
        };
        let replacing = registry
            .packs
            .iter()
            .position(|p| Some(p.id()) == pack_id_of(&text).as_deref());
        match parse_and_compile(&text, origin, replacing.is_some()) {
            Ok(pack) => {
                if disabled.contains(&pack.manifest.id) {
                    continue;
                }
                match replacing {
                    Some(at) => registry.packs[at] = Arc::new(pack),
                    None => registry.packs.push(Arc::new(pack)),
                }
            }
            Err(message) => registry.errors.push(PackError {
                source: name,
                message,
            }),
        }
    }

    registry
}

/// Sorted so the load order - and with it the column order of a search - does
/// not depend on how the filesystem happens to enumerate the directory.
fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .collect();
    paths.sort();
    paths
}

/// The id alone, to spot a bundled pack being replaced before the full parse.
fn pack_id_of(text: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct JustId {
        id: String,
    }
    serde_json::from_str::<JustId>(text).ok().map(|j| j.id)
}

pub fn parse_and_compile(
    text: &str,
    origin: PackOrigin,
    overrides_bundled: bool,
) -> Result<Pack, String> {
    let manifest: PackManifest =
        serde_json::from_str(text).map_err(|e| format!("Invalid pack JSON: {e}"))?;
    compile(manifest, origin, overrides_bundled)
}

pub fn compile(
    mut manifest: PackManifest,
    origin: PackOrigin,
    overrides_bundled: bool,
) -> Result<Pack, String> {
    validate_id(&manifest.id).map_err(|e| format!("id: {e}"))?;
    if manifest.name.trim().is_empty() {
        return Err("name: cannot be empty".into());
    }

    if manifest.environments.is_empty() {
        manifest.environments.push(EnvironmentDef {
            id: "default".into(),
            label: "Default".into(),
        });
    }
    for (i, env) in manifest.environments.iter().enumerate() {
        validate_id(&env.id).map_err(|e| format!("environments[{i}].id: {e}"))?;
        if manifest.environments.iter().filter(|e| e.id == env.id).count() > 1 {
            return Err(format!("environments: duplicate id \"{}\"", env.id));
        }
    }

    let locations = compile_locations(&manifest)?;
    let timestamps = compile_timestamps(&manifest)?;
    let fields = compile_fields(&manifest)?;
    let scrape = compile_scrape(&manifest, &locations)?;
    validate_discovery(&manifest, &locations)?;
    validate_charts(&manifest, &fields)?;

    Ok(Pack {
        manifest,
        origin,
        overrides_bundled,
        fields,
        locations,
        timestamps,
        scrape,
    })
}

fn compile_locations(manifest: &PackManifest) -> Result<Vec<CompiledLocation>, String> {
    let mut out = Vec::with_capacity(manifest.log_locations.len());
    for (i, def) in manifest.log_locations.iter().enumerate() {
        validate_id(&def.id).map_err(|e| format!("logLocations[{i}].id: {e}"))?;
        if manifest
            .log_locations
            .iter()
            .filter(|l| l.id == def.id)
            .count()
            > 1
        {
            return Err(format!("logLocations: duplicate id \"{}\"", def.id));
        }
        let file = Regex::new(&def.file).map_err(|e| format!("logLocations[{i}].file: {e}"))?;
        if def.dated {
            // Without a date in the name there is nothing for a date range to
            // select on, which is what `dated: false` exists to say
            let names: Vec<_> = file.capture_names().flatten().collect();
            let missing: Vec<&str> = ["y", "m", "d"]
                .into_iter()
                .filter(|g| !names.contains(g))
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "logLocations[{i}].file: missing named group(s) {} - a dated location picks \
                     files by the date in the name; set \"dated\": false for undated files",
                    missing.join(", ")
                ));
            }
        }
        out.push(CompiledLocation {
            id: def.id.clone(),
            label: def.label.clone(),
            dir: def.dir.clone(),
            file,
            dated: def.dated,
        });
    }
    Ok(out)
}

fn compile_timestamps(manifest: &PackManifest) -> Result<Vec<CompiledTimestamp>, String> {
    let mut out = Vec::with_capacity(manifest.timestamps.len());
    for (i, def) in manifest.timestamps.iter().enumerate() {
        match def {
            TimestampDef::Builtin(kind) => out.push(CompiledTimestamp::Builtin(*kind)),
            TimestampDef::Custom { regex } => {
                let re =
                    Regex::new(regex).map_err(|e| format!("timestamps[{i}].regex: {e}"))?;
                let names: Vec<_> = re.capture_names().flatten().collect();
                let missing: Vec<&str> = ["y", "m", "d", "H", "M", "S"]
                    .into_iter()
                    .filter(|g| !names.contains(g))
                    .collect();
                if !missing.is_empty() {
                    return Err(format!(
                        "timestamps[{i}].regex: missing named group(s) {}",
                        missing.join(", ")
                    ));
                }
                out.push(CompiledTimestamp::Custom(re));
            }
        }
    }
    Ok(out)
}

fn compile_fields(manifest: &PackManifest) -> Result<Vec<CompiledField>, String> {
    if manifest.fields.len() > MAX_FIELDS_PER_PACK {
        return Err(format!(
            "fields: {} declared, at most {MAX_FIELDS_PER_PACK} are supported",
            manifest.fields.len()
        ));
    }
    let mut out = Vec::with_capacity(manifest.fields.len());
    for (i, def) in manifest.fields.iter().enumerate() {
        validate_field_key(&def.key).map_err(|e| format!("fields[{i}].key: {e}"))?;
        if manifest.fields.iter().filter(|f| f.key == def.key).count() > 1 {
            return Err(format!("fields: duplicate key \"{}\"", def.key));
        }
        let regex = Regex::new(&def.regex).map_err(|e| format!("fields[{i}].regex: {e}"))?;
        let has_value_group = regex.capture_names().flatten().any(|n| n == "v");
        if !has_value_group && regex.captures_len() < 2 {
            return Err(format!(
                "fields[{i}].regex: needs a capture group for the value - either (?<v>...) \
                 or a first plain group"
            ));
        }
        if let Some(gate) = &def.gate {
            if gate.is_empty() {
                return Err(format!(
                    "fields[{i}].gate: cannot be empty - omit it instead"
                ));
            }
        }
        out.push(CompiledField {
            key: format!("{}.{}", manifest.id, def.key),
            local_key: def.key.clone(),
            label: def.label.clone(),
            gate: def.gate.clone(),
            regex,
            kind: def.kind,
        });
    }
    Ok(out)
}

fn compile_scrape(
    manifest: &PackManifest,
    locations: &[CompiledLocation],
) -> Result<Option<CompiledScrape>, String> {
    let DiscoveryDef::Scrape(def) = &manifest.discovery else {
        return Ok(None);
    };
    if !def.url.starts_with("https://") && !def.url.starts_with("http://") {
        return Err("discovery.url: must be an http(s) URL".into());
    }
    let container =
        Regex::new(&def.container).map_err(|e| format!("discovery.container: {e}"))?;
    if !container.capture_names().flatten().any(|n| n == "name") {
        return Err("discovery.container: needs a (?<name>...) group for the service name".into());
    }
    let has_section = container.capture_names().flatten().any(|n| n == "section");
    if !def.environments.is_empty() && !has_section {
        return Err(
            "discovery.environments: needs a (?<section>...) group in discovery.container to \
             match against"
                .into(),
        );
    }
    let endpoint = def
        .endpoint
        .as_deref()
        .map(Regex::new)
        .transpose()
        .map_err(|e| format!("discovery.endpoint: {e}"))?;
    let log_dir = def
        .log_dir
        .as_deref()
        .map(Regex::new)
        .transpose()
        .map_err(|e| format!("discovery.logDir: {e}"))?;
    if log_dir.is_some() && locations.is_empty() {
        return Err(
            "discovery.logDir: there are no logLocations for a matched directory to select".into(),
        );
    }
    for (i, rule) in def.environments.iter().enumerate() {
        if !manifest
            .environments
            .iter()
            .any(|e| e.id == rule.environment)
        {
            return Err(format!(
                "discovery.environments[{i}].environment: \"{}\" is not a declared environment",
                rule.environment
            ));
        }
    }
    if let Some(env) = &def.default_environment {
        if !manifest.environments.iter().any(|e| &e.id == env) {
            return Err(format!(
                "discovery.defaultEnvironment: \"{env}\" is not a declared environment"
            ));
        }
    }
    Ok(Some(CompiledScrape {
        def: def.clone(),
        container,
        endpoint,
        log_dir,
    }))
}

fn validate_discovery(
    manifest: &PackManifest,
    locations: &[CompiledLocation],
) -> Result<(), String> {
    let DiscoveryDef::Static { services } = &manifest.discovery else {
        return Ok(());
    };
    for (i, service) in services.iter().enumerate() {
        if service.name.trim().is_empty() {
            return Err(format!("discovery.services[{i}].name: cannot be empty"));
        }
        if !manifest
            .environments
            .iter()
            .any(|e| e.id == service.environment)
        {
            return Err(format!(
                "discovery.services[{i}].environment: \"{}\" is not a declared environment",
                service.environment
            ));
        }
        if let Some(location) = &service.location {
            if !locations.iter().any(|l| &l.id == location) {
                return Err(format!(
                    "discovery.services[{i}].location: \"{location}\" is not a declared log location"
                ));
            }
        }
    }
    Ok(())
}

const CHART_MODES: &[&str] = &["timeline", "category"];
const BUCKETS: &[&str] = &[
    "auto",
    "second",
    "minute",
    "fiveMinutes",
    "fifteenMinutes",
    "hour",
    "sixHours",
    "day",
];
const METRICS: &[&str] = &["count", "sum", "avg", "min", "max", "p50", "p95", "p99"];
const PLAIN_GROUP_BYS: &[&str] = &["none", "service", "file", "matchedGroup", "custom"];

/// The reference a chart preset makes to one of the pack's fields.
pub fn field_reference(value: &str) -> Option<&str> {
    value.strip_prefix("field:")
}

fn validate_charts(manifest: &PackManifest, fields: &[CompiledField]) -> Result<(), String> {
    let known_field = |local: &str| fields.iter().any(|f| f.local_key == local);

    for (i, chart) in manifest.charts.iter().enumerate() {
        if chart.name.trim().is_empty() {
            return Err(format!("charts[{i}].name: cannot be empty"));
        }
        if !CHART_MODES.contains(&chart.mode.as_str()) {
            return Err(format!(
                "charts[{i}].mode: \"{}\" is not one of {}",
                chart.mode,
                CHART_MODES.join(", ")
            ));
        }
        if !BUCKETS.contains(&chart.bucket.as_str()) {
            return Err(format!(
                "charts[{i}].bucket: \"{}\" is not one of {}",
                chart.bucket,
                BUCKETS.join(", ")
            ));
        }
        if !METRICS.contains(&chart.metric.as_str()) {
            return Err(format!(
                "charts[{i}].metric: \"{}\" is not one of {}",
                chart.metric,
                METRICS.join(", ")
            ));
        }
        match field_reference(&chart.group_by) {
            Some(local) if !known_field(local) => {
                return Err(format!(
                    "charts[{i}].groupBy: no field \"{local}\" in this pack"
                ))
            }
            None if !PLAIN_GROUP_BYS.contains(&chart.group_by.as_str()) => {
                return Err(format!(
                    "charts[{i}].groupBy: \"{}\" is not one of {} or field:<key>",
                    chart.group_by,
                    PLAIN_GROUP_BYS.join(", ")
                ))
            }
            _ => {}
        }
        let counts = chart.metric == "count";
        match chart.value.as_deref() {
            Some("custom") | None => {}
            Some(value) => match field_reference(value) {
                Some(local) if known_field(local) => {
                    let numeric = fields
                        .iter()
                        .find(|f| f.local_key == local)
                        .is_some_and(|f| f.kind == FieldType::Number);
                    if !numeric {
                        return Err(format!(
                            "charts[{i}].value: field \"{local}\" is not a number field"
                        ));
                    }
                }
                Some(local) => {
                    return Err(format!("charts[{i}].value: no field \"{local}\" in this pack"))
                }
                None => {
                    return Err(format!(
                        "charts[{i}].value: \"{value}\" must be field:<key> or custom"
                    ))
                }
            },
        }
        if !counts && chart.value.is_none() {
            return Err(format!(
                "charts[{i}].value: metric \"{}\" needs a number to aggregate",
                chart.metric
            ));
        }
        let needs_regex =
            chart.group_by == "custom" || chart.value.as_deref() == Some("custom");
        if needs_regex && chart.custom_regex.is_none() {
            return Err(format!(
                "charts[{i}].customRegex: required by \"custom\" groupBy or value"
            ));
        }
        if let Some(pattern) = &chart.custom_regex {
            Regex::new(pattern).map_err(|e| format!("charts[{i}].customRegex: {e}"))?;
        }
    }
    Ok(())
}

/// Pack, environment and location ids end up in file paths and in composite
/// keys, so they stay lowercase path-safe slugs.
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("cannot be empty".into());
    }
    let shaped = id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !shaped {
        return Err(format!(
            "\"{id}\" must be lowercase letters, digits and hyphens, starting with a letter or digit"
        ));
    }
    Ok(())
}

/// Field keys are addressed as `<packId>.<key>`, so a dot in the key itself
/// would make the composite ambiguous.
fn validate_field_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("cannot be empty".into());
    }
    let shaped = key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    if !shaped {
        return Err(format!(
            "\"{key}\" must be letters, digits and underscores, starting with a letter"
        ));
    }
    Ok(())
}

// --- UI-facing description --------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldInfo {
    pub key: String,
    pub label: String,
    pub pack_id: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub origin: PackOrigin,
    pub overrides_bundled: bool,
    pub environments: Vec<EnvironmentDef>,
    /// `kudu`, `local-folder`
    pub source_type: String,
    /// `none`, `static`, `scrape`
    pub discovery_type: String,
    /// Button text for a scrape refresh; None when the pack cannot discover
    pub discovery_label: Option<String>,
    pub locations: Vec<LocationInfo>,
    pub fields: Vec<FieldInfo>,
    pub presets: Vec<PresetDef>,
    pub charts: Vec<ChartPresetDef>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationInfo {
    pub id: String,
    pub label: String,
    pub dir: String,
    pub dated: bool,
}

/// Everything the UI needs about what is loaded, including what failed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginsInfo {
    pub packs: Vec<PackInfo>,
    pub errors: Vec<PackError>,
    pub disabled: Vec<String>,
    pub plugins_dir: String,
    /// Ids of the bundled packs, whether loaded or disabled, so the UI can offer
    /// a disabled bundled pack back
    pub bundled: Vec<String>,
}

impl Pack {
    pub fn info(&self) -> PackInfo {
        let m = &self.manifest;
        PackInfo {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.clone(),
            description: m.description.clone(),
            origin: self.origin.clone(),
            overrides_bundled: self.overrides_bundled,
            environments: m.environments.clone(),
            source_type: match m.source {
                SourceDef::Kudu { .. } => "kudu".into(),
                SourceDef::LocalFolder { .. } => "local-folder".into(),
            },
            discovery_type: match m.discovery {
                DiscoveryDef::None => "none".into(),
                DiscoveryDef::Static { .. } => "static".into(),
                DiscoveryDef::Scrape(_) => "scrape".into(),
            },
            discovery_label: match &m.discovery {
                DiscoveryDef::Scrape(def) => Some(
                    def.label
                        .clone()
                        .unwrap_or_else(|| format!("Refresh from {}", def.url)),
                ),
                _ => None,
            },
            locations: self
                .locations
                .iter()
                .map(|l| LocationInfo {
                    id: l.id.clone(),
                    label: l.label.clone(),
                    dir: l.dir.clone(),
                    dated: l.dated,
                })
                .collect(),
            fields: self
                .fields
                .iter()
                .map(|f| FieldInfo {
                    key: f.key.clone(),
                    label: f.label.clone(),
                    pack_id: m.id.clone(),
                    kind: f.kind,
                })
                .collect(),
            presets: m.presets.clone(),
            charts: m.charts.clone(),
        }
    }
}

pub fn bundled_ids() -> Vec<String> {
    BUNDLED.iter().map(|(id, _)| (*id).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> String {
        format!(
            r#"{{
                "id": "test-pack",
                "name": "Test pack",
                "source": {{ "type": "kudu" }}
                {extra}
            }}"#
        )
    }

    fn compile_text(text: &str) -> Result<Pack, String> {
        parse_and_compile(text, PackOrigin::Bundled, false)
    }

    #[test]
    fn every_bundled_pack_compiles() {
        for (id, text) in BUNDLED {
            compile_text(text).unwrap_or_else(|e| panic!("bundled pack {id} does not load: {e}"));
        }
    }

    #[test]
    fn defaults_a_single_environment_when_none_is_declared() {
        let pack = compile_text(&manifest("")).unwrap();
        assert_eq!(pack.manifest.environments.len(), 1);
        assert_eq!(pack.manifest.environments[0].id, "default");
    }

    #[test]
    fn namespaces_field_keys_by_pack() {
        let pack = compile_text(&manifest(
            r#", "fields": [
                 { "key": "idLogin", "label": "Login", "gate": "ID_Login",
                   "type": "guid", "regex": "ID_Login[:=](?<v>[0-9a-fA-F-]{36})" }
               ]"#,
        ))
        .unwrap();
        assert_eq!(pack.fields[0].key, "test-pack.idLogin");
        assert_eq!(pack.fields[0].local_key, "idLogin");
    }

    #[test]
    fn rejects_a_field_regex_with_no_capture_group() {
        let err = compile_text(&manifest(
            r#", "fields": [ { "key": "op", "label": "Op", "regex": "Handling \\w+" } ]"#,
        ))
        .unwrap_err();
        assert!(err.contains("needs a capture group"), "{err}");
    }

    #[test]
    fn rejects_a_dated_file_regex_without_date_groups() {
        let err = compile_text(&manifest(
            r#", "logLocations": [
                 { "id": "logs", "label": "Logs", "dir": "logs", "file": "^log-.*\\.log$" }
               ]"#,
        ))
        .unwrap_err();
        assert!(err.contains("missing named group(s) y, m, d"), "{err}");
    }

    #[test]
    fn accepts_an_undated_growing_file() {
        let pack = compile_text(&manifest(
            r#", "logLocations": [
                 { "id": "app-data", "label": "App_Data", "dir": "site/wwwroot/App_Data",
                   "file": "(?i)^Log\\.txt$", "dated": false }
               ]"#,
        ))
        .unwrap();
        assert!(!pack.locations[0].dated);
        assert!(pack.locations[0].file.is_match("log.TXT"));
    }

    #[test]
    fn matches_a_location_by_the_directory_a_scrape_linked_at() {
        let pack = compile_text(&manifest(
            r#", "logLocations": [
                 { "id": "applogs", "label": "App logs", "dir": "applogs",
                   "file": "^log-(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})\\.log$" }
               ]"#,
        ))
        .unwrap();
        assert_eq!(pack.location_by_dir("/APPLOGS/").map(|l| l.id.as_str()), Some("applogs"));
        assert!(pack.location_by_dir("LogFiles").is_none());
    }

    #[test]
    fn rejects_a_chart_grouping_by_an_unknown_field() {
        let err = compile_text(&manifest(
            r#", "charts": [ { "name": "By op", "groupBy": "field:operation" } ]"#,
        ))
        .unwrap_err();
        assert!(err.contains("no field \"operation\""), "{err}");
    }

    #[test]
    fn rejects_a_non_count_chart_metric_with_nothing_to_aggregate() {
        let err = compile_text(&manifest(
            r#", "charts": [ { "name": "Avg", "metric": "avg" } ]"#,
        ))
        .unwrap_err();
        assert!(err.contains("needs a number to aggregate"), "{err}");
    }

    #[test]
    fn rejects_aggregating_a_non_numeric_field() {
        let err = compile_text(&manifest(
            r#", "fields": [ { "key": "op", "label": "Op", "regex": "Handling (\\w+)" } ],
                "charts": [ { "name": "Avg", "metric": "avg", "value": "field:op" } ]"#,
        ))
        .unwrap_err();
        assert!(err.contains("not a number field"), "{err}");
    }

    #[test]
    fn rejects_scrape_environment_rules_without_a_section_group() {
        let err = compile_text(&manifest(
            r#", "environments": [ { "id": "test", "label": "TEST" } ],
                "discovery": {
                  "type": "scrape", "url": "https://example.com/",
                  "container": "data-page-name=\"(?<name>[^\"]*)\"",
                  "environments": [ { "contains": "prod", "environment": "test" } ]
                }"#,
        ))
        .unwrap_err();
        assert!(err.contains("section"), "{err}");
    }

    #[test]
    fn rejects_an_unknown_environment_in_a_static_service() {
        let err = compile_text(&manifest(
            r#", "environments": [ { "id": "test", "label": "TEST" } ],
                "discovery": { "type": "static", "services": [
                  { "name": "WebJobs", "environment": "staging",
                    "endpoint": "https://example.scm.azurewebsites.net" }
                ] }"#,
        ))
        .unwrap_err();
        assert!(err.contains("not a declared environment"), "{err}");
    }

    #[test]
    fn rejects_an_unknown_manifest_key_so_typos_surface() {
        let err = compile_text(&manifest(r#", "logLocation": []"#)).unwrap_err();
        assert!(err.contains("unknown field"), "{err}");
    }

    #[test]
    fn registry_unions_environments_by_id_keeping_the_first_label() {
        let a = compile_text(
            r#"{ "id": "a", "name": "A", "source": { "type": "kudu" },
                 "environments": [ { "id": "test", "label": "TEST" } ] }"#,
        )
        .unwrap();
        let b = compile_text(
            r#"{ "id": "b", "name": "B", "source": { "type": "local-folder" },
                 "environments": [ { "id": "test", "label": "Testing" },
                                   { "id": "prod", "label": "PROD" } ] }"#,
        )
        .unwrap();
        let registry = Registry {
            packs: vec![Arc::new(a), Arc::new(b)],
            errors: Vec::new(),
        };

        let envs = registry.environments();
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].id, "test");
        assert_eq!(envs[0].label, "TEST");
        assert_eq!(envs[1].id, "prod");
        assert!(registry.has_environment("prod"));
        assert!(!registry.has_environment("staging"));
        // Configs written before packs existed only held Kudu services
        assert_eq!(registry.default_pack_id().as_deref(), Some("a"));
    }
}

use crate::kudu::KuduClient;
use chrono::NaiveDate;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Where a service keeps its logs. Detected per service by probing the Kudu VFS,
/// stored in config, user-overridable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileKind {
    /// New .NET Core apps (NLog): applogs/log-YYYY-MM-DD.log, one file per day
    CoreApplogs,
    /// Old framework apps: site/wwwroot/App_Data/Log.txt, single growing file
    FrameworkAppData,
    /// Linux container stdout: LogFiles/YYYY_MM_DD_<instance>_default_docker[.N].log
    DockerLogfiles,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteLogFile {
    /// VFS-relative path, e.g. "applogs/log-2026-07-14.log"
    pub vfs_path: String,
    pub name: String,
    pub size: u64,
    pub mtime: String,
    /// Day the file covers; None for undated growing files (Log.txt)
    pub date: Option<NaiveDate>,
    /// Instance id for docker logs; multiple ids in one range means scale-out
    pub instance: Option<String>,
}

impl ProfileKind {
    /// Maps a Kudu VFS log directory (as the status page's "Kudu Logs" link
    /// exposes it, e.g. the `applogs` in `.../filemanager/applogs/`) to the
    /// profile that reads it. Returns None for an unrecognised directory, so
    /// callers fall back to probing the VFS.
    pub fn from_log_dir(vfs_path: &str) -> Option<ProfileKind> {
        match vfs_path.trim_matches('/').to_lowercase().as_str() {
            "applogs" => Some(ProfileKind::CoreApplogs),
            "logfiles" => Some(ProfileKind::DockerLogfiles),
            "site/wwwroot/app_data" => Some(ProfileKind::FrameworkAppData),
            _ => None,
        }
    }
}

fn core_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^log-(\d{4})-(\d{2})-(\d{2})\.log$").unwrap())
}

fn docker_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(\d{4})_(\d{2})_(\d{2})_([A-Za-z0-9]+)_default_docker(?:\.\d+)?\.log$")
            .unwrap()
    })
}

/// Probes known log locations and returns the first matching profile.
pub async fn detect(client: &KuduClient) -> Result<Option<ProfileKind>, String> {
    if let Ok(entries) = client.list_dir("applogs").await {
        if entries.iter().any(|e| core_regex().is_match(&e.name)) {
            return Ok(Some(ProfileKind::CoreApplogs));
        }
    }
    if let Ok(entries) = client.list_dir("site/wwwroot/App_Data").await {
        if entries
            .iter()
            .any(|e| !e.is_dir() && e.name.eq_ignore_ascii_case("Log.txt"))
        {
            return Ok(Some(ProfileKind::FrameworkAppData));
        }
    }
    if let Ok(entries) = client.list_dir("LogFiles").await {
        if entries.iter().any(|e| docker_regex().is_match(&e.name)) {
            return Ok(Some(ProfileKind::DockerLogfiles));
        }
    }
    Ok(None)
}

/// Lists the remote log files a date range needs, per profile.
pub async fn list_files(
    client: &KuduClient,
    profile: ProfileKind,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<RemoteLogFile>, String> {
    match profile {
        ProfileKind::CoreApplogs => {
            let entries = client.list_dir("applogs").await?;
            let mut files: Vec<RemoteLogFile> = entries
                .iter()
                .filter_map(|e| {
                    let caps = core_regex().captures(&e.name)?;
                    let date = ymd(&caps[1], &caps[2], &caps[3])?;
                    (date >= from && date <= to).then(|| RemoteLogFile {
                        vfs_path: format!("applogs/{}", e.name),
                        name: e.name.clone(),
                        size: e.size,
                        mtime: e.mtime.clone(),
                        date: Some(date),
                        instance: None,
                    })
                })
                .collect();
            files.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(files)
        }
        ProfileKind::FrameworkAppData => {
            let entries = client.list_dir("site/wwwroot/App_Data").await?;
            let entry = entries
                .iter()
                .find(|e| !e.is_dir() && e.name.eq_ignore_ascii_case("Log.txt"))
                .ok_or("Log.txt not found in site/wwwroot/App_Data")?;
            Ok(vec![RemoteLogFile {
                vfs_path: format!("site/wwwroot/App_Data/{}", entry.name),
                name: entry.name.clone(),
                size: entry.size,
                mtime: entry.mtime.clone(),
                date: None,
                instance: None,
            }])
        }
        ProfileKind::DockerLogfiles => {
            let entries = client.list_dir("LogFiles").await?;
            let mut files: Vec<RemoteLogFile> = entries
                .iter()
                .filter_map(|e| {
                    let caps = docker_regex().captures(&e.name)?;
                    let date = ymd(&caps[1], &caps[2], &caps[3])?;
                    (date >= from && date <= to).then(|| RemoteLogFile {
                        vfs_path: format!("LogFiles/{}", e.name),
                        name: e.name.clone(),
                        size: e.size,
                        mtime: e.mtime.clone(),
                        date: Some(date),
                        instance: Some(caps[4].to_string()),
                    })
                })
                .collect();
            files.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(files)
        }
    }
}

fn ymd(y: &str, m: &str, d: &str) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
}

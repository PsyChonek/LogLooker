//! Finding a service's log files, per the log locations its pack declares.
//!
//! Every service stores its logs somewhere slightly different, so a pack lists
//! the places it knows and the app probes them in order. What it finds is stored
//! in config and stays user-overridable - detection is a convenience, never the
//! only way to get a service working.

use crate::plugin::{CompiledLocation, Pack};
use crate::source::LogSource;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteLogFile {
    /// Path relative to the service root, e.g. "applogs/log-2026-07-14.log"
    pub vfs_path: String,
    pub name: String,
    pub size: u64,
    pub mtime: String,
    /// The day this file covers; None for undated growing files, whose lines have
    /// to be filtered on their own timestamps instead
    pub date: Option<NaiveDate>,
    /// Instance id where the name carries one; several in one range means the
    /// service was scaled out and the logs are per-instance
    pub instance: Option<String>,
}

/// Probes the pack's locations in declaration order and returns the id of the
/// first that holds a matching file. Ordering is the pack author's call: the
/// most specific location belongs first, the catch-all last.
pub async fn detect(source: &LogSource, pack: &Pack) -> Result<Option<String>, String> {
    for location in &pack.locations {
        // A directory that is missing (or forbidden) is simply not this service's
        // location - only a match decides, so listing errors are not fatal here.
        let Ok(entries) = source.list_dir(&location.dir).await else {
            continue;
        };
        if entries
            .iter()
            .any(|e| !e.is_dir && location.file.is_match(&e.name))
        {
            return Ok(Some(location.id.clone()));
        }
    }
    Ok(None)
}

/// The files a date range needs from one location. Dated locations select by the
/// date in the name; undated ones return everything they match, because a single
/// growing file can hold any range and only its lines can say.
pub async fn list_files(
    source: &LogSource,
    location: &CompiledLocation,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<RemoteLogFile>, String> {
    let entries = source.list_dir(&location.dir).await?;
    let mut files: Vec<RemoteLogFile> = entries
        .iter()
        .filter(|e| !e.is_dir)
        .filter_map(|entry| {
            let caps = location.file.captures(&entry.name)?;
            let date = match location.dated {
                true => {
                    let date = ymd(&caps)?;
                    if date < from || date > to {
                        return None;
                    }
                    Some(date)
                }
                false => None,
            };
            Some(RemoteLogFile {
                vfs_path: join_path(&location.dir, &entry.name),
                name: entry.name.clone(),
                size: entry.size,
                mtime: entry.mtime.clone(),
                date,
                instance: caps.name("instance").map(|m| m.as_str().to_string()),
            })
        })
        .collect();
    // Name order is date order for a dated location, and stable for the rest
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

fn join_path(dir: &str, name: &str) -> String {
    let dir = dir.trim().trim_matches('/');
    if dir.is_empty() || dir == "." {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn ymd(caps: &regex::Captures) -> Option<NaiveDate> {
    let part = |name: &str| caps.name(name)?.as_str().parse::<u32>().ok();
    NaiveDate::from_ymd_opt(part("y")? as i32, part("m")?, part("d")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{self, Pack, PackOrigin};
    use std::path::PathBuf;

    fn pack() -> Pack {
        plugin::parse_and_compile(
            r#"{
                "id": "azure", "name": "Azure", "source": { "type": "local-folder" },
                "logLocations": [
                  { "id": "core-applogs", "label": "App logs", "dir": "applogs",
                    "file": "^log-(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})(?:_\\d{1,2})?\\.log$" },
                  { "id": "docker", "label": "Container stdout", "dir": "LogFiles",
                    "file": "^(?<y>\\d{4})_(?<m>\\d{2})_(?<d>\\d{2})_(?<instance>[A-Za-z0-9]+)_default_docker\\.log$" },
                  { "id": "app-data", "label": "Log.txt", "dir": "App_Data",
                    "file": "(?i)^Log\\.txt$", "dated": false }
                ]
            }"#,
            PackOrigin::Bundled,
            false,
        )
        .unwrap()
    }

    fn fixture(name: &str, files: &[(&str, &str)]) -> (PathBuf, LogSource) {
        let dir = std::env::temp_dir().join(format!("loglooker-locations-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        for (path, body) in files {
            let full = dir.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        let source = LogSource::new(
            &plugin::SourceDef::LocalFolder { root: None },
            dir.to_str().unwrap(),
            None,
        )
        .unwrap();
        (dir, source)
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, d).unwrap()
    }

    #[tokio::test]
    async fn detects_the_first_location_that_holds_a_matching_file() {
        let (_dir, source) = fixture(
            "detect",
            &[
                ("applogs/internal-nlog.txt", "not a log"),
                ("LogFiles/2026_07_14_abc123_default_docker.log", "line"),
            ],
        );
        // applogs exists but holds nothing the location matches, so probing goes on
        assert_eq!(detect(&source, &pack()).await.unwrap().as_deref(), Some("docker"));
    }

    #[tokio::test]
    async fn detects_nothing_when_no_location_matches() {
        let (_dir, source) = fixture("nomatch", &[("other/readme.md", "hi")]);
        assert_eq!(detect(&source, &pack()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn lists_dated_files_inside_the_range_only() {
        let (_dir, source) = fixture(
            "dated",
            &[
                ("applogs/log-2026-07-13.log", "a"),
                ("applogs/log-2026-07-14.log", "bb"),
                ("applogs/log-2026-07-14_18.log", "ccc"),
                ("applogs/log-2026-07-20.log", "dddd"),
                ("applogs/internal-nlog.txt", "x"),
            ],
        );
        let pack = pack();
        let location = pack.location("core-applogs").unwrap();

        let files = list_files(&source, location, day(13), day(14)).await.unwrap();
        let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["log-2026-07-13.log", "log-2026-07-14.log", "log-2026-07-14_18.log"]
        );
        assert_eq!(files[0].date, Some(day(13)));
        assert_eq!(files[0].vfs_path, "applogs/log-2026-07-13.log");
        // The hourly archive belongs to its day, not to hour zero of the next
        assert_eq!(files[2].date, Some(day(14)));
    }

    #[tokio::test]
    async fn an_undated_location_ignores_the_range_and_carries_no_date() {
        let (_dir, source) = fixture("undated", &[("App_Data/log.TXT", "text")]);
        let pack = pack();
        let location = pack.location("app-data").unwrap();

        let files = list_files(&source, location, day(1), day(2)).await.unwrap();
        assert_eq!(files.len(), 1, "a case-different name still matches");
        assert_eq!(files[0].date, None);
        assert_eq!(files[0].vfs_path, "App_Data/log.TXT");
    }

    #[tokio::test]
    async fn captures_the_instance_id_of_a_scaled_out_service() {
        let (_dir, source) = fixture(
            "instance",
            &[
                ("LogFiles/2026_07_14_abc123_default_docker.log", "a"),
                ("LogFiles/2026_07_14_def456_default_docker.log", "b"),
            ],
        );
        let pack = pack();
        let files = list_files(&source, pack.location("docker").unwrap(), day(14), day(14))
            .await
            .unwrap();

        let mut instances: Vec<_> = files.iter().filter_map(|f| f.instance.clone()).collect();
        instances.sort();
        assert_eq!(instances, vec!["abc123", "def456"]);
    }

    #[test]
    fn a_root_location_does_not_prefix_the_file_name() {
        assert_eq!(join_path(".", "a.log"), "a.log");
        assert_eq!(join_path("", "a.log"), "a.log");
        assert_eq!(join_path("/applogs/", "a.log"), "applogs/a.log");
    }
}

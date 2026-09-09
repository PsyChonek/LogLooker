use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    current_version: String,
    latest_version: String,
    update_available: bool,
}

// LogLooker releases use numeric major.minor.patch versions. Accept a fourth
// Windows version component too, without depending on localized table headings.
fn version_parts(value: &str) -> Option<[u64; 4]> {
    let parts: Vec<_> = value.split('.').collect();
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    let mut result = [0; 4];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        result[index] = part.parse().ok()?;
    }
    Some(result)
}

fn parse_versions(current: &str, output: &str) -> Result<UpdateCheck, String> {
    let current_parts = version_parts(current).ok_or("unsupportedVersion")?;
    let (latest_parts, latest) = output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            version_parts(line).map(|parts| (parts, line))
        })
        .max_by_key(|(parts, _)| *parts)
        .ok_or("invalidResponse")?;
    Ok(UpdateCheck {
        current_version: current.into(),
        latest_version: latest.into(),
        update_available: latest_parts > current_parts,
    })
}

fn check_exit_code(code: Option<i32>) -> Result<(), String> {
    match code.map(|value| value as u32) {
        Some(0) => Ok(()),
        Some(0x8A150014) => Err("packageUnavailable".into()),
        Some(0x8A150046) => Err("sourceAgreement".into()),
        _ => Err("checkFailed".into()),
    }
}

#[cfg(windows)]
async fn winget_versions() -> Result<String, String> {
    use std::time::Duration;
    use tokio::process::Command;

    let mut command = Command::new("winget.exe");
    command
        .args([
            "show",
            "--id",
            "PsyChonek.LogLooker",
            "--exact",
            "--source",
            "winget",
            "--versions",
            "--disable-interactivity",
        ])
        .current_dir(std::env::temp_dir())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW: background checks stay quiet.
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(45), command.output())
        .await
        .map_err(|_| "checkTimeout")?
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "wingetMissing"
            } else {
                "checkFailed"
            }
        })?;
    check_exit_code(output.status.code())?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateCheck, String> {
    #[cfg(windows)]
    {
        let output = winget_versions().await?;
        // Tauri's version is the running app version; Cargo.toml may lag local builds.
        parse_versions(&app.package_info().version.to_string(), &output)
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err("windowsOnly".into())
    }
}

#[tauri::command]
pub async fn install_winget_update(app: tauri::AppHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        // Recheck before closing the app, including when invoked outside the UI.
        if !check_for_updates(app.clone()).await?.update_available {
            return Err("noUpdate".into());
        }
        let script = include_str!("winget-update.ps1")
            .replace("__LOGLOOKER_PID__", &std::process::id().to_string());
        let powershell = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .ok_or("launchFailed")?
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        Command::new(powershell)
            .args(["-NoLogo", "-NoProfile", "-NoExit", "-Command", &script])
            .current_dir(std::env::temp_dir())
            // This is the interactive installer console requested by the user.
            .creation_flags(0x00000010) // CREATE_NEW_CONSOLE
            .spawn()
            .map_err(|_| "launchFailed")?;
        app.exit(0);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = app;
        Err("windowsOnly".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_compare_numerically_and_ignore_localized_headings() {
        let result = parse_versions(
            "1.1.9",
            "Nalezeno LogLooker [PsyChonek.LogLooker]\nVerze\n-------\n1.1.9\n1.1.21\n1.1.10\n",
        )
        .unwrap();
        assert_eq!(result.latest_version, "1.1.21");
        assert!(result.update_available);
    }

    #[test]
    fn equal_or_older_winget_versions_do_not_offer_updates() {
        assert!(
            !parse_versions("1.1.21", "1.1.21.0\n1.1.20")
                .unwrap()
                .update_available
        );
        assert!(!parse_versions("1.2.0", "1.1.21").unwrap().update_available);
    }

    #[test]
    fn malformed_or_prerelease_output_is_not_treated_as_current() {
        for output in [
            "",
            "No package found",
            "1.2.0-beta.1",
            "1.2",
            "1.2.3.4.5",
            "1.+2.3",
        ] {
            assert_eq!(
                parse_versions("1.1.21", output).unwrap_err(),
                "invalidResponse"
            );
        }
        assert_eq!(
            parse_versions("1.2.0-beta.1", "1.2.0").unwrap_err(),
            "unsupportedVersion"
        );
    }

    #[test]
    fn missing_package_and_source_consent_are_distinct_from_no_update() {
        assert!(check_exit_code(Some(0)).is_ok());
        assert_eq!(
            check_exit_code(Some(0x8A150014_u32 as i32)),
            Err("packageUnavailable".into())
        );
        assert_eq!(
            check_exit_code(Some(0x8A150046_u32 as i32)),
            Err("sourceAgreement".into())
        );
        assert_eq!(check_exit_code(Some(1)), Err("checkFailed".into()));
    }
}

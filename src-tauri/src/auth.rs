use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use std::sync::Mutex;

pub struct TokenState(pub Mutex<Option<CachedToken>>);

impl TokenState {
    pub fn new() -> Self {
        TokenState(Mutex::new(None))
    }
}

pub struct CachedToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

/// Drops the cached bearer token after Kudu rejects it. Token expiry is checked
/// before each use, but Azure can revoke a token before its advertised expiry
/// after a long idle period or an account/subscription change.
pub fn invalidate_token(state: &TokenState) {
    if let Ok(mut guard) = state.0.lock() {
        *guard = None;
    }
}

/// Kudu uses this stable leading phrase for both 401 and 403 responses. Keep
/// the check here so retry behavior and the UI agree on what an access failure
/// is without depending on the rest of the human-readable message.
pub fn is_access_denied(message: &str) -> bool {
    message.contains("Access denied (")
}

#[derive(Deserialize)]
struct AzTokenResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// Unix epoch seconds, present in az >= 2.54
    expires_on: Option<i64>,
    #[serde(rename = "expiresOn")]
    expires_on_local: Option<String>,
}

/// Returns a valid ARM-audience bearer token, refreshing via `az account get-access-token`
/// when the cached one is missing or within 5 minutes of expiry. Kudu accepts these
/// tokens directly on *.scm.azurewebsites.net.
pub async fn get_token(state: &TokenState) -> Result<String, String> {
    if let Ok(guard) = state.0.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.expires_at - Duration::minutes(5) > Utc::now() {
                return Ok(cached.token.clone());
            }
        }
    }

    let output = az_command()
        .args([
            "account",
            "get-access-token",
            "--resource",
            "https://management.azure.com",
            "-o",
            "json",
        ])
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AZ_NOT_INSTALLED.to_string()
            } else {
                format!("Failed to run az CLI: {e}")
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(classify_az_error(&stderr));
    }

    let parsed: AzTokenResponse = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse az token output: {e}"))?;

    let expires_at = parse_expiry(&parsed).unwrap_or_else(|| Utc::now() + Duration::minutes(30));

    if let Ok(mut guard) = state.0.lock() {
        *guard = Some(CachedToken {
            token: parsed.access_token.clone(),
            expires_at,
        });
    }

    Ok(parsed.access_token)
}

const AZ_NOT_INSTALLED: &str =
    "Azure CLI (az) not found. Install it from https://aka.ms/azure-cli, then run 'az login'.";

/// Turns a failed `az account get-access-token` stderr into an actionable message.
/// The distinct cases are: az missing (Windows surfaces this as a non-zero exit
/// rather than a spawn error), no login / expired session, and no subscription set.
fn classify_az_error(stderr: &str) -> String {
    let stderr = stderr.trim();
    let lower = stderr.to_lowercase();

    // On Windows `cmd /C az` still spawns cmd, so a missing az shows up here as
    // "'az' is not recognized ..." rather than an io NotFound on spawn.
    if lower.contains("is not recognized")
        || lower.contains("cannot find")
        || lower.contains("command not found")
    {
        return AZ_NOT_INSTALLED.to_string();
    }

    if lower.contains("az login") || lower.contains("no subscription found") {
        return "Not signed in to Azure. Run 'az login' in a terminal, then retry.".to_string();
    }

    if lower.contains("az account set") || lower.contains("subscription") {
        return format!(
            "No active Azure subscription. Run 'az account set --subscription <name-or-id>' \
             (list them with 'az account list -o table'), then retry.\n\nDetails: {stderr}"
        );
    }

    format!("az account get-access-token failed: {stderr}")
}

fn parse_expiry(parsed: &AzTokenResponse) -> Option<DateTime<Utc>> {
    if let Some(epoch) = parsed.expires_on {
        return DateTime::from_timestamp(epoch, 0);
    }
    // Older az versions only emit a naive local timestamp like "2026-07-14 12:34:56.000000"
    let local = parsed.expires_on_local.as_deref()?;
    let naive = chrono::NaiveDateTime::parse_from_str(local, "%Y-%m-%d %H:%M:%S%.f").ok()?;
    naive
        .and_local_timezone(chrono::Local)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

fn az_command() -> tokio::process::Command {
    // az is a .cmd shim on Windows, so it must go through cmd /C
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.args(["/C", "az"]);
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        cmd
    }
    #[cfg(not(windows))]
    {
        tokio::process::Command::new("az")
    }
}

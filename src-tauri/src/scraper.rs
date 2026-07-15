use crate::profiles::ProfileKind;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    Test,
    Production,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapedService {
    pub name: String,
    pub environment: Environment,
    pub url: String,
    pub kudu_url: Option<String>,
    /// Log profile derived from the "Kudu Logs" link's VFS directory
    /// (`.../filemanager/<dir>/`); None when the page has no such link or the
    /// directory is unrecognised, in which case detection still has to probe.
    pub profile: Option<ProfileKind>,
}

const STATUS_PAGE: &str = "https://status.example.com/";

/// Scrapes the Example status page. The page is server-rendered with one
/// div.data-container per service carrying data-url / data-section-name /
/// data-page-name attributes, a Kudu link (*.scm.azurewebsites.net) and a
/// "Kudu Logs" link (.../filemanager/<dir>/) inside the service's card.
/// Breakage here is non-fatal: the service list lives in editable config and
/// this only refreshes it.
pub async fn scrape_services() -> Result<Vec<ScrapedService>, String> {
    let html = reqwest::get(STATUS_PAGE)
        .await
        .map_err(|e| format!("Failed to fetch {STATUS_PAGE}: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Failed to read {STATUS_PAGE}: {e}"))?;

    parse_services(&html)
}

fn parse_services(html: &str) -> Result<Vec<ScrapedService>, String> {
    let container = Regex::new(
        r#"data-url="([^"]*)"\s+data-section-name="([^"]*)"\s+data-page-name="([^"]*)""#,
    )
    .map_err(|e| e.to_string())?;
    let kudu = Regex::new(r#"href="(https://[^"]*\.scm\.azurewebsites\.net)[/"]"#)
        .map_err(|e| e.to_string())?;
    let logs = Regex::new(r#"href="https://[^"]*\.scm\.azurewebsites\.net/filemanager/([^"]*)""#)
        .map_err(|e| e.to_string())?;

    let matches: Vec<_> = container.captures_iter(html).collect();
    if matches.is_empty() {
        return Err("No services found on the status page; its markup may have changed".into());
    }

    let mut services = Vec::with_capacity(matches.len());
    for (i, caps) in matches.iter().enumerate() {
        let block_start = caps.get(0).unwrap().end();
        let block_end = matches
            .get(i + 1)
            .map(|next| next.get(0).unwrap().start())
            .unwrap_or(html.len());
        let block = &html[block_start..block_end];

        let environment = if caps[2].to_lowercase().contains("prod") {
            Environment::Production
        } else {
            Environment::Test
        };

        let profile = logs
            .captures(block)
            .and_then(|l| ProfileKind::from_log_dir(&l[1]));

        services.push(ScrapedService {
            name: caps[3].to_string(),
            environment,
            url: caps[1].to_string(),
            kudu_url: kudu.captures(block).map(|k| k[1].to_string()),
            profile,
        });
    }
    Ok(services)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_blocks() {
        let html = r#"
            <div class="col mb-4 data-container" data-url="https://api.example.com/client" data-section-name="Production" data-page-name="ClientApi">
              <a href="https://Example-production-client-api.scm.azurewebsites.net" target="_blank">Kudu</a>
              <a href="https://Example-production-client-api.scm.azurewebsites.net/filemanager/applogs/" target="_blank">Kudu Logs</a>
            </div>
            <div class="col mb-4 data-container" data-url="https://api.test.example.com/client" data-section-name="Test" data-page-name="ClientApi">
              <a href="https://Example-test-client-api.scm.azurewebsites.net" target="_blank">Kudu</a>
              <a href="https://Example-test-client-api.scm.azurewebsites.net/filemanager/applogs/" target="_blank">Kudu Logs</a>
            </div>
        "#;
        let services = parse_services(html).unwrap();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].name, "ClientApi");
        assert_eq!(services[0].environment, Environment::Production);
        assert_eq!(
            services[0].kudu_url.as_deref(),
            Some("https://Example-production-client-api.scm.azurewebsites.net")
        );
        assert_eq!(services[0].profile, Some(ProfileKind::CoreApplogs));
        assert_eq!(services[1].environment, Environment::Test);
        assert_eq!(
            services[1].kudu_url.as_deref(),
            Some("https://Example-test-client-api.scm.azurewebsites.net")
        );
        assert_eq!(services[1].profile, Some(ProfileKind::CoreApplogs));
    }

    #[test]
    fn leaves_profile_unset_when_no_logs_link() {
        let html = r#"
            <div class="col mb-4 data-container" data-url="https://app.example.com" data-section-name="Production" data-page-name="WebClient">
              <a href="https://Example-production-webclient.scm.azurewebsites.net" target="_blank">Kudu</a>
            </div>
        "#;
        let services = parse_services(html).unwrap();
        assert_eq!(services[0].profile, None);
    }

    #[test]
    fn leaves_kudu_url_unset_when_the_page_has_no_link() {
        // A service the status page lists without a Kudu link: the URL stays
        // None rather than being guessed from another environment's host name.
        let html = r#"
            <div class="col mb-4 data-container" data-url="https://app.test.example.com" data-section-name="Test" data-page-name="BackOffice">
            </div>
        "#;
        let services = parse_services(html).unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].kudu_url, None);
    }
}

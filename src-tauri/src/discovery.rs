//! Filling in the service list from whatever a pack can be pointed at.
//!
//! Discovery is always optional and never authoritative: it merges into an
//! editable list, so a status page changing its markup degrades to "the refresh
//! button stops finding anything" rather than to an app that cannot be used.

use crate::plugin::{CompiledScrape, DiscoveryDef, Pack};

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredService {
    pub name: String,
    pub environment: String,
    /// None when the page listed a service but no link to reach it; the user can
    /// fill it in by hand afterwards
    pub endpoint: Option<String>,
    /// A log location the page pointed straight at, saving a probe
    pub location: Option<String>,
}

pub async fn discover(pack: &Pack) -> Result<Vec<DiscoveredService>, String> {
    match &pack.manifest.discovery {
        DiscoveryDef::None => Err(format!(
            "Pack \"{}\" does not discover services - add them by hand",
            pack.manifest.name
        )),
        DiscoveryDef::Static { services } => Ok(services
            .iter()
            .map(|s| DiscoveredService {
                name: s.name.clone(),
                environment: s.environment.clone(),
                endpoint: Some(s.endpoint.clone()),
                location: s.location.clone(),
            })
            .collect()),
        DiscoveryDef::Scrape(def) => {
            let scrape = pack
                .scrape
                .as_ref()
                .ok_or("Scrape discovery was not compiled - this is a bug")?;
            let html = reqwest::get(&def.url)
                .await
                .map_err(|e| format!("Failed to fetch {}: {e}", def.url))?
                .text()
                .await
                .map_err(|e| format!("Failed to read {}: {e}", def.url))?;
            parse_scrape(pack, scrape, &html)
        }
    }
}

/// One `container` match per service; each service's own regexes then run over
/// the block that runs from its container match to the next one, which is what
/// keeps a link being attributed to the service it sits under.
fn parse_scrape(
    pack: &Pack,
    scrape: &CompiledScrape,
    html: &str,
) -> Result<Vec<DiscoveredService>, String> {
    let matches: Vec<_> = scrape.container.captures_iter(html).collect();
    if matches.is_empty() {
        return Err(format!(
            "No services found at {} - its markup may have changed",
            scrape.def.url
        ));
    }

    let fallback_env = scrape
        .def
        .default_environment
        .clone()
        .or_else(|| pack.manifest.environments.first().map(|e| e.id.clone()))
        .ok_or("The pack declares no environment to assign discovered services to")?;

    let mut services = Vec::with_capacity(matches.len());
    for (i, caps) in matches.iter().enumerate() {
        let whole = caps.get(0).expect("group 0 always exists");
        let block_end = matches
            .get(i + 1)
            .map(|next| next.get(0).expect("group 0 always exists").start())
            .unwrap_or(html.len());
        let block = &html[whole.end()..block_end];

        let Some(name) = caps.name("name").map(|m| m.as_str().trim()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }

        let section = caps.name("section").map(|m| m.as_str()).unwrap_or_default();
        let environment = environment_for(scrape, section).unwrap_or_else(|| fallback_env.clone());

        let endpoint = scrape
            .endpoint
            .as_ref()
            .and_then(|re| re.captures(block))
            .and_then(|caps| first_group(&caps, "endpoint"));

        // A directory the page links at only helps if the pack knows a location
        // that reads it; anything else leaves detection to probe as usual.
        let location = scrape
            .log_dir
            .as_ref()
            .and_then(|re| re.captures(block))
            .and_then(|caps| first_group(&caps, "dir"))
            .and_then(|dir| pack.location_by_dir(&dir).map(|l| l.id.clone()));

        services.push(DiscoveredService {
            name: name.to_string(),
            environment,
            endpoint,
            location,
        });
    }
    Ok(services)
}

/// The named group if the pattern has one, else the first plain group - so a
/// simple pack can write `href="(https://[^"]*)"` without naming anything.
fn first_group(caps: &regex::Captures, name: &str) -> Option<String> {
    caps.name(name)
        .or_else(|| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn environment_for(scrape: &CompiledScrape, section: &str) -> Option<String> {
    let section = section.to_lowercase();
    scrape
        .def
        .environments
        .iter()
        .find(|rule| section.contains(&rule.contains.to_lowercase()))
        .map(|rule| rule.environment.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{self, PackOrigin};

    /// A pack shaped like a real status-page scrape: a container per service
    /// carrying its name and section, a Kudu link and a log-folder link.
    fn scrape_pack() -> Pack {
        plugin::parse_and_compile(
            r#"{
                "id": "status", "name": "Status page", "source": { "type": "kudu" },
                "environments": [ { "id": "test", "label": "TEST" },
                                  { "id": "production", "label": "PROD" } ],
                "logLocations": [
                  { "id": "core-applogs", "label": "App logs", "dir": "applogs",
                    "file": "^log-(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})\\.log$" }
                ],
                "discovery": {
                  "type": "scrape",
                  "url": "https://status.example.com/",
                  "label": "Refresh from status.example.com",
                  "container": "data-section-name=\"(?<section>[^\"]*)\"\\s+data-page-name=\"(?<name>[^\"]*)\"",
                  "endpoint": "href=\"(https://[^\"]*\\.scm\\.azurewebsites\\.net)[/\"]",
                  "logDir": "href=\"https://[^\"]*\\.scm\\.azurewebsites\\.net/filemanager/([^\"]*)\"",
                  "environments": [ { "contains": "prod", "environment": "production" } ],
                  "defaultEnvironment": "test"
                }
            }"#,
            PackOrigin::Bundled,
            false,
        )
        .expect("scrape pack must compile")
    }

    const PAGE: &str = r#"
        <div data-section-name="Production" data-page-name="ClientApi">
          <a href="https://prod-client-api.scm.azurewebsites.net">Kudu</a>
          <a href="https://prod-client-api.scm.azurewebsites.net/filemanager/applogs/">Logs</a>
        </div>
        <div data-section-name="Test" data-page-name="ClientApi">
          <a href="https://test-client-api.scm.azurewebsites.net">Kudu</a>
          <a href="https://test-client-api.scm.azurewebsites.net/filemanager/applogs/">Logs</a>
        </div>
        <div data-section-name="Test" data-page-name="BackOffice">
        </div>
    "#;

    #[test]
    fn parses_services_with_environment_and_location() {
        let pack = scrape_pack();
        let services = parse_scrape(&pack, pack.scrape.as_ref().unwrap(), PAGE).unwrap();
        assert_eq!(services.len(), 3);

        assert_eq!(services[0].name, "ClientApi");
        assert_eq!(services[0].environment, "production");
        assert_eq!(
            services[0].endpoint.as_deref(),
            Some("https://prod-client-api.scm.azurewebsites.net")
        );
        assert_eq!(services[0].location.as_deref(), Some("core-applogs"));

        assert_eq!(services[1].environment, "test");
    }

    /// A service the page lists without a link keeps no endpoint rather than
    /// inheriting the previous service's - blocks are cut at the next container.
    #[test]
    fn leaves_the_endpoint_unset_when_the_block_has_no_link() {
        let pack = scrape_pack();
        let services = parse_scrape(&pack, pack.scrape.as_ref().unwrap(), PAGE).unwrap();
        assert_eq!(services[2].name, "BackOffice");
        assert_eq!(services[2].endpoint, None);
        assert_eq!(services[2].location, None);
    }

    #[test]
    fn reports_markup_that_no_longer_matches_instead_of_returning_nothing() {
        let pack = scrape_pack();
        let err = parse_scrape(&pack, pack.scrape.as_ref().unwrap(), "<html></html>").unwrap_err();
        assert!(err.contains("markup may have changed"), "{err}");
    }

    /// An unrecognised log directory is simply not a hint; detection still probes.
    #[test]
    fn ignores_a_log_directory_no_location_reads() {
        let pack = scrape_pack();
        let page = r#"
            <div data-section-name="Test" data-page-name="Odd">
              <a href="https://odd.scm.azurewebsites.net">Kudu</a>
              <a href="https://odd.scm.azurewebsites.net/filemanager/somewhere-else/">Logs</a>
            </div>
        "#;
        let services = parse_scrape(&pack, pack.scrape.as_ref().unwrap(), page).unwrap();
        assert_eq!(services[0].location, None);
    }

    #[tokio::test]
    async fn a_pack_without_discovery_says_so() {
        let pack = plugin::parse_and_compile(
            r#"{ "id": "plain", "name": "Plain", "source": { "type": "local-folder" } }"#,
            PackOrigin::Bundled,
            false,
        )
        .unwrap();
        let err = discover(&pack).await.unwrap_err();
        assert!(err.contains("does not discover services"), "{err}");
    }

    #[tokio::test]
    async fn static_discovery_returns_the_packs_own_list() {
        let pack = plugin::parse_and_compile(
            r#"{
                "id": "hosts", "name": "Hosts", "source": { "type": "kudu" },
                "environments": [ { "id": "test", "label": "TEST" } ],
                "discovery": { "type": "static", "services": [
                  { "name": "WebJobs", "environment": "test",
                    "endpoint": "https://test-webjobs.scm.azurewebsites.net" }
                ] }
            }"#,
            PackOrigin::Bundled,
            false,
        )
        .unwrap();
        let services = discover(&pack).await.unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "WebJobs");
        assert_eq!(services[0].environment, "test");
    }
}

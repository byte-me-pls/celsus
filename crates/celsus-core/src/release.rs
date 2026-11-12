//! Looking for a newer release of the app.
//!
//! One request to GitHub for the latest published release of the project. It carries the app's name and version
//! and nothing else: no account, no key, nothing about books.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

/// A release that is newer than the running app.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Release {
    pub version: String,
    /// The release's page, where the installers are.
    pub url: String,
    /// What changed, as the release says it.
    pub notes: String,
}

/// The numbers of a version such as `0.2611.1` or `v0.2611.1`. Anything else has none.
fn numbers(version: &str) -> Option<Vec<u64>> {
    let version = version.trim().trim_start_matches('v');
    let parts: Option<Vec<u64>> = version.split('.').map(|p| p.parse().ok()).collect();
    parts.filter(|p| !p.is_empty())
}

/// Whether version `a` comes after version `b`.
pub fn newer(a: &str, b: &str) -> bool {
    match (numbers(a), numbers(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Read what GitHub says about the latest release of `repo` and keep it if it is newer than `current`.
fn read(answer: &Value, repo: &str, current: &str) -> Option<Release> {
    let version = answer["tag_name"].as_str()?.trim_start_matches('v');
    let url = answer["html_url"].as_str()?;
    // Only the project's own release pages are handed on to be opened.
    if !newer(version, current) || !url.starts_with(&format!("https://github.com/{repo}/releases/"))
    {
        return None;
    }
    Some(Release {
        version: version.to_owned(),
        url: url.to_owned(),
        notes: answer["body"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(4000)
            .collect(),
    })
}

/// The latest published release of `repo` ("owner/name") on GitHub, if it is newer than `current`.
pub fn latest(repo: &str, current: &str) -> Result<Option<Release>, String> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(15)))
        .build();
    let mut response = ureq::Agent::new_with_config(config)
        .get(format!(
            "https://api.github.com/repos/{repo}/releases/latest"
        ))
        .header("User-Agent", format!("Celsus/{current}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    // A project without a published release answers 404: there is nothing newer.
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(format!("GitHub answered {status}"));
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let answer: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(read(&answer, repo, current))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_compare_by_their_numbers() {
        assert!(newer("0.2611.2", "0.2611.1"));
        assert!(newer("v0.2612.0", "0.2611.9"));
        assert!(newer("0.2611.10", "0.2611.9"));
        assert!(!newer("0.2611.1", "0.2611.1"));
        assert!(!newer("0.2610.5", "0.2611.0"));
        assert!(!newer("nightly", "0.2611.0"));
    }

    #[test]
    fn a_newer_release_is_kept_with_its_page_and_notes() {
        let answer = json!({
            "tag_name": "v0.2611.2",
            "html_url": "https://github.com/byte-me-pls/celsus/releases/tag/v0.2611.2",
            "body": "What changed"
        });
        let release = read(&answer, "byte-me-pls/celsus", "0.2611.1").unwrap();
        assert_eq!(release.version, "0.2611.2");
        assert_eq!(release.notes, "What changed");
        assert_eq!(read(&answer, "byte-me-pls/celsus", "0.2611.2"), None);
    }

    #[test]
    fn a_page_outside_the_project_is_not_handed_on() {
        let answer = json!({
            "tag_name": "v9.0.0",
            "html_url": "https://example.com/byte-me-pls/celsus/releases/tag/v9.0.0"
        });
        assert_eq!(read(&answer, "byte-me-pls/celsus", "0.2611.1"), None);
    }
}

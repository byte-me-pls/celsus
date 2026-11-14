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

/// What release notes say in short: the words in bold that open each of their entries (`- **Like this.** …`).
pub fn headlines(notes: &str) -> Vec<String> {
    notes
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("- **")?;
            let end = rest.find("**")?;
            let headline = rest[..end].trim().trim_end_matches('.').trim();
            (!headline.is_empty()).then(|| headline.to_owned())
        })
        .collect()
}

/// The notes of `version` in a changelog written as this project's is: its section (`## 0.2611.6 (date)`), or
/// for a build made before its version had notes, the section not released yet (`## Unreleased`).
pub fn section<'a>(changelog: &'a str, version: &str) -> Option<&'a str> {
    let heading = |title: &str| {
        changelog
            .match_indices("\n## ")
            .map(|(at, _)| at + 1)
            .find(|&at| {
                changelog[at + 3..]
                    .split_whitespace()
                    .next()
                    .is_some_and(|w| w == title)
            })
    };
    let start = heading(version.trim_start_matches('v')).or_else(|| heading("Unreleased"))?;
    let body = &changelog[start..];
    let body = &body[body.find('\n').map_or(body.len(), |n| n + 1)..];
    Some(&body[..body.find("\n## ").map_or(body.len(), |n| n + 1)])
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

    #[test]
    fn the_notes_of_a_version_are_told_in_short() {
        let changelog = "# Changelog\n\nIntro.\n\n## Unreleased\n\n- **Coming.** Soon.\n\n## 0.2611.6 (2026-11-13)\n\n\
                         - **Titles stay flush right.** A title of two lines\n  was read as a paragraph.\n\
                         - **Words are not broken.** More.\n\n## 0.2611.5 (2026-11-13)\n\n- **Older.** Text.\n";
        let notes = section(changelog, "0.2611.6").unwrap();
        assert_eq!(
            headlines(notes),
            ["Titles stay flush right", "Words are not broken"]
        );
        assert_eq!(
            headlines(section(changelog, "v0.2611.5").unwrap()),
            ["Older"]
        );
        // A version with no notes of its own yet is told what is not released.
        assert_eq!(
            headlines(section(changelog, "0.2611.7").unwrap()),
            ["Coming"]
        );
        assert_eq!(section("# Changelog\n", "0.1"), None);
    }
}

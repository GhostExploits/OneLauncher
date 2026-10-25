use crate::{LauncherError, LauncherResult};
use oneclient_common::paths;
use oneclient_net::RequestClient;
use oneclient_net::{EtagPolicy, fetch_cached};

// Release notes must come from this fork's release channel, not the upstream
// OneClient channel. The bundled changelog is a truthful offline fallback until
// the first SpiderClient release publishes changelog.json.
const CHANGELOG_URL: &str =
    "https://github.com/GhostExploits/OneLauncher/releases/latest/download/changelog.json";
const LOCAL_CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ChangelogEntry {
    pub version: String,
    pub body: String,
}

#[tracing::instrument(level = "debug", skip(net))]
pub async fn fetch_changelog(net: &RequestClient) -> LauncherResult<Vec<ChangelogEntry>> {
    // A separate cache name avoids serving an upstream OneClient changelog left
    // by an older installation.
    let cache_path = paths::caches_dir()?.join("spiderclient-changelog.json");

    let fetched = fetch_cached(net, CHANGELOG_URL, &cache_path, EtagPolicy::CommitNow).await;
    let entries = match fetched {
        Ok(Some(fetched)) => fetched.json()?,
        Ok(None) | Err(_) => local_changelog(),
    };

    if entries.is_empty() {
        return Err(LauncherError::InvalidSettingsProfile {
            reason: "changelog is unavailable and the bundled changelog is empty".to_string(),
        });
    }
    Ok(entries)
}

/// Parse the repository's own changelog so the page remains useful when the
/// release channel has no published JSON yet (or the machine is offline).
fn local_changelog() -> Vec<ChangelogEntry> {
    parse_changelog(LOCAL_CHANGELOG)
}

fn parse_changelog(text: &str) -> Vec<ChangelogEntry> {
    let mut entries = Vec::new();
    let mut version: Option<String> = None;
    let mut body = Vec::<&str>::new();

    let flush =
        |version: &mut Option<String>, body: &mut Vec<&str>, entries: &mut Vec<ChangelogEntry>| {
            if let Some(version) = version.take() {
                let text = body.join("\n").trim().to_string();
                if !text.is_empty() {
                    entries.push(ChangelogEntry {
                        version,
                        body: text,
                    });
                }
            }
            body.clear();
        };

    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            flush(&mut version, &mut body, &mut entries);
            version = heading
                .split_whitespace()
                .next()
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            continue;
        }
        if version.is_some() {
            body.push(line);
        }
    }
    flush(&mut version, &mut body, &mut entries);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_changelog_is_parsed() {
        let entries = local_changelog();
        assert!(entries.iter().any(|entry| entry.version == "2.8.0"));
        assert!(entries.iter().all(|entry| !entry.body.trim().is_empty()));
    }

    #[test]
    fn headings_define_release_boundaries() {
        let entries = parse_changelog("intro\n## 1.2.0 (date)\n### Features\n- a\n## 1.1.0\n- b\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].version, "1.2.0");
        assert!(entries[0].body.contains("- a"));
        assert_eq!(entries[1].version, "1.1.0");
        assert!(!entries[1].body.contains("- a"));
    }
}

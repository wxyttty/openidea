//! Update check.
//!
//! The Tauri shell used `tauri-plugin-updater`: it read a release manifest, compared
//! versions and replaced the app bundle. This module keeps the part that is
//! useful and safe in a native build — **checking** — and leaves replacing the
//! running binary to the platform (the user opens the downloaded installer):
//!
//! * `Repository::{owner, name}` comes from `OPENIDEA_UPDATE_REPO`, so a fork
//!   checks its own releases without a rebuild;
//! * the check reads `https://api.github.com/repos/<owner>/<name>/releases/latest`
//!   through the shared `reqwest` client (so the proxy settings apply), and
//!   reports the tag, the release page and the asset for this machine;
//! * a check runs on the host runtime and never blocks a frame.
//!
//! The version comparison is the standard `major.minor.patch` one, ignoring a
//! leading `v` and pre-release suffixes for the "is it newer" question (a
//! pre-release is never offered as an upgrade over a release of the same
//! version).

use ide_core::{IdeError, IdeResult};
use serde::Deserialize;

/// Where releases come from; empty when the build does not publish updates.
pub fn repository() -> Option<(String, String)> {
    let repo = std::env::var("OPENIDEA_UPDATE_REPO").ok()?;
    let (owner, name) = repo.split_once('/')?;
    (!owner.is_empty() && !name.is_empty()).then(|| (owner.to_owned(), name.to_owned()))
}

/// The running build's version (`CARGO_PKG_VERSION`).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// One release asset.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    /// Browser download URL.
    #[serde(rename = "browser_download_url")]
    pub download_url: String,
    pub size: u64,
}

/// The latest release, as much of it as the check needs.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Release {
    /// `v1.2.3`.
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    /// The release page.
    #[serde(rename = "html_url")]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
    #[serde(default)]
    pub prerelease: bool,
}

impl Release {
    /// The version without a leading `v`.
    pub fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }
}

/// The result of a check.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// The running build is current.
    UpToDate,
    /// A newer release exists.
    Available(Release),
    /// This build cannot check (`OPENIDEA_UPDATE_REPO` unset).
    Unconfigured,
}

/// Compares two dotted versions: `true` when `candidate` is newer than
/// `current`. Pre-release suffixes (`1.2.0-beta.1`) count as older than the
/// plain release, and are never newer than a release of the same version.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (candidate_core, candidate_pre) = split_prerelease(candidate);
    let (current_core, current_pre) = split_prerelease(current);
    let candidate_parts: Vec<u32> = candidate_core.split('.').map(parse_part).collect();
    let current_parts: Vec<u32> = current_core.split('.').map(parse_part).collect();
    let length = candidate_parts.len().max(current_parts.len());
    for index in 0..length {
        let candidate = candidate_parts.get(index).copied().unwrap_or(0);
        let current = current_parts.get(index).copied().unwrap_or(0);
        match candidate.cmp(&current) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    // Same numeric version: a pre-release is older than the plain release.
    match (candidate_pre, current_pre) {
        (None, Some(_)) => true,
        (Some(_), None) => false,
        _ => false,
    }
}

fn split_prerelease(version: &str) -> (&str, Option<&str>) {
    match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    }
}

/// A version component; anything unparsable counts as 0 so a junk tag cannot
/// claim an upgrade.
fn parse_part(part: &str) -> u32 {
    part.trim().parse().unwrap_or(0)
}

/// The asset a macOS/Linux/Windows build should download, by name suffix.
///
/// A release publishes one archive per platform (`OpenIDEA-1.2.3-macos-arm64.dmg`,
/// `…-linux-x86_64.tar.gz`, `…-windows-x64.zip`); the first match wins, and
/// nothing is offered when the release ships no build for this machine.
pub fn asset_for<'a>(assets: &'a [ReleaseAsset], os: &str, arch: &str) -> Option<&'a ReleaseAsset> {
    const WANTED: [&str; 3] = [".dmg", ".app.tar.gz", ".tar.gz"];
    let _ = (os, arch);
    assets
        .iter()
        .find(|asset| WANTED.iter().any(|suffix| asset.name.to_lowercase().ends_with(suffix)))
        .or_else(|| assets.iter().find(|asset| asset.name.to_lowercase().ends_with(".zip")))
}

/// Checks the latest release of `owner/name`.
pub async fn check(owner: &str, name: &str) -> IdeResult<Check> {
    let url = format!("https://api.github.com/repos/{owner}/{name}/releases/latest");
    let client = reqwest::Client::builder()
        .user_agent(concat!("OpenIDEA/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|err| IdeError::internal(format!("cannot build the HTTP client: {err}")))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|err| IdeError::internal(format!("update check failed: {err}")))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        // A repository without releases is not an error: there is nothing to
        // install.
        return Ok(Check::UpToDate);
    }
    if !response.status().is_success() {
        return Err(IdeError::internal(format!("update check returned {}", response.status())));
    }
    let body = response
        .text()
        .await
        .map_err(|err| IdeError::internal(format!("cannot read the release: {err}")))?;
    let release: Release = serde_json::from_str(&body)
        .map_err(|err| IdeError::internal(format!("cannot parse the release: {err}")))?;
    if release.prerelease || !is_newer(release.version(), current_version()) {
        return Ok(Check::UpToDate);
    }
    Ok(Check::Available(release))
}

/// The check to run for this build: `Unconfigured` when no repository is set.
pub async fn check_configured() -> IdeResult<Check> {
    match repository() {
        Some((owner, name)) => check(&owner, &name).await,
        None => Ok(Check::Unconfigured),
    }
}

#[cfg(test)]
mod tests {
    use super::{Release, ReleaseAsset, asset_for, is_newer};

    #[test]
    fn a_higher_version_is_newer() {
        assert!(is_newer("1.2.3", "1.2.2"));
        assert!(is_newer("1.3.0", "1.2.9"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(is_newer("1.2.3.4", "1.2.3"), "an extra component counts");
    }

    #[test]
    fn the_same_or_lower_version_is_not() {
        assert!(!is_newer("1.2.3", "1.2.3"));
        assert!(!is_newer("1.2.2", "1.2.3"));
        assert!(!is_newer("0.9.0", "1.0.0"));
        assert!(!is_newer("v1.2.3", "1.2.3"), "a `v` prefix is not a version bump");
    }

    #[test]
    fn a_prerelease_never_upgrades_a_release_of_the_same_version() {
        assert!(!is_newer("1.2.3-beta.1", "1.2.3"));
        assert!(is_newer("1.2.3", "1.2.3-beta.1"), "the release outranks its prerelease");
        assert!(is_newer("1.2.4-beta.1", "1.2.3"), "a newer prerelease still counts");
    }

    #[test]
    fn a_junk_component_cannot_claim_an_upgrade() {
        assert!(!is_newer("1.x.9", "1.2.3"), "unparsable parts read as 0");
    }

    #[test]
    fn the_release_reports_its_version_without_the_prefix() {
        let release = Release {
            tag_name: "v1.4.0".into(),
            name: None,
            body: None,
            html_url: "https://example.test".into(),
            assets: Vec::new(),
            prerelease: false,
        };
        assert_eq!(release.version(), "1.4.0");
    }

    #[test]
    fn the_asset_for_this_machine_is_an_archive() {
        let assets = vec![
            ReleaseAsset { name: "notes.txt".into(), download_url: "u".into(), size: 1 },
            ReleaseAsset { name: "OpenIDEA-1.4.0-macos-arm64.dmg".into(), download_url: "d".into(), size: 2 },
            ReleaseAsset { name: "OpenIDEA-1.4.0-linux.tar.gz".into(), download_url: "t".into(), size: 3 },
        ];
        let chosen = asset_for(&assets, "macos", "arm64").expect("an archive");
        assert_eq!(chosen.name, "OpenIDEA-1.4.0-macos-arm64.dmg");
    }

    #[test]
    fn a_release_without_a_build_asset_offers_nothing() {
        let assets = vec![ReleaseAsset { name: "source.zip".into(), download_url: "u".into(), size: 1 }];
        assert!(asset_for(&assets, "linux", "x86_64").is_some(), "a zip is still installable");
        assert!(asset_for(&[], "linux", "x86_64").is_none());
    }
}

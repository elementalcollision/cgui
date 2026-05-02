//! Phase-1 update detection. Queries GitHub Releases for the installed
//! container runtime and for cgui itself, compares against the running
//! version, and surfaces an `UpdateInfo` when a newer release exists.
//!
//! Strictly read-only at this phase — no download, no install, no auto-upgrade.
//! The status bar gets a chip; `cgui doctor` gets a section; nothing destructive.
//!
//! Network is minimal: 24h cache means at most ~2 GitHub API calls per repo
//! per day. Uses `curl` (always present on macOS) so we don't add an HTTPS
//! client dependency just for two endpoints.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Component {
    AppleContainer,
    CguiSelf,
}

impl Component {
    pub fn label(self) -> &'static str {
        match self {
            Component::AppleContainer => "container",
            Component::CguiSelf => "cgui",
        }
    }
    pub fn repo(self) -> &'static str {
        match self {
            Component::AppleContainer => "apple/container",
            Component::CguiSelf => "elementalcollision/cgui",
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // published_at is surfaced by future phases (modal/notes)
pub struct UpdateInfo {
    pub component: Component,
    pub installed: String,
    pub latest: String,
    pub release_url: String,
    pub published_at: String,
}

/// Cached snapshot of one component's most recent check. Persisted in
/// `state.json` so we don't re-hit the GitHub API every refresh.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CachedRelease {
    pub component: String,        // Component label
    pub latest_tag: String,
    pub release_url: String,
    pub published_at: String,
    pub fetched_at: u64,          // unix seconds
}

const CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// Public entry point. Returns one `UpdateInfo` per component that is
/// behind its latest release. Honours the user's opt-out for *automatic*
/// callers (TUI background task, doctor); explicit `cgui update` should
/// call `check_force` to bypass the gate.
pub async fn check(prefs: &mut crate::prefs::Prefs) -> Vec<UpdateInfo> {
    if prefs.auto_update_check == Some(false) {
        return Vec::new();
    }
    check_force(prefs).await
}

/// Same as `check` but ignores the opt-out — used by the explicit
/// `cgui update` subcommand where the user has typed the verb themselves.
pub async fn check_force(prefs: &mut crate::prefs::Prefs) -> Vec<UpdateInfo> {
    let mut out = Vec::new();
    for c in [Component::AppleContainer, Component::CguiSelf] {
        if let Some(info) = check_component(prefs, c).await {
            out.push(info);
        }
    }
    out
}

async fn check_component(
    prefs: &mut crate::prefs::Prefs,
    c: Component,
) -> Option<UpdateInfo> {
    let installed = installed_version(c)?;

    // Try cache first.
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let cached = prefs
        .update_cache
        .iter()
        .find(|cr| cr.component == c.label() && now.saturating_sub(cr.fetched_at) < CACHE_TTL_SECS)
        .cloned();

    let latest = match cached {
        Some(cr) => cr,
        None => {
            let fresh = fetch_latest(c.repo()).await?;
            let cr = CachedRelease {
                component: c.label().to_string(),
                latest_tag: fresh.tag_name,
                release_url: fresh.html_url,
                published_at: fresh.published_at,
                fetched_at: now,
            };
            prefs
                .update_cache
                .retain(|x| x.component != c.label());
            prefs.update_cache.push(cr.clone());
            prefs.last_update_check = Some(now);
            prefs.save();
            cr
        }
    };

    if compare_versions(&installed, &latest.latest_tag) == std::cmp::Ordering::Less {
        Some(UpdateInfo {
            component: c,
            installed,
            latest: latest.latest_tag,
            release_url: latest.release_url,
            published_at: latest.published_at,
        })
    } else {
        None
    }
}

fn installed_version(c: Component) -> Option<String> {
    match c {
        Component::AppleContainer => {
            // `container --version` prints e.g. "container CLI version 0.12.3 (build: …)"
            let out = std::process::Command::new(crate::runtime::binary())
                .arg("--version")
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout);
            s.split_whitespace()
                .find(|t| {
                    let t = t.trim_start_matches('v');
                    parse_version(t).is_some()
                })
                .map(|s| s.trim_start_matches('v').to_string())
        }
        Component::CguiSelf => Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    published_at: String,
}

async fn fetch_latest(repo: &str) -> Option<GhRelease> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        tokio::process::Command::new("curl")
            .args([
                "-sSL",
                "--max-time",
                "6",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "User-Agent: cgui",
                &url,
            ])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice::<GhRelease>(&out.stdout).ok()
}

/// Parse `MAJOR.MINOR.PATCH` (with optional leading `v`). Returns None for
/// any non-numeric or extra-suffix variant; we don't try to handle pre-release
/// tags in phase 1.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches('v');
    let mut it = s.split('.');
    let major: u32 = it.next()?.parse().ok()?;
    let minor: u32 = it.next()?.parse().ok()?;
    let patch_part = it.next()?;
    // Allow build/pre-release suffix on patch (e.g. "3-beta1") — take leading digits.
    let patch_digits: String = patch_part.chars().take_while(|c| c.is_ascii_digit()).collect();
    let patch: u32 = patch_digits.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(av), Some(bv)) => av.cmp(&bv),
        // If either side fails to parse, treat as equal so we don't false-alarm.
        _ => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn parse_clean() {
        assert_eq!(parse_version("0.12.3"), Some((0, 12, 3)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
    }
    #[test]
    fn parse_suffix() {
        assert_eq!(parse_version("0.12.3-beta1"), Some((0, 12, 3)));
    }
    #[test]
    fn parse_bad() {
        assert_eq!(parse_version("0.12"), None);
        assert_eq!(parse_version("not.a.version"), None);
    }
    #[test]
    fn cmp_works() {
        assert_eq!(compare_versions("0.12.3", "0.13.0"), Ordering::Less);
        assert_eq!(compare_versions("0.13.0", "0.12.3"), Ordering::Greater);
        assert_eq!(compare_versions("0.12.3", "0.12.3"), Ordering::Equal);
        assert_eq!(compare_versions("garbage", "0.1.0"), Ordering::Equal);
    }
}

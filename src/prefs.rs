//! Persistent user preferences.
//!
//! Best-effort: any IO error silently degrades to in-memory only. Stored at
//! `$XDG_CONFIG_HOME/cgui/state.json` (defaults to `~/.config/cgui/state.json`
//! on macOS — we deliberately don't follow Apple's `~/Library/Preferences`
//! convention because cgui is a CLI tool and dotfile-style config is friendlier
//! for terminal users.)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Prefs {
    /// Last active tab name (lowercased, e.g. "containers").
    pub tab: Option<String>,
    /// Sort key index per tab (`{"containers": 1, ...}`).
    pub sort: HashMap<String, u8>,
    /// Whether to show stopped containers as well as running.
    pub show_all: Option<bool>,
    /// Active runtime profile name (matches an entry in `profiles.toml`).
    pub profile: Option<String>,
    /// Recently-pulled image references, newest first, capped.
    #[serde(default)]
    pub recent_pulls: Vec<String>,
    /// Recently-built (context, tag) pairs, newest first, capped.
    #[serde(default)]
    pub recent_builds: Vec<RecentBuild>,
    /// Whether to query GitHub Releases for newer versions of the runtime
    /// and cgui itself. Default true; set to `false` for fully offline use.
    pub auto_update_check: Option<bool>,
    /// Unix-epoch seconds of the last successful update check.
    pub last_update_check: Option<u64>,
    /// Cached release snapshots, keyed by component label.
    #[serde(default)]
    pub update_cache: Vec<crate::update::CachedRelease>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RecentBuild {
    pub path: String,
    pub tag: Option<String>,
}

const RECENT_CAP: usize = 10;

impl Prefs {
    pub fn push_recent_pull(&mut self, reference: &str) {
        self.recent_pulls.retain(|r| r != reference);
        self.recent_pulls.insert(0, reference.to_string());
        self.recent_pulls.truncate(RECENT_CAP);
    }
    pub fn push_recent_build(&mut self, path: &str, tag: Option<&str>) {
        self.recent_builds
            .retain(|b| b.path != path || b.tag.as_deref() != tag);
        self.recent_builds.insert(
            0,
            RecentBuild {
                path: path.to_string(),
                tag: tag.map(|t| t.to_string()),
            },
        );
        self.recent_builds.truncate(RECENT_CAP);
    }
}

impl Prefs {
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            // Atomic-ish: write to a sibling tmp file and rename.
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("cgui").join("state.json"))
}

//! TUI state machine. Pure data + transitions; rendering lives in `ui`.

use crate::container::{self, Container, Image, Network, StatRow, Volume};
use crate::prefs::Prefs;
use crate::runtime::{self, Profile};
use crate::theme::Theme;
use std::path::PathBuf;
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tab {
    Containers,
    Images,
    Volumes,
    Networks,
    Logs,
    Stacks,
}

impl Tab {
    pub const ALL: &'static [Tab] = &[
        Tab::Containers,
        Tab::Images,
        Tab::Volumes,
        Tab::Networks,
        Tab::Stacks,
        Tab::Logs,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Tab::Containers => "Containers",
            Tab::Images => "Images",
            Tab::Volumes => "Volumes",
            Tab::Networks => "Networks",
            Tab::Stacks => "Stacks",
            Tab::Logs => "Logs",
        }
    }
    pub fn key(self) -> &'static str {
        // Stable, lowercase token for serialization.
        match self {
            Tab::Containers => "containers",
            Tab::Images => "images",
            Tab::Volumes => "volumes",
            Tab::Networks => "networks",
            Tab::Stacks => "stacks",
            Tab::Logs => "logs",
        }
    }
    pub fn from_key(s: &str) -> Option<Tab> {
        Self::ALL.iter().copied().find(|t| t.key() == s)
    }
}

/// Top-level interaction mode. Controls how keystrokes are dispatched and
/// which overlay (if any) is rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Browse,
    Filter,
    PromptPull,
    Detail,
    PullProgress,
    /// Two-field prompt for `container build`: context path + tag.
    PromptBuild,
    /// Search-as-you-type within the Logs tab. Highlights matching substrings
    /// in place; Enter exits but keeps the highlight; Esc clears.
    LogSearch,
    /// Help overlay listing keybindings for the current tab.
    Help,
    /// Right-click context menu near the click position.
    ContextMenu,
    /// File picker for choosing a build context directory.
    FilePicker,
    /// Pick a runtime profile (which container CLI to shell out to).
    ProfilePicker,
    /// Single-field prompt for a new stack name.
    PromptStackName,
    /// Parsed Trivy scan results modal.
    TrivyResult,
    /// Phase-2 update prompt: shows release notes and lets the user dismiss
    /// or open the release URL. No download/install yet.
    UpdatePrompt,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OperationKind {
    Pull,
    Build,
    Trivy,
    StackUp,
    StackDown,
    UpdateDownload,
}

impl OperationKind {
    pub fn verb(self) -> &'static str {
        match self {
            OperationKind::Pull => "pull",
            OperationKind::Build => "build",
            OperationKind::Trivy => "scan",
            OperationKind::StackUp => "stack up",
            OperationKind::StackDown => "stack down",
            OperationKind::UpdateDownload => "download",
        }
    }
    pub fn participle(self) -> &'static str {
        match self {
            OperationKind::Pull => "Pulling",
            OperationKind::Build => "Building",
            OperationKind::Trivy => "Scanning",
            OperationKind::StackUp => "Bringing up",
            OperationKind::StackDown => "Tearing down",
            OperationKind::UpdateDownload => "Downloading",
        }
    }
    pub fn done(self) -> &'static str {
        match self {
            OperationKind::Pull => "Pulled",
            OperationKind::Build => "Built",
            OperationKind::Trivy => "Scanned",
            OperationKind::StackUp => "Stack up",
            OperationKind::StackDown => "Stack down",
            OperationKind::UpdateDownload => "Downloaded",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContextMenu {
    pub x: u16,
    pub y: u16,
    pub items: Vec<(String, ContextAction)>,
    pub selected: usize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ContextAction {
    Inspect,
    Logs,
    Start,
    Stop,
    Kill,
    Delete,
    Exec,
    Pull,
    TrivyScan,
    StackUp,
    StackDown,
    Refresh,
    ToggleAll,
    Help,
}

#[derive(Copy, Clone, Debug)]
pub struct SortKey(pub u8);

impl SortKey {
    pub fn options(tab: Tab) -> &'static [&'static str] {
        match tab {
            Tab::Containers => &["id", "image", "status"],
            Tab::Images => &["reference", "size"],
            Tab::Volumes => &["name", "driver"],
            Tab::Networks => &["id", "state"],
            Tab::Stacks => &["name"],
            Tab::Logs => &[],
        }
    }
    pub fn label(self, tab: Tab) -> &'static str {
        let opts = Self::options(tab);
        if opts.is_empty() {
            "—"
        } else {
            opts[(self.0 as usize) % opts.len()]
        }
    }
    pub fn cycle(self, tab: Tab) -> Self {
        let n = Self::options(tab).len().max(1);
        SortKey(((self.0 as usize + 1) % n) as u8)
    }
}

pub struct App {
    pub running: bool,
    pub tab: Tab,
    pub show_all: bool,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub networks: Vec<Network>,
    pub stats: Vec<StatRow>,
    pub selected: usize,
    pub status: String,
    pub status_set_at: Instant,
    pub logs: String,
    pub log_target: Option<String>,
    pub cpu_history: VecDeque<f64>,
    pub mem_history: VecDeque<f64>,
    pub last_refresh: Instant,

    // New: filter / sort / mode / detail / pull state.
    pub mode: Mode,
    pub filter: String,
    pub prompt_buf: String,
    pub sort_key: SortKey,
    pub detail: String,
    pub detail_scroll: u16,
    pub pull_log: Arc<Mutex<Vec<String>>>,
    pub pull_running: bool,

    /// Container ids the user has multi-selected with Space. Batch verbs
    /// operate on this set when non-empty (else fall back to the highlighted
    /// row only).
    pub marked: HashSet<String>,

    /// Search query active on the Logs tab.
    pub log_search: String,
    /// What we're currently pulling, if anything (used to label the gauge and
    /// the backgrounded-pull indicator in the status bar).
    pub pull_reference: Option<String>,

    /// Persisted per-tab sort key — kept here in sync with `sort_key` for the
    /// active tab so we don't lose sort choice when switching away and back.
    pub sort_keys: HashMap<String, u8>,

    /// Cached layout rects from the most recent draw, used for mouse hit
    /// testing. None until first draw.
    pub layout: LayoutCache,

    /// Loaded-from-disk and saved-back preferences.
    pub prefs: Prefs,

    /// Color theme (loaded once at startup).
    pub theme: Theme,

    /// What kind of operation is currently running in the modal (pull or build).
    pub op_kind: OperationKind,

    /// Path of the build context for an in-flight `container build`.
    pub build_path: String,
    /// Two-field prompt buffer for build: 0 = path, 1 = tag.
    pub build_field: u8,
    pub build_tag: String,

    /// Scroll offsets for long views — wheel events bump these.
    pub log_scroll: u16,
    pub op_scroll: u16,

    /// Right-click context menu, when open.
    pub context_menu: Option<ContextMenu>,

    /// Recent CPU% samples per container id, capped (~20 samples = ~40s of
    /// history at the 2s refresh interval). Drives the sparkline column.
    pub cpu_history_per_id: HashMap<String, VecDeque<f64>>,

    /// Whether log search treats the query as a regex.
    pub log_search_regex: bool,

    /// File picker state.
    pub picker: PickerState,

    /// Loaded runtime profiles + active selection.
    pub profiles: Vec<Profile>,
    pub profile_picker_selected: usize,

    /// Streaming log buffer used by both one-shot fetch and `logs -f` follow.
    pub logs_buf: Arc<Mutex<Vec<String>>>,
    /// Whether a follow stream is currently active.
    pub log_following: bool,

    /// Recent-preset navigation state for the pull/build prompts. None means
    /// the user is editing freely; Some(i) means they're scrolling through
    /// recents and `typed_*` holds whatever they had typed before they
    /// started navigating.
    pub recent_idx: Option<usize>,
    pub typed_pull: String,
    pub typed_build_path: String,
    pub typed_build_tag: String,

    /// Loaded compose-style stacks from `~/.config/cgui/stacks/*.toml`.
    pub stacks: Vec<crate::stacks::Stack>,

    /// Latest parsed Trivy report, if a scan finished successfully.
    pub trivy_report: Option<crate::trivy::Report>,
    pub trivy_scroll: u16,
    /// Severity filter for the trivy results table; None shows all.
    pub trivy_filter: Option<crate::trivy::Severity>,
    /// Substring search across CVE id / package / title.
    pub trivy_search: String,
    /// True while the user is typing into the trivy search bar.
    pub trivy_search_active: bool,
    /// Captured trivy JSON body (filled by spawn_trivy on completion).
    pub trivy_json: Arc<Mutex<String>>,

    /// Healthcheck state per (stack, service). Driven by the background
    /// healthcheck/restart task.
    pub health: HashMap<(String, String), HealthEntry>,

    /// Available upgrades for runtime / cgui itself, populated by the
    /// background update-check task. Empty unless `prefs.auto_update_check`
    /// is true and a newer release exists.
    pub updates: Vec<crate::update::UpdateInfo>,
    /// Component labels the user dismissed *for this session* via
    /// `[L]ater`. Filtered out of chip rendering; cleared on restart.
    pub dismissed_updates: HashSet<String>,
    /// Index into `visible_updates()` for the modal.
    pub update_modal_idx: usize,
    /// Scroll offset in the modal's release-notes pane.
    pub update_notes_scroll: u16,

    /// Where the most recent successful update download landed. Used by
    /// phase 4 (`installer`) and surfaced in the status bar after `[D]ownload`.
    pub download_result: Arc<Mutex<Option<std::path::PathBuf>>>,
}

#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // last_check is recorded for future "stale" rendering
pub struct HealthEntry {
    pub ok: Option<bool>,
    pub last_check: Option<std::time::SystemTime>,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct PickerState {
    pub path: PathBuf,
    pub entries: Vec<PickerEntry>,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct PickerEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Default, Clone, Debug)]
pub struct LayoutCache {
    pub tabs: Option<Rect>,
    pub body: Option<Rect>,
}

impl App {
    pub fn new() -> Self {
        let prefs = Prefs::load();
        let tab = prefs
            .tab
            .as_deref()
            .and_then(Tab::from_key)
            .unwrap_or(Tab::Containers);
        let show_all = prefs.show_all.unwrap_or(true);
        let sort_keys: HashMap<String, u8> = prefs.sort.clone();
        let sort_key = SortKey(sort_keys.get(tab.key()).copied().unwrap_or(0));

        // Activate the saved profile (or profiles.toml's `default`) before any
        // container.rs call goes out, so the very first refresh hits the
        // intended runtime.
        let profiles = runtime::load_profiles();
        let want = prefs
            .profile
            .clone()
            .or_else(runtime::default_name)
            .unwrap_or_else(|| profiles[0].name.clone());
        if let Some(p) = profiles.iter().find(|p| p.name == want) {
            runtime::set_active(p);
        }
        let profile_picker_selected = profiles
            .iter()
            .position(|p| p.name == runtime::name())
            .unwrap_or(0);
        Self {
            tab,
            show_all,
            sort_key,
            sort_keys,
            prefs,
            layout: LayoutCache::default(),
            running: true,
            containers: vec![],
            images: vec![],
            volumes: vec![],
            networks: vec![],
            stats: vec![],
            selected: 0,
            status: default_status().into(),
            status_set_at: Instant::now(),
            logs: String::new(),
            log_target: None,
            cpu_history: VecDeque::with_capacity(120),
            mem_history: VecDeque::with_capacity(120),
            last_refresh: Instant::now() - std::time::Duration::from_secs(60),
            mode: Mode::Browse,
            filter: String::new(),
            prompt_buf: String::new(),
            detail: String::new(),
            detail_scroll: 0,
            pull_log: Arc::new(Mutex::new(Vec::new())),
            pull_running: false,
            marked: HashSet::new(),
            log_search: String::new(),
            pull_reference: None,
            theme: Theme::load(),
            op_kind: OperationKind::Pull,
            build_path: String::new(),
            build_field: 0,
            build_tag: String::new(),
            log_scroll: 0,
            op_scroll: 0,
            context_menu: None,
            cpu_history_per_id: HashMap::new(),
            log_search_regex: false,
            picker: PickerState::default(),
            profiles,
            profile_picker_selected,
            logs_buf: Arc::new(Mutex::new(Vec::new())),
            log_following: false,
            recent_idx: None,
            typed_pull: String::new(),
            typed_build_path: String::new(),
            typed_build_tag: String::new(),
            stacks: {
                let _ = crate::stacks::ensure_sample();
                crate::stacks::load_all()
            },
            trivy_report: None,
            trivy_scroll: 0,
            trivy_filter: None,
            trivy_search: String::new(),
            trivy_search_active: false,
            trivy_json: Arc::new(Mutex::new(String::new())),
            health: HashMap::new(),
            updates: Vec::new(),
            dismissed_updates: HashSet::new(),
            update_modal_idx: 0,
            update_notes_scroll: 0,
            download_result: Arc::new(Mutex::new(None)),
        }
    }

    /// Push a directory listing into the picker.
    pub fn picker_load(&mut self, path: PathBuf) {
        let mut entries: Vec<PickerEntry> = vec![PickerEntry {
            name: "..".into(),
            is_dir: true,
        }];
        if let Ok(rd) = std::fs::read_dir(&path) {
            let mut rest: Vec<PickerEntry> = rd
                .flatten()
                .filter_map(|de| {
                    let name = de.file_name().to_string_lossy().into_owned();
                    // Hide dotfiles to keep the picker readable.
                    if name.starts_with('.') {
                        return None;
                    }
                    let is_dir = de.file_type().ok().map(|t| t.is_dir()).unwrap_or(false);
                    Some(PickerEntry { name, is_dir })
                })
                .collect();
            rest.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            entries.extend(rest);
        }
        self.picker = PickerState {
            path,
            entries,
            selected: 0,
        };
    }

    /// True/false flips ~once per 500 ms; used by ui to pulse alerting rows.
    pub fn pulse_phase(&self) -> bool {
        (self.last_refresh.elapsed().as_millis() / 500) % 2 == 0
    }

    /// Cycle pull-prompt history. `dir = +1` = older, `-1` = newer.
    pub fn cycle_recent_pull(&mut self, dir: i32) {
        let recents = self.prefs.recent_pulls.clone();
        if recents.is_empty() {
            return;
        }
        match (self.recent_idx, dir) {
            (None, d) if d > 0 => {
                self.typed_pull = self.prompt_buf.clone();
                self.recent_idx = Some(0);
                self.prompt_buf = recents[0].clone();
            }
            (Some(0), d) if d < 0 => {
                self.recent_idx = None;
                self.prompt_buf = std::mem::take(&mut self.typed_pull);
            }
            (Some(i), d) => {
                let n = recents.len();
                let new_i = (i as i32 + d).clamp(0, n as i32 - 1) as usize;
                self.recent_idx = Some(new_i);
                self.prompt_buf = recents[new_i].clone();
            }
            _ => {}
        }
    }

    /// Cycle build-prompt history. Replaces both fields together.
    pub fn cycle_recent_build(&mut self, dir: i32) {
        let recents = self.prefs.recent_builds.clone();
        if recents.is_empty() {
            return;
        }
        match (self.recent_idx, dir) {
            (None, d) if d > 0 => {
                self.typed_build_path = self.build_path.clone();
                self.typed_build_tag = self.build_tag.clone();
                self.recent_idx = Some(0);
                self.build_path = recents[0].path.clone();
                self.build_tag = recents[0].tag.clone().unwrap_or_default();
            }
            (Some(0), d) if d < 0 => {
                self.recent_idx = None;
                self.build_path = std::mem::take(&mut self.typed_build_path);
                self.build_tag = std::mem::take(&mut self.typed_build_tag);
            }
            (Some(i), d) => {
                let n = recents.len();
                let new_i = (i as i32 + d).clamp(0, n as i32 - 1) as usize;
                self.recent_idx = Some(new_i);
                self.build_path = recents[new_i].path.clone();
                self.build_tag = recents[new_i].tag.clone().unwrap_or_default();
            }
            _ => {}
        }
    }

    /// Updates not dismissed for the current session. Drives both the
    /// status-bar chip and the modal cycler.
    pub fn visible_updates(&self) -> Vec<&crate::update::UpdateInfo> {
        self.updates
            .iter()
            .filter(|u| !self.dismissed_updates.contains(u.component.label()))
            .collect()
    }

    /// Current update for the modal, clamped if items were dismissed.
    pub fn current_update(&self) -> Option<crate::update::UpdateInfo> {
        let v = self.visible_updates();
        if v.is_empty() {
            None
        } else {
            let i = self.update_modal_idx.min(v.len() - 1);
            Some(v[i].clone())
        }
    }

    /// Activate a runtime profile and persist it.
    pub fn select_profile(&mut self, idx: usize) {
        if let Some(p) = self.profiles.get(idx).cloned() {
            runtime::set_active(&p);
            self.prefs.profile = Some(p.name.clone());
            self.save_prefs();
            self.set_status(format!("runtime: {} ({})", p.name, p.binary));
        }
    }

    /// Persist current preferences. Cheap; safe to call after any change.
    pub fn save_prefs(&mut self) {
        self.prefs.tab = Some(self.tab.key().to_string());
        self.prefs.show_all = Some(self.show_all);
        self.prefs.sort = self.sort_keys.clone();
        self.prefs.profile = Some(runtime::name());
        self.prefs.save();
    }

    /// Whether a pull's modal is worth re-opening: either it's running, or it
    /// finished and the log buffer still has content to show.
    pub fn pull_attachable(&self) -> bool {
        if self.pull_running {
            return true;
        }
        match self.pull_log.lock() {
            Ok(v) => !v.is_empty(),
            Err(_) => false,
        }
    }

    /// Toggle multi-select mark on the currently highlighted container.
    pub fn toggle_mark_current_container(&mut self) {
        if let Some(id) = self.current_container_id() {
            if !self.marked.remove(&id) {
                self.marked.insert(id);
            }
        }
    }

    /// Container ids to act on for a verb: the marked set if non-empty, else
    /// just the currently highlighted row.
    pub fn target_container_ids(&self) -> Vec<String> {
        if !self.marked.is_empty() {
            let mut v: Vec<String> = self.marked.iter().cloned().collect();
            v.sort();
            v
        } else {
            self.current_container_id().into_iter().collect()
        }
    }

    /// Stats lookup keyed by container id/name. Used to overlay live CPU/MEM
    /// onto the Containers table.
    pub fn stats_by_id(&self) -> HashMap<String, (f64, u64, u64)> {
        let mut m = HashMap::with_capacity(self.stats.len());
        for s in &self.stats {
            let key = if !s.id.is_empty() { &s.id } else { &s.name };
            if !key.is_empty() {
                m.insert(key.clone(), (s.cpu_percent, s.memory_usage, s.memory_limit));
            }
        }
        m
    }

    pub fn next_tab(&mut self) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.set_tab(Tab::ALL[(i + 1) % Tab::ALL.len()]);
    }
    pub fn prev_tab(&mut self) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.set_tab(Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()]);
    }
    pub fn set_tab(&mut self, t: Tab) {
        if self.tab == t {
            return;
        }
        // Stash sort key for outgoing tab; restore for incoming.
        self.sort_keys.insert(self.tab.key().to_string(), self.sort_key.0);
        self.tab = t;
        self.sort_key = SortKey(self.sort_keys.get(t.key()).copied().unwrap_or(0));
        self.selected = 0;
        self.save_prefs();
    }

    /// Indices into the underlying tab data after filter+sort. The Logs tab
    /// returns empty.
    pub fn view_indices(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        let matches = |s: &str| f.is_empty() || s.to_lowercase().contains(&f);
        let key = self.sort_key.label(self.tab);
        match self.tab {
            Tab::Containers => sorted(
                self.containers
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| matches(&c.id) || matches(&c.image) || matches(&c.status))
                    .map(|(i, _)| i)
                    .collect(),
                |&i| match key {
                    "image" => self.containers[i].image.clone(),
                    "status" => self.containers[i].status.clone(),
                    _ => self.containers[i].id.clone(),
                },
            ),
            Tab::Images => sorted(
                self.images
                    .iter()
                    .enumerate()
                    .filter(|(_, im)| matches(&im.reference))
                    .map(|(i, _)| i)
                    .collect(),
                |&i| match key {
                    "size" => self.images[i].size.clone(),
                    _ => self.images[i].reference.clone(),
                },
            ),
            Tab::Volumes => sorted(
                self.volumes
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| matches(&v.name) || matches(&v.source))
                    .map(|(i, _)| i)
                    .collect(),
                |&i| match key {
                    "driver" => self.volumes[i].driver.clone(),
                    _ => self.volumes[i].name.clone(),
                },
            ),
            Tab::Networks => sorted(
                self.networks
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| matches(&n.id) || matches(&n.state))
                    .map(|(i, _)| i)
                    .collect(),
                |&i| match key {
                    "state" => self.networks[i].state.clone(),
                    _ => self.networks[i].id.clone(),
                },
            ),
            Tab::Logs => vec![],
            Tab::Stacks => sorted(
                self.stacks
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| matches(&s.name))
                    .map(|(i, _)| i)
                    .collect(),
                |&i| self.stacks[i].name.clone(),
            ),
        }
    }

    pub fn row_count(&self) -> usize {
        self.view_indices().len()
    }
    pub fn move_down(&mut self) {
        let n = self.row_count();
        if n > 0 {
            self.selected = (self.selected + 1).min(n - 1);
        }
    }
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Return the underlying-array index of the currently highlighted row.
    pub fn selected_row(&self) -> Option<usize> {
        self.view_indices().get(self.selected).copied()
    }

    pub fn current_container_id(&self) -> Option<String> {
        self.selected_row()
            .and_then(|i| self.containers.get(i).map(|c| c.id.clone()))
    }
    pub fn current_image_ref(&self) -> Option<String> {
        self.selected_row()
            .and_then(|i| self.images.get(i).map(|im| im.reference.clone()))
    }
    pub fn current_stack(&self) -> Option<crate::stacks::Stack> {
        self.selected_row()
            .and_then(|i| self.stacks.get(i).cloned())
    }
    pub fn reload_stacks(&mut self) {
        self.stacks = crate::stacks::load_all();
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_set_at = Instant::now();
    }
    pub fn reset_status(&mut self) {
        self.status = default_status().into();
    }

    /// Apply a `RefreshResult` produced by `fetch_all()`. Pure; safe to call
    /// from anywhere on the event loop.
    pub fn apply_refresh(&mut self, r: RefreshResult) {
        if let Some(v) = r.containers {
            self.containers = v;
        }
        if let Some(v) = r.images {
            self.images = v;
        }
        if let Some(v) = r.volumes {
            self.volumes = v;
        }
        if let Some(v) = r.networks {
            self.networks = v;
        }
        if let Some(v) = r.stats {
            let total_cpu: f64 = v.iter().map(|s| s.cpu_percent).sum();
            let used: u64 = v.iter().map(|s| s.memory_usage).sum();
            let limit: u64 = v.iter().map(|s| s.memory_limit).sum();
            for s in &v {
                let key = if !s.id.is_empty() { &s.id } else { &s.name };
                if key.is_empty() {
                    continue;
                }
                let q = self
                    .cpu_history_per_id
                    .entry(key.clone())
                    .or_insert_with(|| VecDeque::with_capacity(20));
                push_capped(q, s.cpu_percent, 20);
            }
            self.stats = v;
            push_capped(&mut self.cpu_history, total_cpu, 120);
            let mem_pct = if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            };
            push_capped(&mut self.mem_history, mem_pct, 120);
        }
        if let Some(e) = r.error {
            self.set_status(e);
        }
        let n = self.row_count();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
        self.last_refresh = Instant::now();
    }
}

/// Snapshot of every CLI list call. Pure data — no `&mut App` involved, so
/// the fetch can run as a detached tokio task and the event loop never
/// blocks waiting on the runtime CLI.
#[derive(Default, Debug, Clone)]
pub struct RefreshResult {
    pub containers: Option<Vec<container::Container>>,
    pub images: Option<Vec<container::Image>>,
    pub volumes: Option<Vec<container::Volume>>,
    pub networks: Option<Vec<container::Network>>,
    pub stats: Option<Vec<container::StatRow>>,
    pub error: Option<String>,
}

/// Run every list call in parallel and collect the results. Errors on any
/// one call are recorded into `error` (last writer wins) but never abort
/// the others — partial results are better than none.
pub async fn fetch_all(show_all: bool) -> RefreshResult {
    let (c, i, v, n, s) = tokio::join!(
        container::list_containers(show_all),
        container::list_images(),
        container::list_volumes(),
        container::list_networks(),
        container::stats_snapshot(),
    );
    let mut out = RefreshResult::default();
    match c {
        Ok(v) => out.containers = Some(v),
        Err(e) => out.error = Some(format!("ls: {e}")),
    }
    out.images = i.ok();
    out.volumes = v.ok();
    out.networks = n.ok();
    out.stats = s.ok();
    out
}

pub fn default_status() -> &'static str {
    "q quit · ←→ tabs · ↑↓ select · Space mark · Enter inspect · / filter · o sort · r refresh · a all · s/x/K/d/l/e · p pull"
}

fn sorted<F>(mut v: Vec<usize>, key: F) -> Vec<usize>
where
    F: Fn(&usize) -> String,
{
    v.sort_by_key(&key);
    v
}

fn push_capped(q: &mut VecDeque<f64>, v: f64, cap: usize) {
    if q.len() == cap {
        q.pop_front();
    }
    q.push_back(v);
}

//! TUI state machine. Pure data + transitions; rendering lives in `ui`.

use crate::container::{self, Container, Image, Network, StatRow, Volume};
use anyhow::Result;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tab {
    Containers,
    Images,
    Volumes,
    Networks,
    Logs,
}

impl Tab {
    pub const ALL: &'static [Tab] = &[
        Tab::Containers,
        Tab::Images,
        Tab::Volumes,
        Tab::Networks,
        Tab::Logs,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Tab::Containers => "Containers",
            Tab::Images => "Images",
            Tab::Volumes => "Volumes",
            Tab::Networks => "Networks",
            Tab::Logs => "Logs",
        }
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
}

impl App {
    pub fn new() -> Self {
        Self {
            running: true,
            tab: Tab::Containers,
            show_all: true,
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
            sort_key: SortKey(0),
            detail: String::new(),
            detail_scroll: 0,
            pull_log: Arc::new(Mutex::new(Vec::new())),
            pull_running: false,
        }
    }

    pub fn next_tab(&mut self) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(i + 1) % Tab::ALL.len()];
        self.selected = 0;
        self.sort_key = SortKey(0);
    }
    pub fn prev_tab(&mut self) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()];
        self.selected = 0;
        self.sort_key = SortKey(0);
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

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_set_at = Instant::now();
    }
    pub fn reset_status(&mut self) {
        self.status = default_status().into();
    }

    pub async fn refresh(&mut self) -> Result<()> {
        match container::list_containers(self.show_all).await {
            Ok(v) => self.containers = v,
            Err(e) => self.set_status(format!("ls error: {e}")),
        }
        if let Ok(v) = container::list_images().await {
            self.images = v;
        }
        if let Ok(v) = container::list_volumes().await {
            self.volumes = v;
        }
        if let Ok(v) = container::list_networks().await {
            self.networks = v;
        }
        if let Ok(v) = container::stats_snapshot().await {
            let total_cpu: f64 = v.iter().map(|s| s.cpu_percent).sum();
            let used: u64 = v.iter().map(|s| s.memory_usage).sum();
            let limit: u64 = v.iter().map(|s| s.memory_limit).sum();
            self.stats = v;
            push_capped(&mut self.cpu_history, total_cpu, 120);
            let mem_pct = if limit > 0 {
                (used as f64 / limit as f64) * 100.0
            } else {
                0.0
            };
            push_capped(&mut self.mem_history, mem_pct, 120);
        }
        let n = self.row_count();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
        self.last_refresh = Instant::now();
        Ok(())
    }
}

pub fn default_status() -> &'static str {
    "q quit · ←→ tabs · ↑↓ select · Enter inspect · / filter · o sort · r refresh · a all · s/x/K/d/l/e · p pull"
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

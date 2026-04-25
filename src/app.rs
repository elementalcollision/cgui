//! TUI state machine. Pure data + transitions; rendering lives in `ui`.

use crate::container::{self, Container, Image, Network, StatRow, Volume};
use anyhow::Result;
use std::collections::VecDeque;
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
            status: "Ready — q quit · ←→ tabs · ↑↓ select · r refresh · s start · x stop · k kill · d delete · l logs · a all".into(),
            status_set_at: Instant::now(),
            logs: String::new(),
            log_target: None,
            cpu_history: VecDeque::with_capacity(120),
            mem_history: VecDeque::with_capacity(120),
            last_refresh: Instant::now() - std::time::Duration::from_secs(60),
        }
    }

    pub fn next_tab(&mut self) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(i + 1) % Tab::ALL.len()];
        self.selected = 0;
    }
    pub fn prev_tab(&mut self) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()];
        self.selected = 0;
    }

    pub fn row_count(&self) -> usize {
        match self.tab {
            Tab::Containers => self.containers.len(),
            Tab::Images => self.images.len(),
            Tab::Volumes => self.volumes.len(),
            Tab::Networks => self.networks.len(),
            Tab::Logs => 0,
        }
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

    pub fn current_container_id(&self) -> Option<String> {
        self.containers.get(self.selected).map(|c| c.id.clone())
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_set_at = Instant::now();
    }

    pub async fn refresh(&mut self) -> Result<()> {
        // Don't fail the loop if any one source errors — record into status.
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
            // aggregate cpu/mem for sparkline
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
        self.selected = self
            .selected
            .min(self.row_count().saturating_sub(1).max(0));
        self.last_refresh = Instant::now();
        Ok(())
    }
}

fn push_capped(q: &mut VecDeque<f64>, v: f64, cap: usize) {
    if q.len() == cap {
        q.pop_front();
    }
    q.push_back(v);
}

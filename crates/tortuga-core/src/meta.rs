//! Tortuga-specific per-torrent data that librqbit does not store:
//! file priorities, piece order, category, timestamps, lifetime totals.

use std::{collections::HashMap, path::PathBuf};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::util::{now_ts, write_json_atomic};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilePriority {
    Skip,
    Low,
    #[default]
    Normal,
    High,
}

impl FilePriority {
    pub fn tier(self) -> u8 {
        match self {
            FilePriority::Skip => 0,
            FilePriority::Low => 1,
            FilePriority::Normal => librqbit::DEFAULT_FILE_TIER,
            FilePriority::High => 3,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TorrentMeta {
    pub priorities: Vec<FilePriority>,
    pub sequential: bool,
    pub category: Option<String>,
    pub added_at: i64,
    pub completed_at: Option<i64>,
    /// Paused by the user (as opposed to paused by the scheduler / network guard).
    pub user_paused: bool,
    /// Lifetime totals across restarts.
    pub total_downloaded: u64,
    pub total_uploaded: u64,
    pub transfer: crate::transfer::TransferStats,
    /// Magnet / source the torrent was added from, used to show pending/error rows.
    pub source: Option<String>,
}

impl TorrentMeta {
    pub fn new(sequential: bool) -> Self {
        Self {
            sequential,
            added_at: now_ts(),
            ..Default::default()
        }
    }

    pub fn piece_priority(&self, file_count: usize) -> librqbit::PiecePriorityConfig {
        librqbit::PiecePriorityConfig {
            file_tiers: (0..file_count)
                .map(|i| self.priorities.get(i).copied().unwrap_or_default().tier())
                .collect(),
            sequential: self.sequential,
        }
    }

    pub fn only_files(&self, file_count: usize) -> Option<Vec<usize>> {
        if !self.priorities.contains(&FilePriority::Skip) {
            return None;
        }
        Some(
            (0..file_count)
                .filter(|i| {
                    self.priorities.get(*i).copied().unwrap_or_default() != FilePriority::Skip
                })
                .collect(),
        )
    }
}

pub struct MetaStore {
    path: PathBuf,
    items: Mutex<HashMap<String, TorrentMeta>>,
}

impl MetaStore {
    pub fn load(path: PathBuf) -> Self {
        let items = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            items: Mutex::new(items),
        }
    }

    pub fn get(&self, hash: &str) -> Option<TorrentMeta> {
        self.items.lock().get(hash).cloned()
    }

    pub fn get_or_default(&self, hash: &str, default_sequential: bool) -> TorrentMeta {
        self.items
            .lock()
            .entry(hash.to_owned())
            .or_insert_with(|| TorrentMeta::new(default_sequential))
            .clone()
    }

    pub fn update<R>(&self, hash: &str, f: impl FnOnce(&mut TorrentMeta) -> R) -> R {
        let mut g = self.items.lock();
        let m = g
            .entry(hash.to_owned())
            .or_insert_with(|| TorrentMeta::new(false));
        f(m)
    }

    pub fn insert(&self, hash: &str, meta: TorrentMeta) {
        self.items.lock().insert(hash.to_owned(), meta);
    }

    pub fn remove(&self, hash: &str) {
        self.items.lock().remove(hash);
    }

    pub fn categories(&self) -> Vec<String> {
        let mut c: Vec<String> = self
            .items
            .lock()
            .values()
            .filter_map(|m| m.category.clone())
            .filter(|c| !c.is_empty())
            .collect();
        c.sort();
        c.dedup();
        c
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let snapshot = self.items.lock().clone();
        write_json_atomic(&self.path, &snapshot)
    }
}

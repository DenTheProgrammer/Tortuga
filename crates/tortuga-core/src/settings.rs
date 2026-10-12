use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::net::NetworkMode;

/// What the speed scheduler does in a given hour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleSlot {
    /// Regular limits.
    #[default]
    Normal,
    /// Alternative ("turtle") limits.
    Alt,
    /// All torrents paused.
    Off,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SchedulerSettings {
    pub enabled: bool,
    /// 7 days (Monday first) x 24 hours.
    pub grid: Vec<Vec<ScheduleSlot>>,
}

impl Default for SchedulerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            grid: vec![vec![ScheduleSlot::Normal; 24]; 7],
        }
    }
}

impl SchedulerSettings {
    pub fn slot(&self, weekday_from_monday: usize, hour: usize) -> ScheduleSlot {
        self.grid
            .get(weekday_from_monday)
            .and_then(|d| d.get(hour))
            .copied()
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NetworkSettings {
    pub mode: NetworkMode,
    /// Interface to use for "direct" mode. `None` = pick automatically.
    pub direct_interface: Option<String>,
    /// Interface to use for "VPN only" mode. `None` = pick automatically.
    pub vpn_interface: Option<String>,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            mode: NetworkMode::Auto,
            direct_interface: None,
            vpn_interface: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    // Interface
    pub language: String,
    pub theme: String,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    pub notify_on_complete: bool,

    // Streaming: "builtin", "system", "vlc", "mpv", "mpc-hc", "mpc-be", "potplayer" or "custom".
    pub player: String,
    /// Executable for the "custom" player (or to override detection).
    pub player_path: Option<String>,

    // Downloads
    pub download_dir: PathBuf,
    pub default_sequential: bool,
    pub start_paused: bool,
    /// Stop seeding once upload/download ratio reaches this value.
    pub seed_ratio_limit: Option<f64>,

    // Connection
    pub listen_port: u16,
    pub upnp: bool,
    pub dht: bool,
    pub lsd: bool,
    pub utp: bool,
    pub peer_limit: u32,
    /// socks5://[user:pass@]host:port
    pub proxy_url: Option<String>,
    pub network: NetworkSettings,

    // Speed, KiB/s, 0 = unlimited
    pub download_limit: u32,
    pub upload_limit: u32,
    pub alt_download_limit: u32,
    pub alt_upload_limit: u32,
    pub alt_speed_enabled: bool,
    pub scheduler: SchedulerSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: "ru".into(),
            theme: "dark".into(),
            close_to_tray: true,
            start_minimized: false,
            notify_on_complete: true,
            player: "builtin".into(),
            player_path: None,
            download_dir: default_download_dir(),
            default_sequential: false,
            start_paused: false,
            seed_ratio_limit: None,
            listen_port: 0,
            upnp: true,
            dht: true,
            lsd: true,
            utp: false,
            peer_limit: 128,
            proxy_url: None,
            network: NetworkSettings::default(),
            download_limit: 0,
            upload_limit: 0,
            alt_download_limit: 512,
            alt_upload_limit: 128,
            alt_speed_enabled: false,
            scheduler: SchedulerSettings::default(),
        }
    }
}

fn default_download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|d| d.download_dir().map(|p| p.to_path_buf()))
        .or_else(|| directories::UserDirs::new().map(|d| d.home_dir().join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("Downloads"))
        .join("Tortuga")
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut s: Settings = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if s.listen_port == 0 {
            // Pick a random high port once and keep it, like classic clients do.
            s.listen_port = rand::random_range(20000..60000);
        }
        if s.scheduler.grid.len() != 7 || s.scheduler.grid.iter().any(|d| d.len() != 24) {
            s.scheduler.grid = SchedulerSettings::default().grid;
        }
        s
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        crate::util::write_json_atomic(path, self)
    }

    /// Settings that require the BitTorrent session to be restarted.
    pub fn session_fingerprint(&self) -> impl PartialEq + use<> {
        (
            self.listen_port,
            self.upnp,
            self.dht,
            self.lsd,
            self.utp,
            self.peer_limit,
            self.proxy_url.clone(),
            self.download_dir.clone(),
        )
    }
}

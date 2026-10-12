//! Tortuga torrent engine.
//!
//! Wraps a (patched) librqbit session with everything a desktop client needs:
//! file priorities, persistence of client-side state, speed scheduler, RSS,
//! VPN-aware network routing, and a local HTTP API with streaming.

pub mod create;
pub mod dns;
pub mod engine;
pub mod meta;
pub mod net;
pub mod netguard;
pub mod rss;
pub mod server;
pub mod settings;
pub mod transfer;
pub mod util;

pub use engine::{Engine, Event};
pub use librqbit;
pub use settings::Settings;

/// Default data directory (settings, session state, RSS).
pub fn default_data_dir() -> std::path::PathBuf {
    directories::ProjectDirs::from("app", "Tortuga", "Tortuga")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(".tortuga"))
}

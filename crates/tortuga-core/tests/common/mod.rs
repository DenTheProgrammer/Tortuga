//! Helpers shared by the integration tests: engines on localhost without DHT,
//! a seeder, and polling.

#![allow(dead_code)]

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use tortuga_core::{
    Engine, Settings,
    create::{CreateRequest, CreateResponse},
    engine::{TorrentDetails, TorrentState, TorrentSummary},
    net::NetworkMode,
};

pub const MIB: usize = 1024 * 1024;

pub fn write_settings(dir: &Path, download_dir: &Path, upload_limit: u32) {
    let mut s = Settings {
        download_dir: download_dir.to_path_buf(),
        dht: false,
        upnp: false,
        lsd: false,
        upload_limit,
        ..Default::default()
    };
    s.network.mode = NetworkMode::System;
    std::fs::create_dir_all(dir).unwrap();
    s.save(&dir.join("settings.json")).unwrap();
}

/// Engine with its config in `root/name` and downloads in `root/name-dl`.
pub async fn engine(root: &Path, name: &str, upload_limit: u32) -> Arc<Engine> {
    let dir = root.join(name);
    if !dir.join("settings.json").exists() {
        write_settings(&dir, &root.join(format!("{name}-dl")), upload_limit);
    }
    Engine::new(dir).await.unwrap()
}

pub fn data(n: usize, seed: u8) -> Vec<u8> {
    let mut x: u32 = 0x2f6b ^ ((seed as u32) << 8) ^ seed as u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// Write `files` under `dir` (paths may contain `/`).
pub fn write_files(dir: &Path, files: &[(&str, Vec<u8>)]) {
    for (name, bytes) in files {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
}

/// Create a torrent from `path` on `seeder` and wait until it seeds.
pub async fn seed(
    seeder: &Arc<Engine>,
    path: &Path,
    piece_length: u32,
) -> (CreateResponse, String) {
    let created = tortuga_core::create::create(
        seeder,
        CreateRequest {
            path: path.to_string_lossy().into_owned(),
            piece_length: Some(piece_length),
            seed: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    wait(seeder, &created.id, 30, "seeding", |t| {
        matches!(t.state, TorrentState::Seeding)
    })
    .await;
    let peer = format!(
        "127.0.0.1:{}",
        seeder.state().session.listen_port.expect("listen port")
    );
    (created, peer)
}

pub fn get(e: &Engine, id: &str) -> Option<TorrentSummary> {
    e.state().torrents.into_iter().find(|t| t.id == id)
}

pub async fn wait(
    e: &Arc<Engine>,
    id: &str,
    secs: u64,
    what: &str,
    cond: impl Fn(&TorrentSummary) -> bool,
) -> TorrentSummary {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if let Some(t) = get(e, id).filter(|t| cond(t)) {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}: {:#?}", get(e, id));
}

pub async fn wait_details(
    e: &Arc<Engine>,
    id: &str,
    secs: u64,
    what: &str,
    cond: impl Fn(&TorrentDetails) -> bool,
) -> TorrentDetails {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if let Some(d) = e.details(id).ok().filter(|d| cond(d)) {
            return d;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "timed out waiting for {what}: {:#?}",
        e.details(id).map(|d| d.files)
    );
}

/// Without DHT/trackers, peers must be re-introduced after a session restart.
pub async fn reconnect(e: &Arc<Engine>, id: &str, peer: &str) {
    for _ in 0..100 {
        if e.add_peers(id, &[peer.to_owned()]).unwrap_or(0) > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn serve(e: &Arc<Engine>) -> String {
    let (addr, _task) =
        tortuga_core::server::serve(e.clone(), "127.0.0.1:0".parse().unwrap(), None)
            .await
            .unwrap();
    format!("http://{addr}")
}

pub fn file_index(d: &TorrentDetails, suffix: &str) -> usize {
    d.files
        .iter()
        .position(|f| f.path.ends_with(suffix))
        .unwrap_or_else(|| panic!("no file {suffix}"))
}

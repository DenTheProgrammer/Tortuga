//! Restart behaviour: resume where we left off, fast startup for complete
//! torrents, and sane handling of files deleted while Tortuga was closed.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use tortuga_core::{
    Engine, Settings,
    engine::{AddRequest, TorrentState, TorrentSummary},
    meta::FilePriority,
    net::NetworkMode,
};

fn settings(dir: &Path, download_dir: &Path, upload_limit: u32) {
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

fn data(n: usize, seed: u8) -> Vec<u8> {
    let mut x: u32 = 0x9e37 ^ seed as u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn get(e: &Engine, id: &str) -> Option<TorrentSummary> {
    e.state().torrents.into_iter().find(|t| t.id == id)
}

/// After a restart the leecher has no DHT/trackers in tests, so reconnect manually.
async fn reconnect(e: &Arc<Engine>, id: &str, peer: &str) {
    for _ in 0..100 {
        if e.add_peers(id, &[peer.to_owned()]).unwrap_or(0) > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait(
    e: &Arc<Engine>,
    id: &str,
    secs: u64,
    what: &str,
    cond: impl Fn(&TorrentSummary) -> bool,
) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        if get(e, id).as_ref().is_some_and(&cond) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}: {:#?}", get(e, id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_and_missing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let content = root.join("content/Show S01");
    std::fs::create_dir_all(&content).unwrap();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("Show.S01E01.mkv", data(1_500_000, 1)),
        ("Show.S01E02.mkv", data(1_500_000, 2)),
        ("Show.S01E03.mkv", data(1_500_000, 3)),
    ];
    for (n, d) in &files {
        std::fs::write(content.join(n), d).unwrap();
    }

    // Seeder uploads slowly so the leecher can be stopped half-way.
    settings(&root.join("a"), &root.join("a-dl"), 400);
    settings(&root.join("b"), &root.join("b-dl"), 0);
    let a = Engine::new(root.join("a")).await.unwrap();
    let created = tortuga_core::create::create(
        &a,
        tortuga_core::create::CreateRequest {
            path: content.to_string_lossy().into_owned(),
            piece_length: Some(64 * 1024),
            seed: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let id = created.id.clone();
    wait(&a, &id, 30, "seeder ready", |t| {
        matches!(t.state, TorrentState::Seeding)
    })
    .await;
    let peer = format!("127.0.0.1:{}", a.state().session.listen_port.unwrap());

    // 1. Download part of it, then "close the app".
    let b = Engine::new(root.join("b")).await.unwrap();
    b.add(AddRequest {
        torrent_base64: Some(created.torrent_base64.clone()),
        peers: Some(vec![peer.clone()]),
        ..Default::default()
    })
    .await
    .unwrap();
    // Series are detected and downloaded in order.
    assert!(get(&b, &id).unwrap().sequential);
    wait(&b, &id, 60, "partial download", |t| {
        t.state != TorrentState::Checking && t.progress > 0.3
    })
    .await;
    let before = get(&b, &id).unwrap().done;
    b.shutdown().await;
    drop(b);

    // 2. Restart: progress is kept and the startup check is quick.
    let started = Instant::now();
    let b = Engine::new(root.join("b")).await.unwrap();
    wait(&b, &id, 20, "resume after restart", |t| {
        !matches!(t.state, TorrentState::Checking) && t.done > 0
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "startup took {:?}",
        started.elapsed()
    );
    assert!(
        get(&b, &id).unwrap().done + 256 * 1024 >= before,
        "lost progress: {} < {before}",
        get(&b, &id).unwrap().done
    );
    reconnect(&b, &id, &peer).await;
    wait(&b, &id, 60, "complete", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    let out = std::path::PathBuf::from(get(&b, &id).unwrap().save_path);
    for (n, d) in &files {
        assert!(std::fs::read(out.join(n)).unwrap() == *d, "{n} differs");
    }
    b.shutdown().await;
    drop(b);

    // 3. A watched episode was deleted while the app was closed.
    std::fs::remove_file(out.join("Show.S01E01.mkv")).unwrap();
    let b = Engine::new(root.join("b")).await.unwrap();
    let mut events = b.subscribe();
    // The piece shared by E01 and E02 has to be fetched again.
    reconnect(&b, &id, &peer).await;
    wait(&b, &id, 20, "start after deletion", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    let details = b.details(&id).unwrap();
    let e01 = details
        .files
        .iter()
        .find(|f| f.path.ends_with("E01.mkv"))
        .unwrap();
    // It isn't downloaded again on its own.
    for _ in 0..50 {
        if b.details(&id).unwrap().files[e01.index].priority == FilePriority::Skip {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        b.details(&id).unwrap().files[e01.index].priority,
        FilePriority::Skip
    );
    let ev = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(tortuga_core::Event::FilesMissing { files, all, .. }) = events.recv().await {
                return (files, all);
            }
        }
    })
    .await
    .expect("FilesMissing event");
    assert_eq!(ev, (1, false));
    // The other episodes are intact.
    for (n, d) in &files[1..] {
        assert!(std::fs::read(out.join(n)).unwrap() == *d, "{n} differs");
    }
    b.shutdown().await;
    drop(b);

    // 4. The whole folder was deleted: torrent waits paused; Start re-downloads it.
    std::fs::remove_dir_all(&out).unwrap();
    let b = Engine::new(root.join("b")).await.unwrap();
    wait(&b, &id, 20, "paused after folder deletion", |t| {
        t.user_paused && t.done == 0
    })
    .await;
    b.resume(&id).await.unwrap();
    reconnect(&b, &id, &peer).await;
    wait(&b, &id, 60, "re-download", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    // E01 stays skipped (deleted on purpose in step 3); the rest is back.
    assert_eq!(
        b.details(&id).unwrap().files[e01.index].priority,
        FilePriority::Skip
    );
    for (n, d) in &files[1..] {
        assert!(std::fs::read(out.join(n)).unwrap() == *d, "{n} differs");
    }
    b.shutdown().await;
    a.shutdown().await;
}

//! Two engines on localhost: one seeds, one downloads, then stream over HTTP.

use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use tortuga_core::{
    Engine, Settings,
    engine::{AddRequest, TorrentState},
    meta::FilePriority,
    net::NetworkMode,
};

fn write_settings(dir: &Path, download_dir: &Path) {
    let mut s = Settings {
        download_dir: download_dir.to_path_buf(),
        dht: false,
        upnp: false,
        lsd: false,
        ..Default::default()
    };
    s.network.mode = NetworkMode::System;
    std::fs::create_dir_all(dir).unwrap();
    s.save(&dir.join("settings.json")).unwrap();
}

fn random_bytes(n: usize, seed: u8) -> Vec<u8> {
    let mut x: u32 = 0x12345 ^ seed as u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

async fn wait_for(
    engine: &Arc<Engine>,
    id: &str,
    cond: impl Fn(&tortuga_core::engine::TorrentSummary) -> bool,
) {
    for _ in 0..600 {
        if let Some(t) = engine.state().torrents.into_iter().find(|t| t.id == id)
            && cond(&t)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out; state: {:#?}", engine.state().torrents);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn seed_download_priorities_and_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Content: a folder with three files.
    let content = root.join("seed-content/Show");
    std::fs::create_dir_all(&content).unwrap();
    let files = [
        ("ep1.mkv", random_bytes(3 * 1024 * 1024 + 123, 1)),
        ("ep2.mkv", random_bytes(2 * 1024 * 1024 + 7, 2)),
        ("extras.bin", random_bytes(1024 * 1024, 3)),
    ];
    for (name, data) in &files {
        std::fs::write(content.join(name), data).unwrap();
    }

    write_settings(&root.join("a"), &root.join("a-dl"));
    write_settings(&root.join("b"), &root.join("b-dl"));
    let a = Engine::new(root.join("a")).await.unwrap();
    let b = Engine::new(root.join("b")).await.unwrap();

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
    wait_for(&a, &created.id, |t| {
        matches!(t.state, TorrentState::Seeding)
    })
    .await;
    let a_port = a.state().session.listen_port.expect("seeder port");

    // Resolve shows the file list, as the add dialog would.
    let resolved = b
        .resolve(&AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(resolved.files.len(), 3);
    let extras = resolved
        .files
        .iter()
        .position(|f| f.path.ends_with("extras.bin"))
        .unwrap();
    let ep1 = resolved
        .files
        .iter()
        .position(|f| f.path.ends_with("ep1.mkv"))
        .unwrap();
    let mut priorities = vec![FilePriority::Normal; 3];
    priorities[extras] = FilePriority::Skip;

    let added = b
        .add(AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            priorities: Some(priorities),
            peers: Some(vec![format!("127.0.0.1:{a_port}")]),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(added.id, created.id);

    wait_for(&b, &added.id, |t| t.progress >= 1.0).await;

    let details = b.details(&added.id).unwrap();
    let out_dir = std::path::PathBuf::from(&details.summary.save_path);
    for (name, data) in &files {
        let p = out_dir.join(name);
        if *name == "extras.bin" {
            // Skipped file must not be fully downloaded.
            let got = std::fs::read(&p).unwrap_or_default();
            assert_ne!(&got, data, "skipped file was downloaded");
        } else {
            assert_eq!(&std::fs::read(&p).unwrap(), data, "{name} differs");
        }
    }
    assert_eq!(details.files[extras].priority, FilePriority::Skip);

    // Stream with a Range request through the HTTP API.
    let (addr, _task) =
        tortuga_core::server::serve(b.clone(), "127.0.0.1:0".parse().unwrap(), None)
            .await
            .unwrap();
    let client = reqwest::Client::new();
    let base = format!("http://{addr}");

    let unauth = client
        .get(format!("{base}/api/state"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);

    let play: serde_json::Value = client
        .post(format!("{base}/api/torrents/{}/files/{ep1}/play", added.id))
        .header("x-tortuga-token", &b.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let path = play["path"].as_str().unwrap();
    let resp = client
        .get(format!("{base}{path}"))
        .header("range", "bytes=1000-1999")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 206);
    assert_eq!(
        resp.headers()["content-range"].to_str().unwrap(),
        format!("bytes 1000-1999/{}", files[0].1.len())
    );
    let body = resp.bytes().await.unwrap();
    assert_eq!(&body[..], &files[0].1[1000..2000]);

    // Enabling the skipped file downloads it too.
    let mut changes = HashMap::new();
    changes.insert(extras, FilePriority::High);
    b.set_file_priorities(&added.id, &changes).await.unwrap();
    for _ in 0..300 {
        if std::fs::read(out_dir.join("extras.bin")).ok().as_ref() == Some(&files[2].1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        std::fs::read(out_dir.join("extras.bin")).unwrap(),
        files[2].1
    );

    // Pause / resume / remove.
    b.pause(&added.id).await.unwrap();
    wait_for(&b, &added.id, |t| {
        matches!(t.state, TorrentState::Completed | TorrentState::Paused)
    })
    .await;
    b.resume(&added.id).await.unwrap();
    wait_for(&b, &added.id, |t| matches!(t.state, TorrentState::Seeding)).await;
    b.remove(&added.id, true).await.unwrap();
    assert!(b.state().torrents.is_empty());
}

//! Streaming while the torrent is still downloading: the pieces the player
//! asks for must arrive long before the rest of the file.

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

const MIB: usize = 1024 * 1024;

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
    let mut x: u32 = 0x5bd1 ^ seed as u32;
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

async fn wait(e: &Arc<Engine>, id: &str, cond: impl Fn(&TorrentSummary) -> bool) {
    for _ in 0..300 {
        if get(e, id).as_ref().is_some_and(&cond) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out: {:#?}", get(e, id));
}

/// GET a byte range, returning the body and how long it took.
async fn range(client: &reqwest::Client, url: &str, from: usize, to: usize) -> (Vec<u8>, Duration) {
    let start = Instant::now();
    let fut = async {
        let resp = client
            .get(url)
            .header("range", format!("bytes={from}-{to}"))
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.bytes().await.unwrap().to_vec();
        assert_eq!(
            status,
            206,
            "{}",
            String::from_utf8_lossy(&body[..body.len().min(500)])
        );
        body
    };
    let body = tokio::time::timeout(Duration::from_secs(30), fut)
        .await
        .expect("stream request hung");
    (body, start.elapsed())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stream_while_downloading() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let content = root.join("seed/Movie");
    std::fs::create_dir_all(&content).unwrap();
    let movie = data(40 * MIB + 4321, 1);
    let sample = data(2 * MIB + 99, 2);
    std::fs::write(content.join("movie.mkv"), &movie).unwrap();
    std::fs::write(content.join("sample.mkv"), &sample).unwrap();

    // The seeder uploads at 2 MiB/s, so the whole movie takes ~20 s.
    settings(&root.join("a"), &root.join("a-dl"), 2048);
    settings(&root.join("b"), &root.join("b-dl"), 0);
    let a = Engine::new(root.join("a")).await.unwrap();
    let b = Engine::new(root.join("b")).await.unwrap();

    let created = tortuga_core::create::create(
        &a,
        tortuga_core::create::CreateRequest {
            path: content.to_string_lossy().into_owned(),
            piece_length: Some(256 * 1024),
            seed: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    wait(&a, &created.id, |t| {
        matches!(t.state, TorrentState::Seeding)
    })
    .await;
    let peer = format!("127.0.0.1:{}", a.state().session.listen_port.unwrap());

    let resolved = b
        .resolve(&AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let idx = |name: &str| {
        resolved
            .files
            .iter()
            .position(|f| f.path.ends_with(name))
            .unwrap()
    };
    let (movie_idx, sample_idx) = (idx("movie.mkv"), idx("sample.mkv"));
    let mut priorities = vec![FilePriority::Normal; 2];
    priorities[sample_idx] = FilePriority::Skip;

    let id = b
        .add(AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            priorities: Some(priorities),
            peers: Some(vec![peer]),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;

    let (addr, _task) =
        tortuga_core::server::serve(b.clone(), "127.0.0.1:0".parse().unwrap(), None)
            .await
            .unwrap();
    let base = format!("http://{addr}");
    let client = reqwest::Client::new();

    let play: serde_json::Value = client
        .post(format!("{base}/api/torrents/{id}/files/{movie_idx}/play"))
        .header("x-tortuga-token", &b.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = format!("{base}{}", play["path"].as_str().unwrap());

    // Start of the file, as a player opens it.
    let (body, took) = range(&client, &url, 0, MIB - 1).await;
    assert_eq!(body, movie[..MIB]);
    eprintln!("first 1 MiB in {took:?}");
    assert!(took < Duration::from_secs(5), "start took {took:?}");

    // Seek into the middle: these pieces jump the queue.
    let at = 30 * MIB;
    let (body, took) = range(&client, &url, at, at + MIB - 1).await;
    assert_eq!(body, movie[at..at + MIB]);
    let progress = get(&b, &id).unwrap().progress;
    eprintln!(
        "seek to 30 MiB in {took:?}, torrent at {:.0}%",
        progress * 100.0
    );
    assert!(took < Duration::from_secs(5), "seek took {took:?}");
    assert!(
        progress < 0.7,
        "seek only finished after most of the download ({progress})"
    );

    // Players read the tail for the index (mp4 moov / mkv cues).
    let tail = movie.len() - 64 * 1024;
    let (body, took) = range(&client, &url, tail, movie.len() - 1).await;
    assert_eq!(body, movie[tail..]);
    eprintln!("tail in {took:?}");
    assert!(took < Duration::from_secs(5), "tail took {took:?}");

    // A copied link to a skipped file of a paused torrent still plays.
    b.pause(&id).await.unwrap();
    wait(&b, &id, |t| matches!(t.state, TorrentState::Paused)).await;
    let direct = format!(
        "{base}/stream/{id}/{sample_idx}/sample.mkv?token={}",
        b.token
    );
    let (body, took) = range(&client, &direct, MIB, MIB + 64 * 1024 - 1).await;
    assert_eq!(body, sample[MIB..MIB + 64 * 1024]);
    eprintln!("skipped file of a paused torrent in {took:?}");

    b.shutdown().await;
    a.shutdown().await;
}

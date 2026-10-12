//! End-to-end checks of the features users rely on, especially the edge
//! cases: duplicates, restarts, bad input, settings changes mid-download.

mod common;

use std::{collections::HashMap, time::Duration};

use common::*;
use tortuga_core::{
    engine::{AddRequest, TorrentState},
    meta::FilePriority,
};

/// Magnet with a peer hint: metadata, auto in-order for series, duplicates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn magnet_download() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let show = root.join("src/Show S01");
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("Show.S01E01.mkv", data(MIB + 17, 1)),
        ("Show.S01E02.mkv", data(MIB + 5, 2)),
        ("Show.S01E03.mkv", data(MIB, 3)),
        ("info.txt", data(300, 4)),
    ];
    write_files(&show, &files);
    let a = engine(root, "a", 0).await;
    let b = engine(root, "b", 0).await;
    let (created, peer) = seed(&a, &show, 64 * 1024).await;

    let req = AddRequest {
        source: Some(created.magnet.clone()),
        peers: Some(vec![peer.clone()]),
        ..Default::default()
    };
    let first = b.add(req.clone()).await.unwrap();
    assert_eq!(first.id, created.id);
    assert!(!first.already_added);
    // Adding the same magnet again (double click, RSS + manual) is harmless.
    let second = b.add(req.clone()).await.unwrap();
    assert_eq!(second.id, created.id);
    assert!(second.already_added);
    assert_eq!(
        b.state()
            .torrents
            .iter()
            .filter(|t| t.id == created.id)
            .count(),
        1
    );

    let t = wait(&b, &created.id, 60, "magnet download", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    assert!(
        t.sequential,
        "series should download in order automatically"
    );
    let out = std::path::PathBuf::from(&t.save_path);
    for (name, bytes) in &files {
        assert_eq!(&std::fs::read(out.join(name)).unwrap(), bytes, "{name}");
    }
    // Once added, the magnet is a regular torrent; no metadata row is left.
    assert!(
        !b.state()
            .torrents
            .iter()
            .any(|t| t.state == TorrentState::Metadata)
    );
    assert!(b.add(req).await.unwrap().already_added);

    b.shutdown().await;
    a.shutdown().await;
}

/// A magnet whose metadata hasn't arrived yet must survive a restart and be
/// removable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_magnet_survives_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let hash = "0123456789abcdef0123456789abcdef01234567";
    let magnet = format!("magnet:?xt=urn:btih:{hash}&dn=Lost%20Show");

    let b = engine(root, "b", 0).await;
    let r = b
        .add(AddRequest {
            source: Some(magnet.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(r.id, hash);
    let t = get(&b, hash).unwrap();
    assert!(matches!(
        t.state,
        TorrentState::Metadata | TorrentState::Error
    ));
    assert_eq!(t.name, "Lost Show");
    // Pasting the bare hash is the same torrent, not a second row.
    let again = b
        .add(AddRequest {
            source: Some(hash.to_uppercase()),
            ..Default::default()
        })
        .await;
    let again = again.unwrap();
    assert!(again.already_added, "{again:?}");
    b.shutdown().await;
    drop(b);

    let b = engine(root, "b", 0).await;
    let t = get(&b, hash).expect("pending magnet lost on restart");
    // Without DHT (off in tests) it can't look for peers and shows why.
    assert!(
        matches!(t.state, TorrentState::Metadata | TorrentState::Error),
        "{t:?}"
    );
    assert_eq!(t.name, "Lost Show");

    b.remove(hash, false).await.unwrap();
    assert!(get(&b, hash).is_none());
    b.shutdown().await;
    drop(b);

    let b = engine(root, "b", 0).await;
    assert!(get(&b, hash).is_none(), "removed magnet came back");
    b.shutdown().await;
}

/// Bad input gets a clear 4xx, never a hang or a 500.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn api_rejects_bad_input() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let content = root.join("src/one.bin");
    write_files(&root.join("src"), &[("one.bin", data(300_000, 9))]);
    let a = engine(root, "a", 0).await;
    let (created, _) = seed(&a, &content, 64 * 1024).await;
    let base = serve(&a).await;
    let c = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let tok = a.token.clone();
    let id = created.id.clone();

    let status = |r: reqwest::Response| r.status().as_u16();
    let post = |path: &str, body: serde_json::Value| {
        c.post(format!("{base}{path}"))
            .header("x-tortuga-token", &tok)
            .json(&body)
            .send()
    };
    let get_ = |path: &str| {
        c.get(format!("{base}{path}"))
            .header("x-tortuga-token", &tok)
            .send()
    };

    // Auth.
    assert_eq!(
        status(c.get(format!("{base}/api/state")).send().await.unwrap()),
        401
    );
    assert_eq!(
        status(
            c.get(format!("{base}/api/state?token=wrong"))
                .send()
                .await
                .unwrap()
        ),
        401
    );
    assert_eq!(
        status(
            c.get(format!("{base}/stream/{id}/0/one.bin"))
                .send()
                .await
                .unwrap()
        ),
        401
    );

    // Unknown / malformed torrents.
    let unknown = "f".repeat(40);
    assert_eq!(
        status(get_(&format!("/api/torrents/{unknown}")).await.unwrap()),
        404
    );
    let s = status(get_("/api/torrents/not-a-hash").await.unwrap());
    assert!((400..500).contains(&s), "malformed id gave {s}");
    let s = status(
        post("/api/torrents", serde_json::json!({"source": "hello"}))
            .await
            .unwrap(),
    );
    assert_eq!(s, 400);
    let s = status(post("/api/torrents", serde_json::json!({})).await.unwrap());
    assert_eq!(s, 400);
    let s = status(
        post(
            "/api/torrents",
            serde_json::json!({"torrent_base64": "!!!"}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(s, 400);
    let s = status(
        post(
            "/api/torrents",
            serde_json::json!({"torrent_base64": "aGVsbG8="}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(s, 400, "garbage .torrent");

    // Files that don't exist.
    let s = status(
        post(
            &format!("/api/torrents/{id}/files/7/play"),
            serde_json::json!({}),
        )
        .await
        .unwrap(),
    );
    assert!((400..500).contains(&s), "bad file index gave {s}");
    let s = status(
        get_(&format!("/stream/{id}/7/x.bin?token={tok}"))
            .await
            .unwrap(),
    );
    assert!((400..500).contains(&s), "bad stream index gave {s}");
    let s = status(
        post(
            &format!("/api/torrents/{id}/files"),
            serde_json::json!({"priorities": {"99": "skip"}}),
        )
        .await
        .unwrap(),
    );
    assert!(s < 500, "out of range priority gave {s}");

    // Ranges.
    let url = format!("{base}/stream/{id}/0/one.bin?token={tok}");
    let r = c
        .get(&url)
        .header("range", "bytes=999999-")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 416);
    let r = c
        .get(&url)
        .header("range", "bytes=-100")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.bytes().await.unwrap().len(), 100);
    let r = c.get(&url).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap().len(), 300_000);

    // Settings validation.
    let mut settings: serde_json::Value =
        get_("/api/settings").await.unwrap().json().await.unwrap();
    settings["scheduler"]["grid"] = serde_json::json!([[]]);
    let s = status(
        c.put(format!("{base}/api/settings"))
            .header("x-tortuga-token", &tok)
            .json(&settings)
            .send()
            .await
            .unwrap(),
    );
    assert_eq!(s, 400);

    a.shutdown().await;
}

/// Changing connection settings restarts the BitTorrent session; downloads
/// must carry on with their priorities intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_change_mid_download() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = root.join("src/Pack");
    write_files(
        &dir,
        &[("big.bin", data(8 * MIB, 1)), ("skip.bin", data(MIB, 2))],
    );
    let a = engine(root, "a", 2048).await;
    let b = engine(root, "b", 0).await;
    let (created, peer) = seed(&a, &dir, 256 * 1024).await;

    let resolved = b
        .resolve(&AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let skip = resolved
        .files
        .iter()
        .position(|f| f.path.ends_with("skip.bin"))
        .unwrap();
    let mut pr = vec![FilePriority::Normal; 2];
    pr[skip] = FilePriority::Skip;
    let id = b
        .add(AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            priorities: Some(pr),
            peers: Some(vec![peer.clone()]),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    wait(&b, &id, 30, "some progress", |t| {
        t.state != TorrentState::Checking && t.done > MIB as u64
    })
    .await;

    let mut s = b.settings();
    let old_port = s.listen_port;
    s.listen_port = old_port + 1;
    s.download_limit = 0;
    s.peer_limit = 50;
    let s = b.update_settings(s).await.unwrap();
    assert_eq!(s.listen_port, old_port + 1);
    assert_eq!(b.state().session.listen_port, Some(old_port + 1));
    reconnect(&b, &id, &peer).await;

    let t = wait(&b, &id, 60, "finish after restart", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    let d = b.details(&id).unwrap();
    assert_eq!(d.files[skip].priority, FilePriority::Skip);
    assert!(d.files[skip].done < MIB as u64, "skipped file downloaded");
    assert!(matches!(
        t.state,
        TorrentState::Seeding | TorrentState::Completed
    ));

    // Turtle mode limits apply immediately.
    b.set_alt_speed(true).unwrap();
    let st = b.state().session;
    assert!(st.alt_speed_active);
    assert_eq!(st.download_limit, b.settings().alt_download_limit);
    b.set_alt_speed(false).unwrap();
    assert!(!b.state().session.alt_speed_active);

    b.shutdown().await;
    a.shutdown().await;
}

/// In-order mode: earlier episodes finish before later ones start.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_order_episodes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = root.join("src/Series");
    // Out-of-order names on disk; natural order is 2 < 10.
    write_files(
        &dir,
        &[
            ("Ep 10.mkv", data(2 * MIB, 10)),
            ("Ep 2.mkv", data(2 * MIB, 2)),
            ("Ep 1.mkv", data(2 * MIB, 1)),
            ("Ep 3.mkv", data(2 * MIB, 3)),
        ],
    );
    let a = engine(root, "a", 1536).await;
    let b = engine(root, "b", 0).await;
    let (created, peer) = seed(&a, &dir, 128 * 1024).await;
    let id = b
        .add(AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            peers: Some(vec![peer]),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    assert!(get(&b, &id).unwrap().sequential, "series not detected");

    let d = wait_details(&b, &id, 60, "Ep 1 done", |d| {
        let f = &d.files[file_index(d, "Ep 1.mkv")];
        f.done == f.length
    })
    .await;
    let done = |name: &str| d.files[file_index(&d, name)].done;
    assert!(
        done("Ep 3.mkv") < MIB as u64 / 2,
        "Ep 3 too early: {}",
        done("Ep 3.mkv")
    );
    assert!(
        done("Ep 10.mkv") < MIB as u64 / 2,
        "Ep 10 too early: {}",
        done("Ep 10.mkv")
    );

    let d = wait_details(&b, &id, 60, "Ep 3 done", |d| {
        let f = &d.files[file_index(d, "Ep 3.mkv")];
        f.done == f.length
    })
    .await;
    let f2 = &d.files[file_index(&d, "Ep 2.mkv")];
    assert_eq!(f2.done, f2.length, "Ep 3 finished before Ep 2");

    b.shutdown().await;
    a.shutdown().await;
}

/// Selected files first: marking Ep 3 + Ep 4 "in order" downloads them before the rest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selected_files_first() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = root.join("src/S");
    write_files(
        &dir,
        &[
            ("E1.mkv", data(2 * MIB, 1)),
            ("E2.mkv", data(2 * MIB, 2)),
            ("E3.mkv", data(2 * MIB, 3)),
            ("E4.mkv", data(2 * MIB, 4)),
        ],
    );
    let a = engine(root, "a", 1536).await;
    let b = engine(root, "b", 0).await;
    let (created, peer) = seed(&a, &dir, 128 * 1024).await;
    let id = b
        .add(AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            peers: Some(vec![peer]),
            paused: Some(true),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let d = wait_details(&b, &id, 30, "details", |_| true).await;
    let (e3, e4) = (file_index(&d, "E3.mkv"), file_index(&d, "E4.mkv"));
    b.download_in_order(&id, &[e4, e3]).await.unwrap();
    b.resume(&id).await.unwrap();
    let d = wait_details(&b, &id, 60, "E3+E4 done", |d| {
        [e3, e4]
            .iter()
            .all(|i| d.files[*i].done == d.files[*i].length)
    })
    .await;
    let rest: u64 = d
        .files
        .iter()
        .filter(|f| f.index != e3 && f.index != e4)
        .map(|f| f.done)
        .sum();
    assert!(rest < MIB as u64, "other files got {rest} bytes first");
    assert_eq!(d.files[e3].priority, FilePriority::High);

    b.shutdown().await;
    a.shutdown().await;
}

/// Pause all / resume all, remove with files, re-adding existing data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_remove_readd() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Non-ASCII and spaces in names, plus an empty file.
    let dir = root.join("src/Фильм (2024)");
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("Фильм 1080p.mkv", data(3 * MIB + 3, 1)),
        ("Субтитры/рус.srt", data(4000, 2)),
        ("empty.nfo", Vec::new()),
    ];
    write_files(&dir, &files);
    let a = engine(root, "a", 0).await;
    let b = engine(root, "b", 0).await;
    let (created, peer) = seed(&a, &dir, 64 * 1024).await;
    let add = AddRequest {
        torrent_base64: Some(created.torrent_base64.clone()),
        peers: Some(vec![peer.clone()]),
        ..Default::default()
    };
    let id = b.add(add.clone()).await.unwrap().id;
    let t = wait(&b, &id, 60, "download", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    let out = std::path::PathBuf::from(&t.save_path);
    for (name, bytes) in &files {
        assert_eq!(&std::fs::read(out.join(name)).unwrap(), bytes, "{name}");
    }

    // Stream a file with Cyrillic and spaces through the URL the UI builds.
    let base = serve(&b).await;
    let d = b.details(&id).unwrap();
    let movie = file_index(&d, "1080p.mkv");
    let c = reqwest::Client::new();
    let play: serde_json::Value = c
        .post(format!("{base}/api/torrents/{id}/files/{movie}/play"))
        .header("x-tortuga-token", &b.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = c
        .get(format!("{base}{}", play["path"].as_str().unwrap()))
        .header("range", "bytes=0-9")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(&r.bytes().await.unwrap()[..], &files[0].1[..10]);

    b.pause_all().await.unwrap();
    wait(&b, &id, 10, "paused", |t| {
        matches!(t.state, TorrentState::Paused | TorrentState::Completed)
    })
    .await;
    assert!(get(&b, &id).unwrap().user_paused);
    b.resume_all().await.unwrap();
    wait(&b, &id, 10, "running", |t| t.state == TorrentState::Seeding).await;

    // Remove keeping files, add again: existing data is reused, not downloaded.
    b.remove(&id, false).await.unwrap();
    assert!(get(&b, &id).is_none());
    a.pause(&created.id).await.unwrap(); // no seeder: data must come from disk
    let id = b.add(add.clone()).await.unwrap().id;
    wait(&b, &id, 30, "recheck existing data", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;

    // Remove with files: they're gone and the torrent can be added again.
    b.remove(&id, true).await.unwrap();
    assert!(!out.join(files[0].0).exists(), "files not deleted");
    a.resume(&created.id).await.unwrap();
    wait(&a, &created.id, 30, "seeder back", |t| {
        t.state == TorrentState::Seeding
    })
    .await;
    let id = b.add(add).await.unwrap().id;
    reconnect(&b, &id, &peer).await;
    wait(&b, &id, 60, "download again", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;

    b.shutdown().await;
    a.shutdown().await;
}

/// Priorities across a restart, and the all-skipped case.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn priorities_persist() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = root.join("src/P");
    write_files(&dir, &[("a.bin", data(MIB, 1)), ("b.bin", data(MIB, 2))]);
    let a = engine(root, "a", 0).await;
    let (created, peer) = seed(&a, &dir, 64 * 1024).await;

    let b = engine(root, "b", 0).await;
    let id = b
        .add(AddRequest {
            torrent_base64: Some(created.torrent_base64.clone()),
            paused: Some(true),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let d = wait_details(&b, &id, 30, "details", |_| true).await;
    let (fa, fb) = (file_index(&d, "a.bin"), file_index(&d, "b.bin"));
    b.set_file_priorities(
        &id,
        &HashMap::from([(fa, FilePriority::Low), (fb, FilePriority::Skip)]),
    )
    .await
    .unwrap();
    b.shutdown().await;
    drop(b);

    let b = engine(root, "b", 0).await;
    let d = wait_details(&b, &id, 30, "details after restart", |_| true).await;
    assert_eq!(d.files[fa].priority, FilePriority::Low);
    assert_eq!(d.files[fb].priority, FilePriority::Skip);
    assert!(get(&b, &id).unwrap().user_paused, "pause lost on restart");

    // Skipping every file stops the torrent instead of erroring; Start
    // brings everything back.
    b.set_file_priorities(&id, &HashMap::from([(fa, FilePriority::Skip)]))
        .await
        .unwrap();
    b.resume(&id).await.unwrap();
    reconnect(&b, &id, &peer).await;
    let t = wait(&b, &id, 60, "full download", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    assert!(!t.user_paused);
    let d = b.details(&id).unwrap();
    assert!(d.files.iter().all(|f| f.priority == FilePriority::Normal));

    b.shutdown().await;
    a.shutdown().await;
}

/// Creating torrents: single file, folder, and errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_torrents() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let a = engine(root, "a", 0).await;
    write_files(&root.join("src"), &[("single.iso", data(700_000, 1))]);
    std::fs::create_dir_all(root.join("src/empty")).unwrap();

    let out = root.join("out/single.torrent");
    let r = tortuga_core::create::create(
        &a,
        tortuga_core::create::CreateRequest {
            path: root.join("src/single.iso").to_string_lossy().into_owned(),
            trackers: vec!["udp://tracker.example:80/announce".into()],
            save_to: Some(out.to_string_lossy().into_owned()),
            seed: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r.name, "single.iso");
    assert!(r.magnet.contains(&r.id));
    assert!(std::fs::metadata(&out).unwrap().len() > 0);

    for bad in ["src/empty", "src/missing"] {
        let res = tortuga_core::create::create(
            &a,
            tortuga_core::create::CreateRequest {
                path: root.join(bad).to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .await;
        assert!(res.is_err(), "{bad} should fail");
    }
    a.shutdown().await;
}

/// The weekly scheduler: "off" hours pause everything, "turtle" hours apply
/// the alternative limits, and both undo cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduler_slots() {
    use tortuga_core::settings::ScheduleSlot;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_files(&root.join("src"), &[("f.bin", data(200_000, 1))]);
    let a = engine(root, "a", 0).await;
    let (created, _) = seed(&a, &root.join("src/f.bin"), 64 * 1024).await;
    let id = created.id;

    let set_all = |slot: ScheduleSlot, enabled: bool| {
        let mut s = a.settings();
        s.scheduler.enabled = enabled;
        s.scheduler.grid = vec![vec![slot; 24]; 7];
        s
    };

    a.update_settings(set_all(ScheduleSlot::Off, true))
        .await
        .unwrap();
    // A finished torrent that is stopped shows as completed rather than queued.
    let t = wait(&a, &id, 10, "stopped by scheduler", |t| {
        matches!(t.state, TorrentState::Queued | TorrentState::Completed)
    })
    .await;
    assert!(!t.user_paused, "scheduler must not look like a user pause");

    a.update_settings(set_all(ScheduleSlot::Alt, true))
        .await
        .unwrap();
    wait(&a, &id, 10, "running again", |t| {
        t.state == TorrentState::Seeding
    })
    .await;
    assert!(a.state().session.alt_speed_active);

    a.update_settings(set_all(ScheduleSlot::Off, false))
        .await
        .unwrap();
    wait(&a, &id, 10, "disabled scheduler", |t| {
        t.state == TorrentState::Seeding
    })
    .await;
    assert!(!a.state().session.alt_speed_active);

    a.shutdown().await;
}

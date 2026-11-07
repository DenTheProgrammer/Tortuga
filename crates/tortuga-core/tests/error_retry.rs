//! Start retries failed torrents in place after storage becomes available.

mod common;

use common::*;
use std::time::Duration;
use tortuga_core::{
    engine::{AddRequest, TorrentState},
    meta::FilePriority,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_retries_storage_error_and_keeps_existing_data() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bytes = data(MIB, 51);
    write_files(&root.join("src"), &[("film.bin", bytes.clone())]);
    let a = engine(root, "a", 0).await;
    let (created, peer) = seed(&a, &root.join("src/film.bin"), 64 * 1024).await;
    let b = engine(root, "b", 0).await;

    // Keep real, valid partial data. Prevent writing resume data to reproduce
    // an asynchronous storage error without filling the user's disk.
    write_files(
        &root.join("b-dl"),
        &[("film.bin", bytes[..MIB / 2].to_vec())],
    );
    let blocked = root.join(format!("b/session/{}.bitv.tmp", created.id));
    std::fs::create_dir(&blocked).unwrap();
    let id = b
        .add(AddRequest {
            torrent_base64: Some(created.torrent_base64),
            category: Some("Films".into()),
            priorities: Some(vec![FilePriority::High]),
            sequential: Some(true),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    wait(&b, &id, 10, "storage error", |t| {
        t.state == TorrentState::Error
    })
    .await;
    let before = get(&b, &id).unwrap();

    // Start while storage is still blocked must leave a retryable error.
    b.resume(&id).await.unwrap();
    wait(&b, &id, 10, "repeated storage error", |t| {
        t.state == TorrentState::Error
    })
    .await;
    std::fs::remove_dir(&blocked).unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        get(&b, &id).unwrap().state,
        TorrentState::Error,
        "must wait for Start"
    );

    // Exercise the same endpoint as the Start button.
    let base = serve(&b).await;
    let response = reqwest::Client::new()
        .post(format!("{base}/api/torrents/{id}/resume"))
        .header("x-tortuga-token", &b.token)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let resumed = wait(&b, &id, 10, "existing pieces after retry", |t| {
        matches!(t.state, TorrentState::Downloading | TorrentState::Stalled) && t.done > 0
    })
    .await;
    assert_eq!(resumed.done, (MIB / 2) as u64);
    assert_eq!(resumed.added_at, before.added_at);
    assert_eq!(resumed.save_path, before.save_path);
    assert_eq!(resumed.category.as_deref(), Some("Films"));
    assert!(resumed.sequential);
    assert!(resumed.error.is_none());
    assert_eq!(
        b.details(&id).unwrap().files[0].priority,
        FilePriority::High
    );
    reconnect(&b, &id, &peer).await;
    wait(&b, &id, 20, "download after retry", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    assert_eq!(std::fs::read(b.file_path(&id, 0).unwrap()).unwrap(), bytes);
    assert_eq!(b.state().torrents.len(), 1);
    b.shutdown().await;
    a.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_retries_failed_magnet_add_with_saved_options() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bytes = data(MIB, 52);
    write_files(&root.join("src"), &[("film.bin", bytes.clone())]);
    let a = engine(root, "a", 0).await;
    let (created, peer) = seed(&a, &root.join("src/film.bin"), 64 * 1024).await;
    let b = engine(root, "b", 0).await;

    // Metadata resolves, but the destination cannot be opened yet.
    let destination = root.join("destination");
    std::fs::write(&destination, b"blocks directory creation").unwrap();
    let id = b
        .add(AddRequest {
            source: Some(created.magnet),
            peers: Some(vec![peer]),
            save_path: Some(destination.to_string_lossy().into_owned()),
            paused: Some(true),
            category: Some("Films".into()),
            priorities: Some(vec![FilePriority::High]),
            sequential: Some(true),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    let before = wait(&b, &id, 10, "failed magnet add", |t| {
        t.state == TorrentState::Error
    })
    .await;
    assert_eq!(before.size, 0, "should still be a pending torrent");
    b.resume(&id).await.unwrap();
    wait(&b, &id, 10, "failed magnet retry", |t| {
        t.state == TorrentState::Error
    })
    .await;

    std::fs::remove_file(&destination).unwrap();
    std::fs::create_dir(&destination).unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        get(&b, &id).unwrap().state,
        TorrentState::Error,
        "must wait for Start"
    );
    // The tray's Start all must retry failed pending requests too.
    b.resume_all().await.unwrap();
    let resumed = wait(&b, &id, 20, "magnet download after retry", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    assert!(!resumed.user_paused);
    assert_eq!(resumed.category.as_deref(), Some("Films"));
    assert!(resumed.sequential);
    assert!(resumed.error.is_none());
    assert_eq!(
        b.details(&id).unwrap().files[0].priority,
        FilePriority::High
    );
    assert_eq!(std::fs::read(destination.join("film.bin")).unwrap(), bytes);
    assert_eq!(b.state().torrents.len(), 1);
    b.shutdown().await;
    a.shutdown().await;
}

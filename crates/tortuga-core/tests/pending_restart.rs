//! A network restart must move magnet metadata lookups to the new session.

mod common;

use std::{net::SocketAddr, time::Duration};

use common::*;
use tortuga_core::engine::{AddRequest, TorrentState};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_metadata_restarts_with_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let bytes = data(MIB, 23);
    write_files(&root.join("src/Film"), &[("film.mkv", bytes.clone())]);
    let a = engine(root, "a", 0).await;
    let (created, peer) = seed(&a, &root.join("src/Film"), 64 * 1024).await;
    let peer: SocketAddr = peer.parse().unwrap();

    // The first connection waits forever. Only a new metadata lookup can
    // reach the seeder through this proxy after the session restarts.
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = proxy.local_addr().unwrap().to_string();
    let (accepted, first) = tokio::sync::oneshot::channel();
    let proxy_task = tokio::spawn(async move {
        let mut accepted = Some(accepted);
        while let Ok((mut incoming, _)) = proxy.accept().await {
            if let Some(notify) = accepted.take() {
                let _ = notify.send(());
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    drop(incoming);
                });
            } else {
                tokio::spawn(async move {
                    let mut outgoing = tokio::net::TcpStream::connect(peer).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                });
            }
        }
    });

    let b = engine(root, "b", 0).await;
    let id = b
        .add(AddRequest {
            source: Some(created.magnet),
            peers: Some(vec![address]),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(get(&b, &id).unwrap().state, TorrentState::Metadata);

    let unused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut settings = b.settings();
    settings.listen_port = unused.local_addr().unwrap().port();
    drop(unused);
    b.update_settings(settings).await.unwrap();
    wait(&b, &id, 10, "metadata after network restart", |t| {
        t.state != TorrentState::Metadata
    })
    .await;
    wait(&b, &id, 20, "download after network restart", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;
    assert_eq!(std::fs::read(b.file_path(&id, 0).unwrap()).unwrap(), bytes);
    proxy_task.abort();
    b.shutdown().await;
    a.shutdown().await;
}

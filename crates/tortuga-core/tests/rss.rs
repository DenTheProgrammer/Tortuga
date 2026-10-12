//! RSS auto-download end to end: a local feed server, rules, de-duplication.

mod common;

use std::sync::Arc;

use axum::{Router, routing::get};
use common::*;
use tortuga_core::{
    engine::TorrentState,
    rss::{self, Feed, Rule},
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rss_rules_download_matching_items_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_files(&root.join("src/E01"), &[("Show.S01E01.mkv", data(MIB, 1))]);
    write_files(&root.join("src/E02"), &[("Show.S01E02.mkv", data(MIB, 2))]);
    let a = engine(root, "a", 0).await;
    let b = engine(root, "b", 0).await;
    let (e01, peer) = seed(&a, &root.join("src/E01/Show.S01E01.mkv"), 64 * 1024).await;
    let (e02, _) = seed(&a, &root.join("src/E02/Show.S01E02.mkv"), 64 * 1024).await;

    // The feed links E01 by magnet (with a peer hint) and E02 by .torrent URL.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let magnet = format!("{}&x.pe={peer}", e01.magnet).replace('&', "&amp;");
    let feed_xml = Arc::new(format!(
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Tracker</title>
        <item><title>Show S01E01 1080p WEB</title><guid>e01-1080</guid><link>{magnet}</link></item>
        <item><title>Show S01E01 720p WEB</title><guid>e01-720</guid><link>{magnet}</link></item>
        <item><title>Other Thing 1080p</title><guid>other</guid><link>{magnet}</link></item>
        <item><title>Show S01E02 1080p WEB</title><guid>e02-1080</guid>
          <enclosure url="{base}/e02.torrent" type="application/x-bittorrent" length="1"/></item>
        </channel></rss>"#
    ));
    let e02_bytes = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(&e02.torrent_base64)
            .unwrap()
    };
    let app = Router::new()
        .route(
            "/feed.xml",
            get({
                let x = feed_xml.clone();
                move || async move { x.to_string() }
            }),
        )
        .route(
            "/e02.torrent",
            get(move || async move { e02_bytes.clone() }),
        )
        .route("/broken.xml", get(|| async { "<html>not a feed" }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let store = rss::store(&b);
    let feed = store
        .upsert_feed(Feed {
            url: format!("{base}/feed.xml"),
            ..Default::default()
        })
        .unwrap();
    store
        .upsert_rule(Rule {
            name: "Show".into(),
            must_contain: "show 1080p".into(),
            must_not_contain: "720p".into(),
            ..Default::default()
        })
        .unwrap();

    rss::refresh_feed(&b, &feed.id).await.unwrap();
    // .torrent links are fetched in the background.
    wait(&b, &e02.id, 30, "E02 via RSS .torrent link", |_| true).await;
    let ids: Vec<String> = b.state().torrents.iter().map(|t| t.id.clone()).collect();
    assert!(ids.contains(&e01.id), "E01 not added: {ids:?}");
    assert_eq!(ids.len(), 2, "unexpected extra torrents: {ids:?}");

    wait(&b, &e01.id, 60, "E01 via RSS magnet", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;

    // A second refresh doesn't add anything again.
    rss::refresh_feed(&b, &feed.id).await.unwrap();
    assert_eq!(b.state().torrents.len(), 2);
    let snap = store.snapshot();
    assert_eq!(snap.items.len(), 4);
    assert_eq!(snap.items.iter().filter(|i| i.downloaded).count(), 2);
    assert!(snap.feeds[0].last_error.is_none());

    // A broken feed reports an error instead of failing silently.
    let broken = store
        .upsert_feed(Feed {
            url: format!("{base}/broken.xml"),
            ..Default::default()
        })
        .unwrap();
    assert!(rss::refresh_feed(&b, &broken.id).await.is_err());
    let snap = store.snapshot();
    let f = snap.feeds.iter().find(|f| f.id == broken.id).unwrap();
    assert!(f.last_error.is_some());
    // Bad input is rejected up front.
    assert!(
        store
            .upsert_feed(Feed {
                url: "not a url".into(),
                ..Default::default()
            })
            .is_err()
    );
    assert!(
        store
            .upsert_rule(Rule {
                must_contain: "([".into(),
                use_regex: true,
                ..Default::default()
            })
            .is_err()
    );

    b.shutdown().await;
    a.shutdown().await;
}

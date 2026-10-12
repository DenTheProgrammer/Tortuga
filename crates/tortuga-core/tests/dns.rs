//! A filtering DNS (AdGuard, Pi-hole…) that blocks tracker names must not stop
//! downloads: names are resolved over DNS-over-HTTPS instead.

mod common;

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

use axum::{Router, extract::Query, routing::get};
use common::*;
use tortuga_core::engine::{AddRequest, TorrentState};

/// The tracker's name: `.invalid` never resolves through real DNS, just like a
/// name blocked by a filter.
const TRACKER_HOST: &str = "tracker.tortuga.invalid";

/// A non-loopback address of this machine (loopback answers are treated as a
/// filter's sink address and ignored).
fn lan_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_tracker_name_resolves_over_https() {
    let Some(lan) = lan_ip() else {
        eprintln!("no non-loopback IPv4 address; skipping");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_files(&root.join("src/Film"), &[("film.mkv", data(3 * MIB, 7))]);
    let a = engine(root, "a", 0).await;
    let (created, peer) = seed(&a, &root.join("src/Film"), 64 * 1024).await;
    let seed_port: u16 = peer.rsplit(':').next().unwrap().parse().unwrap();

    // Mock DoH: answers only for the tracker name, NXDOMAIN otherwise.
    let doh = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let doh_url = format!("http://{}/dns-query", doh.local_addr().unwrap());
    let doh_app = Router::new().route(
        "/dns-query",
        get(
            move |Query(q): Query<std::collections::HashMap<String, String>>| async move {
                let ok = q.get("name").map(String::as_str) == Some(TRACKER_HOST)
                    && q.get("type").map(String::as_str) == Some("A");
                if ok {
                    format!(r#"{{"Status":0,"Answer":[{{"type":1,"data":"{lan}"}}]}}"#)
                } else {
                    r#"{"Status":3}"#.to_string()
                }
            },
        ),
    );
    tokio::spawn(async move { axum::serve(doh, doh_app).await.unwrap() });
    tortuga_core::dns::set_doh_endpoints(vec![doh_url]);

    // Minimal HTTP tracker handing out the seeder (compact peer list).
    let tracker = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let tracker_port = tracker.local_addr().unwrap().port();
    let tracker_app = Router::new().route(
        "/announce",
        get(move || async move {
            let mut body = b"d8:intervali60e5:peers6:".to_vec();
            body.extend_from_slice(&[127, 0, 0, 1]);
            body.extend_from_slice(&seed_port.to_be_bytes());
            body.push(b'e');
            body
        }),
    );
    tokio::spawn(async move { axum::serve(tracker, tracker_app).await.unwrap() });

    // The direct lookup goes through the fallback.
    let addrs = tortuga_core::dns::resolve(TRACKER_HOST, 80).await.unwrap();
    assert_eq!(addrs, vec![std::net::SocketAddr::from((lan, 80))]);
    // Names the system resolves never touch DoH.
    assert!(tortuga_core::dns::resolve("localhost", 80).await.is_ok());

    // A magnet whose only way to find peers is that tracker.
    let b = engine(root, "b", 0).await;
    let tracker_url = format!("http://{TRACKER_HOST}:{tracker_port}/announce");
    let magnet = format!(
        "{}&tr={}",
        created.magnet.split("&tr=").next().unwrap(),
        url::form_urlencoded::byte_serialize(tracker_url.as_bytes()).collect::<String>()
    );
    let id = b
        .add(AddRequest {
            source: Some(magnet),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    wait(&b, &id, 60, "download via a DNS-blocked tracker", |t| {
        matches!(t.state, TorrentState::Seeding | TorrentState::Completed)
    })
    .await;

    b.shutdown().await;
    a.shutdown().await;
}

//! A stale DHT node must not kill the socket receiving replies from all nodes.

#![cfg(windows)]

use std::{net::Ipv4Addr, time::Duration};

use librqbit_dualstack_sockets::{BindOpts, UdpSocket};
use tokio::time::{sleep, timeout};

#[tokio::test]
async fn udp_survives_unreachable_destination() {
    for bind_addr in ["0.0.0.0:0", "[::]:0"] {
        let socket = UdpSocket::bind_udp(bind_addr.parse().unwrap(), BindOpts::default()).unwrap();
        let local = (Ipv4Addr::LOCALHOST, socket.bind_addr().port());
        let closed = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let unreachable = closed.local_addr().unwrap();
        drop(closed);

        // Receiving the ICMP error before the valid reply used to fail with
        // WSAECONNRESET (10054), causing librqbit's DHT framer to terminate.
        socket.send_to(b"probe", unreachable).await.unwrap();
        let sender = tokio::spawn(async move {
            sleep(Duration::from_millis(200)).await;
            let peer = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            peer.send_to(b"reply", local).await.unwrap();
            peer.local_addr().unwrap()
        });
        let mut buf = [0; 32];
        let (len, from) = timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
            .await
            .expect("UDP reply timed out")
            .expect("an unreachable node must not kill UDP reception");
        assert_eq!(&buf[..len], b"reply");
        assert_eq!(from, sender.await.unwrap());
    }
}

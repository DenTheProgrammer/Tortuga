//! Hostname resolution that survives filtering DNS.
//!
//! Ad-blocking DNS (AdGuard, Pi-hole, some VPNs) often blocks tracker and DHT
//! bootstrap names: lookups fail or return a sink address like 0.0.0.0. Other
//! clients then still work from cached DHT nodes, but a fresh install can't
//! find a single peer. We use the system resolver first and fall back to
//! DNS-over-HTTPS (Cloudflare, then Google, by IP) when it fails or answers
//! with a sink address.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use serde::Deserialize;
use tracing::{debug, info};

const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const DOH_TIMEOUT: Duration = Duration::from_secs(6);

/// DoH JSON endpoints, tried in order. Addressed by IP so they don't depend on
/// the (possibly filtered) system DNS.
static ENDPOINTS: RwLock<Vec<String>> = RwLock::new(Vec::new());
/// host -> (addresses, resolved at)
type Cache = HashMap<String, (Vec<IpAddr>, Instant)>;
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

fn endpoints() -> Vec<String> {
    let e = ENDPOINTS.read();
    if e.is_empty() {
        vec![
            "https://1.1.1.1/dns-query".into(),
            "https://8.8.8.8/resolve".into(),
            "https://1.0.0.1/dns-query".into(),
            "https://8.8.4.4/resolve".into(),
        ]
    } else {
        e.clone()
    }
}

/// Override the DoH endpoints (tests).
#[doc(hidden)]
pub fn set_doh_endpoints(urls: Vec<String>) {
    *ENDPOINTS.write() = urls;
    *CACHE.lock() = None;
}

/// True if a filter answered instead of the real DNS (0.0.0.0, ::, or
/// loopback for a public name).
fn is_sink(ip: &IpAddr) -> bool {
    ip.is_unspecified() || ip.is_loopback()
}

fn is_local_name(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost")
}

#[derive(Deserialize)]
struct DohResponse {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Answer", default)]
    answer: Vec<DohAnswer>,
}

#[derive(Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    kind: u16,
    data: String,
}

fn parse_doh(body: &[u8]) -> Vec<IpAddr> {
    let Ok(r) = serde_json::from_slice::<DohResponse>(body) else {
        return Vec::new();
    };
    if r.status != 0 {
        return Vec::new();
    }
    r.answer
        .iter()
        .filter(|a| a.kind == 1 || a.kind == 28)
        .filter_map(|a| a.data.parse().ok())
        .filter(|ip| !is_sink(ip))
        .collect()
}

fn doh_client() -> reqwest::Client {
    static CLIENT: Mutex<Option<reqwest::Client>> = Mutex::new(None);
    CLIENT
        .lock()
        .get_or_insert_with(|| {
            reqwest::Client::builder()
                .timeout(DOH_TIMEOUT)
                .build()
                .unwrap_or_default()
        })
        .clone()
}

async fn doh_lookup(host: &str) -> Vec<IpAddr> {
    let client = doh_client();
    for endpoint in endpoints() {
        let mut ips = Vec::new();
        for kind in ["A", "AAAA"] {
            let Ok(url) = url::Url::parse_with_params(&endpoint, [("name", host), ("type", kind)])
            else {
                continue;
            };
            let resp = client
                .get(url)
                .header("accept", "application/dns-json")
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    if let Ok(body) = r.bytes().await {
                        ips.extend(parse_doh(&body));
                    }
                }
                Ok(r) => debug!(%endpoint, status = %r.status(), "DoH error"),
                Err(e) => debug!(%endpoint, "DoH failed: {e}"),
            }
        }
        if !ips.is_empty() {
            return ips;
        }
    }
    Vec::new()
}

/// Resolve `host`, falling back to DNS-over-HTTPS if the system DNS fails or
/// is filtered.
pub async fn resolve_ips(host: &str) -> std::io::Result<Vec<IpAddr>> {
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    if let Some((ips, at)) = CACHE.lock().as_ref().and_then(|c| c.get(host).cloned())
        && at.elapsed() < CACHE_TTL
    {
        return Ok(ips);
    }

    let system: Vec<IpAddr> = match tokio::net::lookup_host((host, 0)).await {
        Ok(it) => it.map(|a| a.ip()).collect(),
        Err(e) => {
            debug!(host, "system DNS failed: {e}");
            Vec::new()
        }
    };
    let usable: Vec<IpAddr> = if is_local_name(host) {
        system
    } else {
        system.into_iter().filter(|ip| !is_sink(ip)).collect()
    };
    let ips = if usable.is_empty() && !is_local_name(host) {
        let ips = doh_lookup(host).await;
        if !ips.is_empty() {
            info!(host, "system DNS blocked or failed; resolved over HTTPS");
        }
        ips
    } else {
        usable
    };

    if ips.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("can't resolve {host}"),
        ));
    }
    CACHE
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(host.to_owned(), (ips.clone(), Instant::now()));
    Ok(ips)
}

pub async fn resolve(host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
    Ok(resolve_ips(host)
        .await?
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect())
}

/// For reqwest (HTTP trackers, .torrent URLs, RSS).
pub struct FallbackResolver;

impl reqwest::dns::Resolve for FallbackResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs = resolve(&host, 0).await?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub fn reqwest_resolver() -> Arc<dyn reqwest::dns::Resolve> {
    Arc::new(FallbackResolver)
}

/// Use the fallback for UDP tracker hostnames too.
pub fn install_udp_tracker_resolver() {
    librqbit_tracker_comms::resolver::set_resolver(Some(Arc::new(|host, port| {
        Box::pin(async move { resolve(&host, port).await })
    })));
}

/// Well-known DHT entry points (the same set uTorrent / qBittorrent use).
const DHT_BOOTSTRAP: &[(&str, u16)] = &[
    ("router.bittorrent.com", 6881),
    ("router.utorrent.com", 6881),
    ("dht.transmissionbt.com", 6881),
    ("dht.libtorrent.org", 25401),
    ("dht.aelitis.com", 6881),
];

/// DHT bootstrap nodes as "ip:port", resolved with the fallback so a filtering
/// DNS can't leave DHT without entry points. Names that can't be resolved at
/// all are kept as is.
pub async fn dht_bootstrap_addrs() -> Vec<String> {
    let lookups = DHT_BOOTSTRAP.iter().map(|(host, port)| async move {
        match tokio::time::timeout(Duration::from_secs(5), resolve(host, *port)).await {
            Ok(Ok(addrs)) if !addrs.is_empty() => {
                addrs.into_iter().map(|a| a.to_string()).collect::<Vec<_>>()
            }
            _ => vec![format!("{host}:{port}")],
        }
    });
    futures::future::join_all(lookups)
        .await
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_doh_answers_and_drops_sinks() {
        let body = br#"{"Status":0,"Answer":[
            {"name":"t.example","type":5,"data":"cdn.example."},
            {"name":"cdn.example","type":1,"data":"93.184.216.34"},
            {"name":"cdn.example","type":1,"data":"0.0.0.0"},
            {"name":"cdn.example","type":28,"data":"2606:2800:220:1::1"}]}"#;
        let ips = parse_doh(body);
        assert_eq!(ips.len(), 2);
        assert!(ips.contains(&"93.184.216.34".parse().unwrap()));
        assert!(parse_doh(br#"{"Status":3}"#).is_empty(), "NXDOMAIN");
        assert!(parse_doh(b"garbage").is_empty());
    }

    #[test]
    fn sink_detection() {
        assert!(is_sink(&"0.0.0.0".parse().unwrap()));
        assert!(is_sink(&"::".parse().unwrap()));
        assert!(is_sink(&"127.0.0.1".parse().unwrap()));
        assert!(!is_sink(&"1.2.3.4".parse().unwrap()));
    }
}

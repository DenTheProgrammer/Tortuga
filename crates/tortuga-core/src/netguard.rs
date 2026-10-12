//! Picks how torrent traffic is routed (see `net.rs`) and reacts to VPNs
//! appearing / disappearing.
//!
//! Auto mode goes through the system route first. If a VPN is active and,
//! after a grace period, downloads are clearly starved (no DHT nodes, or no
//! live peers and almost no speed), it switches to binding the physical
//! adapter, i.e. bypasses the VPN. It stays there while the VPN is up and
//! returns to the system route when the VPN goes away.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use tracing::info;

use crate::{
    engine::{Engine, Event, Health},
    net::{self, Binding, NetSnapshot, NetStatus, NetworkMode, Route},
};

/// How long downloads must look starved through the VPN before switching.
const STARVED_FOR: Duration = Duration::from_secs(75);
/// Below this total download speed with few peers we consider P2P blocked/throttled.
const STARVED_BPS: u64 = 24 * 1024;
const STARVED_MAX_PEERS: u32 = 2;
/// DHT with this few nodes after the grace period means UDP is blocked.
const DHT_MIN_NODES: usize = 8;
/// Known-but-unreachable peers needed to blame the VPN rather than a dead swarm.
const UNREACHABLE_PEERS: u32 = 4;

#[derive(Default)]
struct AutoState {
    /// When we started observing the current (VPN, system route) situation.
    since: Option<Instant>,
    /// When downloads first looked starved.
    starved_since: Option<Instant>,
    /// Auto mode decided to bypass the VPN (sticky while the VPN stays up).
    bypassing: bool,
    /// Identity of the VPN interface the decision was made for.
    vpn_key: Option<String>,
}

impl AutoState {
    /// One observation in auto mode while a VPN is up; returns whether to
    /// bypass it.
    fn step(&mut self, vpn_key: Option<String>, health: &Health, dht: bool, now: Instant) -> bool {
        if self.vpn_key != vpn_key {
            *self = AutoState {
                vpn_key,
                since: Some(now),
                ..Default::default()
            };
        }
        if self.bypassing {
            return true;
        }
        let since = *self.since.get_or_insert(now);
        let grace_over = now.duration_since(since) >= STARVED_FOR;
        let dht_blocked = grace_over && dht && health.dht_nodes < DHT_MIN_NODES;
        // Peers are known (trackers/DHT work) but we can't reach them. A dead
        // swarm with no peers at all is not the VPN's fault.
        let unreachable = health.seen_peers >= health.live_peers + UNREACHABLE_PEERS;
        let starved = health.downloading > 0
            && health.down_bps < STARVED_BPS
            && health.live_peers <= STARVED_MAX_PEERS
            && (unreachable || dht_blocked);
        if starved {
            self.starved_since.get_or_insert(now);
        } else {
            self.starved_since = None;
        }
        let starved_long = self
            .starved_since
            .is_some_and(|s| now.duration_since(s) >= STARVED_FOR);
        if (dht_blocked && health.downloading > 0) || starved_long {
            info!(?health, "VPN looks bad for P2P, bypassing it");
            self.bypassing = true;
        }
        self.bypassing
    }
}

static AUTO: Mutex<Option<AutoState>> = Mutex::new(None);

fn with_auto<R>(f: impl FnOnce(&mut AutoState) -> R) -> R {
    let mut g = AUTO.lock();
    f(g.get_or_insert_with(Default::default))
}

pub(crate) fn reset_auto(_engine: &Engine) {
    with_auto(|a| *a = AutoState::default());
}

struct Decision {
    binding: Binding,
    route: Route,
    interface_name: Option<String>,
    reason: &'static str,
}

fn decide(engine: &Engine, snap: &NetSnapshot) -> Decision {
    let settings = engine.settings();
    let mode = settings.network.mode;
    let system = |reason| Decision {
        binding: Binding::default(),
        route: Route::System,
        interface_name: None,
        reason,
    };
    let pick = |name: &Option<String>, fallback: &Option<net::NetInterface>| {
        name.as_deref()
            .and_then(|n| net::find_interface(&snap.interfaces, n).cloned())
            .or_else(|| fallback.clone())
    };

    match mode {
        NetworkMode::System => system("mode_system"),
        NetworkMode::Direct => match pick(&settings.network.direct_interface, &snap.physical) {
            Some(i) => Decision {
                binding: net::binding_for(&i),
                route: Route::Direct,
                interface_name: Some(i.display_name.clone()),
                reason: "mode_direct",
            },
            None => system("no_physical_interface"),
        },
        NetworkMode::VpnOnly => {
            let vpn_fallback = snap
                .vpn
                .clone()
                .or_else(|| snap.interfaces.iter().find(|i| i.is_vpn).cloned());
            match pick(&settings.network.vpn_interface, &vpn_fallback)
                .filter(|i| i.is_vpn || settings.network.vpn_interface.is_some())
            {
                Some(i) => Decision {
                    binding: net::binding_for(&i),
                    route: Route::Vpn,
                    interface_name: Some(i.display_name.clone()),
                    reason: "mode_vpn_only",
                },
                None => Decision {
                    binding: Binding::default(),
                    route: Route::Blocked,
                    interface_name: None,
                    reason: "vpn_missing_blocked",
                },
            }
        }
        NetworkMode::Auto => {
            if !snap.vpn_active {
                with_auto(|a| *a = AutoState::default());
                return system("auto_no_vpn");
            }
            let vpn_key = snap.vpn.as_ref().map(|v| v.bind_name.clone());
            let health = engine.health();
            let dht = engine.settings().dht;
            let bypass = with_auto(|a| a.step(vpn_key, &health, dht, Instant::now()));
            match (bypass, &snap.physical) {
                (true, Some(i)) => Decision {
                    binding: net::binding_for(i),
                    route: Route::Direct,
                    interface_name: Some(i.display_name.clone()),
                    reason: "auto_vpn_slow_bypassing",
                },
                (true, None) => system("no_physical_interface"),
                (false, _) => system("auto_vpn_ok"),
            }
        }
    }
}

pub(crate) fn initial_binding(engine: &Engine) -> Binding {
    let snap = net::snapshot();
    let d = decide(engine, &snap);
    set_status(engine, &snap, &d);
    if d.route == Route::Blocked {
        engine.set_network_blocked(true);
    }
    d.binding
}

fn set_status(engine: &Engine, snap: &NetSnapshot, d: &Decision) -> bool {
    let status = NetStatus {
        mode: engine.network_mode(),
        route: d.route,
        vpn_active: snap.vpn_active,
        vpn_name: snap.vpn.as_ref().map(|v| v.display_name.clone()),
        interface_name: d.interface_name.clone(),
        reason: d.reason,
    };
    let mut g = engine.net_status.write();
    let changed = g.route != status.route
        || g.vpn_active != status.vpn_active
        || g.interface_name != status.interface_name
        || g.mode != status.mode
        || g.reason != status.reason;
    *g = status;
    changed
}

pub(crate) async fn evaluate(engine: &Arc<Engine>) {
    let snap = tokio::task::spawn_blocking(net::snapshot)
        .await
        .unwrap_or_else(|_| net::snapshot());
    let d = decide(engine, &snap);
    #[cfg(windows)]
    let d = {
        let mut d = d;
        if d.route == Route::Direct && direct_access_denied(&d.binding).await {
            d.reason = "direct_access_denied";
        }
        d
    };
    let changed = set_status(engine, &snap, &d);
    engine.set_network_blocked(d.route == Route::Blocked);
    if d.route != Route::Blocked
        && let Err(e) = engine.restart_with_binding(d.binding.clone()).await
    {
        engine.emit(Event::Error {
            id: None,
            message: format!("network: {e:#}"),
        });
    }
    if changed {
        let status = engine.net_status.read().clone();
        engine.emit(Event::NetworkChanged { status });
    }
}

// A selected adapter does not mean traffic can leave through it. VPN clients
// may reject non-tunnel sockets in WFP, before any packet reaches the network.
// Check from Tortuga's own process and retain the selected direct binding:
// silently falling back to the VPN would violate explicit direct mode.
#[cfg(windows)]
async fn direct_access_denied(binding: &Binding) -> bool {
    use librqbit_dualstack_sockets::{BindDevice, ConnectOpts, Error, tcp_connect};

    static CACHE: Mutex<Option<(Binding, Instant, bool)>> = Mutex::new(None);
    if let Some((_, _, denied)) = CACHE
        .lock()
        .as_ref()
        .filter(|(b, at, _)| b == binding && at.elapsed() < Duration::from_secs(30))
    {
        return *denied;
    }
    let Some(device) = binding
        .device
        .as_deref()
        .and_then(|name| BindDevice::new_from_name(name).ok())
    else {
        return false;
    };
    let probe = |addr| {
        tokio::time::timeout(
            Duration::from_secs(2),
            tcp_connect(
                addr,
                ConnectOpts {
                    bind_device: Some(&device),
                    ..Default::default()
                },
            ),
        )
    };
    let (first, second) = tokio::join!(
        probe("1.1.1.1:443".parse().unwrap()),
        probe("8.8.8.8:443".parse().unwrap())
    );
    let denied = [first, second].into_iter().all(|result| {
        matches!(result, Ok(Err(Error::Connect(ref e)))
            if e.kind() == std::io::ErrorKind::PermissionDenied)
    });
    *CACHE.lock() = Some((binding.clone(), Instant::now(), denied));
    denied
}

pub(crate) fn spawn(engine: Arc<Engine>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = engine.cancel.cancelled() => break,
                _ = interval.tick() => {}
            }
            evaluate(&engine).await;
        }
    });
}

pub fn interfaces() -> NetSnapshot {
    net::snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(downloading: u32, bps: u64, live: u32, seen: u32, dht: usize) -> Health {
        Health {
            downloading,
            down_bps: bps,
            live_peers: live,
            seen_peers: seen,
            dht_nodes: dht,
        }
    }

    /// Feed the same health every 5 s for `secs`; return the final decision.
    fn run(a: &mut AutoState, vpn: &str, h: Health, start: Instant, secs: u64) -> bool {
        let mut out = false;
        for t in (0..=secs).step_by(5) {
            out = a.step(Some(vpn.into()), &h, true, start + Duration::from_secs(t));
        }
        out
    }

    #[test]
    fn bypasses_when_peers_are_unreachable() {
        let mut a = AutoState::default();
        let t0 = Instant::now();
        let h = health(1, 0, 0, 40, 300);
        assert!(!run(&mut a, "wg0", h, t0, 60), "too early");
        assert!(run(&mut a, "wg0", h, t0, 160));
    }

    #[test]
    fn dead_swarm_is_not_the_vpns_fault() {
        let mut a = AutoState::default();
        assert!(!run(
            &mut a,
            "wg0",
            health(1, 0, 0, 0, 300),
            Instant::now(),
            600
        ));
    }

    #[test]
    fn healthy_vpn_is_kept() {
        let mut a = AutoState::default();
        assert!(!run(
            &mut a,
            "wg0",
            health(2, 3_000_000, 25, 60, 300),
            Instant::now(),
            600
        ));
        // Seeding only: nothing to judge by.
        assert!(!run(
            &mut a,
            "wg0",
            health(0, 0, 0, 50, 300),
            Instant::now(),
            600
        ));
    }

    #[test]
    fn blocked_dht_while_downloading_bypasses() {
        let mut a = AutoState::default();
        assert!(run(
            &mut a,
            "wg0",
            health(1, 0, 0, 0, 0),
            Instant::now(),
            120
        ));
    }

    #[test]
    fn new_vpn_starts_over() {
        let mut a = AutoState::default();
        let t0 = Instant::now();
        assert!(run(&mut a, "wg0", health(1, 0, 0, 40, 300), t0, 200));
        let later = t0 + Duration::from_secs(300);
        assert!(!a.step(Some("tun1".into()), &health(1, 0, 0, 40, 300), true, later));
    }
}

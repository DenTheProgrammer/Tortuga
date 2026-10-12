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
    engine::{Engine, Event},
    net::{self, Binding, NetSnapshot, NetStatus, NetworkMode, Route},
};

/// How long downloads must look starved through the VPN before switching.
const STARVED_FOR: Duration = Duration::from_secs(75);
/// Below this total download speed with few peers we consider P2P blocked/throttled.
const STARVED_BPS: u64 = 24 * 1024;
const STARVED_MAX_PEERS: u32 = 2;
/// DHT with this few nodes after the grace period means UDP is blocked.
const DHT_MIN_NODES: usize = 8;

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
            let now = Instant::now();
            let bypass = with_auto(|a| {
                if a.vpn_key != vpn_key {
                    *a = AutoState {
                        vpn_key: vpn_key.clone(),
                        since: Some(now),
                        ..Default::default()
                    };
                }
                if a.bypassing {
                    return true;
                }
                let since = *a.since.get_or_insert(now);
                let grace_over = now.duration_since(since) >= STARVED_FOR;
                let dht_blocked =
                    grace_over && engine.settings().dht && health.dht_nodes < DHT_MIN_NODES;
                let starved = health.downloading > 0
                    && health.down_bps < STARVED_BPS
                    && health.live_peers <= STARVED_MAX_PEERS;
                if starved {
                    a.starved_since.get_or_insert(now);
                } else {
                    a.starved_since = None;
                }
                let starved_long = a
                    .starved_since
                    .is_some_and(|s| now.duration_since(s) >= STARVED_FOR);
                if (dht_blocked && health.downloading > 0) || starved_long {
                    info!(?health, "VPN looks bad for P2P, bypassing it");
                    a.bypassing = true;
                }
                a.bypassing
            });
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
        || g.mode != status.mode;
    *g = status;
    changed
}

pub(crate) async fn evaluate(engine: &Engine) {
    let snap = tokio::task::spawn_blocking(net::snapshot)
        .await
        .unwrap_or_else(|_| net::snapshot());
    let d = decide(engine, &snap);
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

pub(crate) fn spawn(engine: Arc<Engine>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            evaluate(&engine).await;
        }
    });
}

pub fn interfaces() -> NetSnapshot {
    net::snapshot()
}

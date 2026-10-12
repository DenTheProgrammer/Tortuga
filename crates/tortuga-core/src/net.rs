//! Network interface discovery and VPN handling.
//!
//! Tortuga can route torrent traffic:
//! - like the rest of the system (through the VPN if one is up),
//! - directly through the physical adapter, bypassing the VPN (split tunnel),
//! - only through the VPN (kill switch: no VPN, no traffic),
//! - automatically: through the system route first, switching to direct
//!   when the VPN blocks or throttles P2P.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    #[default]
    Auto,
    System,
    Direct,
    VpnOnly,
}

#[derive(Clone, Debug, Serialize)]
pub struct NetInterface {
    /// Name used for binding (index on Windows, OS name elsewhere).
    pub bind_name: String,
    /// Human readable name (friendly name on Windows).
    pub display_name: String,
    pub description: Option<String>,
    pub index: u32,
    pub ipv4: Vec<Ipv4Addr>,
    pub is_vpn: bool,
    pub is_default_route: bool,
    pub has_gateway: bool,
    pub kind: &'static str,
}

/// How the BitTorrent session should bind its sockets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Binding {
    pub device: Option<String>,
    pub local_ip: Option<IpAddr>,
}

fn vpn_name_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?ix)
            wireguard | wintun | \bwg\d* | \btun\d* | utun | \btap\b | tap-windows | \bppp
            | openvpn | nordlynx | nordvpn | proton | mullvad | expressvpn | surfshark | windscribe
            | cyberghost | private\ internet\ access | \bpia\b | hotspot\ shield | ipvanish | tunnelbear
            | amnezia | outline | hiddify | v2ray | xray | sing-?box | clash | nekoray | throne | happ
            | anyconnect | fortinet | forticlient | globalprotect | pangp | juniper | pulse\ secure
            | softether | zscaler | cloudflare\ warp | \bwarp\b | adguard\ vpn | kaspersky\ vpn | radmin
            | tailscale | zerotier | hamachi | vpn",
        )
        .unwrap()
    })
}

fn kind_of(t: netdev::interface::types::InterfaceType) -> &'static str {
    use netdev::interface::types::InterfaceType as T;
    match t {
        T::Ethernet | T::FastEthernetT | T::FastEthernetFx | T::GigabitEthernet => "ethernet",
        T::Wireless80211 => "wifi",
        T::Loopback => "loopback",
        T::Tunnel | T::Ppp | T::ProprietaryVirtual => "tunnel",
        T::Wwanpp | T::Wwanpp2 => "cellular",
        _ => "other",
    }
}

/// Local IP the OS would use to reach the internet. No packets are sent.
pub fn default_route_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).ok()?;
    s.connect(SocketAddr::from((Ipv4Addr::new(1, 1, 1, 1), 53)))
        .ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

pub fn list_interfaces() -> Vec<NetInterface> {
    let default_ip = default_route_ip();
    let re = vpn_name_re();
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| !i.is_loopback() && i.is_up())
        .filter(|i| !i.ipv4.is_empty())
        .map(|i| {
            let display_name = i.friendly_name.clone().unwrap_or_else(|| i.name.clone());
            let haystack = format!(
                "{} {} {}",
                i.name,
                display_name,
                i.description.clone().unwrap_or_default()
            );
            let kind = kind_of(i.if_type);
            let is_vpn = kind == "tunnel" || i.is_tun() || re.is_match(&haystack);
            let ipv4: Vec<Ipv4Addr> = i.ipv4.iter().map(|n| n.addr()).collect();
            let is_default_route = default_ip
                .map(|d| ipv4.iter().any(|a| IpAddr::V4(*a) == d))
                .unwrap_or(false);
            NetInterface {
                bind_name: if cfg!(windows) {
                    i.index.to_string()
                } else {
                    i.name.clone()
                },
                display_name,
                description: i.description.clone(),
                index: i.index,
                ipv4,
                is_vpn,
                is_default_route,
                has_gateway: i.gateway.is_some(),
                kind,
            }
        })
        .collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct NetSnapshot {
    pub interfaces: Vec<NetInterface>,
    /// The default route goes through a VPN interface.
    pub vpn_active: bool,
    pub vpn: Option<NetInterface>,
    /// Best physical adapter to bypass the VPN.
    pub physical: Option<NetInterface>,
}

pub fn snapshot() -> NetSnapshot {
    let interfaces = list_interfaces();
    let vpn = interfaces
        .iter()
        .find(|i| i.is_vpn && i.is_default_route)
        .cloned();
    let physical = pick_physical(&interfaces);
    NetSnapshot {
        vpn_active: vpn.is_some(),
        vpn,
        physical,
        interfaces,
    }
}

fn is_private_v4(a: &Ipv4Addr) -> bool {
    // 100.64.0.0/10 is carrier-grade NAT, also a LAN-ish address.
    a.is_private() || a.is_link_local() || (a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 64)
}

/// A non-VPN adapter that most likely has a gateway: prefer Ethernet, then Wi-Fi,
/// and adapters with a private (LAN) address, which is what home routers hand out.
fn pick_physical(ifaces: &[NetInterface]) -> Option<NetInterface> {
    ifaces
        .iter()
        .filter(|i| !i.is_vpn && i.kind != "loopback")
        .filter(|i| i.ipv4.iter().any(|a| !a.is_link_local()))
        .max_by_key(|i| {
            let kind = match i.kind {
                "ethernet" => 3,
                "wifi" => 2,
                "cellular" => 1,
                _ => 0,
            };
            (
                i.is_default_route,
                i.has_gateway,
                kind,
                i.ipv4.iter().any(is_private_v4),
            )
        })
        .cloned()
}

pub fn find_interface<'a>(ifaces: &'a [NetInterface], name: &str) -> Option<&'a NetInterface> {
    ifaces
        .iter()
        .find(|i| i.bind_name == name || i.display_name == name)
}

/// Turn an interface into session binding options.
pub fn binding_for(iface: &NetInterface) -> Binding {
    Binding {
        device: Some(iface.bind_name.clone()),
        // Windows can't bind HTTP clients to a device; bind to its address instead.
        local_ip: if cfg!(windows) {
            iface.ipv4.first().copied().map(IpAddr::V4)
        } else {
            None
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// Default OS routing.
    System,
    /// Bound to the physical adapter, bypassing the VPN.
    Direct,
    /// Bound to the VPN adapter.
    Vpn,
    /// VPN-only mode and no VPN: traffic blocked.
    Blocked,
}

#[derive(Clone, Debug, Serialize)]
pub struct NetStatus {
    pub mode: NetworkMode,
    pub route: Route,
    pub vpn_active: bool,
    pub vpn_name: Option<String>,
    pub interface_name: Option<String>,
    /// Machine-readable reason for the current route, translated by the UI.
    pub reason: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(
        name: &str,
        kind: &'static str,
        vpn: bool,
        default: bool,
        ip: [u8; 4],
    ) -> NetInterface {
        NetInterface {
            bind_name: name.into(),
            display_name: name.into(),
            description: None,
            index: 1,
            ipv4: vec![Ipv4Addr::from(ip)],
            is_vpn: vpn,
            is_default_route: default,
            has_gateway: true,
            kind,
        }
    }

    #[test]
    fn vpn_names() {
        let re = vpn_name_re();
        for n in [
            "WireGuard Tunnel",
            "wg0",
            "tun0",
            "TAP-Windows Adapter V9",
            "AmneziaVPN",
            "utun3",
            "NordLynx",
        ] {
            assert!(re.is_match(n), "{n}");
        }
        for n in [
            "Ethernet",
            "Wi-Fi",
            "eth0",
            "wlan0",
            "Realtek PCIe GbE Family Controller",
            "Intel(R) Wi-Fi 6 AX201",
        ] {
            assert!(!re.is_match(n), "{n}");
        }
    }

    #[test]
    fn picks_ethernet_over_wifi_and_never_vpn() {
        let list = vec![
            iface("wg0", "tunnel", true, true, [10, 2, 0, 2]),
            iface("wlan0", "wifi", false, false, [192, 168, 1, 5]),
            iface("eth0", "ethernet", false, false, [192, 168, 1, 6]),
        ];
        assert_eq!(pick_physical(&list).unwrap().bind_name, "eth0");
    }

    #[test]
    fn prefers_wifi_with_gateway_over_wsl_ethernet_without_one() {
        let wifi = iface("Wi-Fi", "wifi", false, false, [192, 168, 1, 98]);
        let mut wsl = iface("vEthernet (WSL)", "ethernet", false, false, [172, 22, 64, 1]);
        wsl.has_gateway = false;
        let vpn = iface("tun2", "tunnel", true, true, [10, 33, 0, 2]);
        assert_eq!(pick_physical(&[wifi, wsl, vpn]).unwrap().bind_name, "Wi-Fi");
    }
}

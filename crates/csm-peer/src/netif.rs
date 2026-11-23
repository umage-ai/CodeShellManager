//! This machine's addresses, for the address picker in Settings > Remote access.
//!
//! A read-only system query (`getifaddrs` / `GetAdaptersAddresses` through `if-addrs`). Nothing is
//! opened or sent. The list is a convenience: the person may name any address of this machine, and
//! the server binds exactly the one chosen (see `csm_peer_proto::addr::check_bind`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use csm_engine::NetInterface;

/// The most addresses listed (a machine with many container bridges still gets a usable list).
const MAX: usize = 64;

/// Whether an adapter name looks like a WireGuard tunnel: the kernel's `wg0`, the Windows tunnel
/// service's `wg` / `WireGuard Tunnel`, or any name that says so. A tunnel with another name is
/// simply not marked; the person can still pick it.
pub fn looks_like_wireguard(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    if n.contains("wireguard") {
        return true;
    }
    match n.strip_prefix("wg") {
        Some(rest) => rest.chars().next().is_none_or(|c| c.is_ascii_digit() || matches!(c, '-' | '_' | ' ')),
        None => false,
    }
}

/// An address worth offering: not unspecified, multicast or link-local (a link-local IPv6 address
/// cannot be bound without a scope, and 169.254/16 is a failed DHCP lease).
fn offered(ip: &IpAddr) -> bool {
    if ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

/// The network an address is on, `10.8.0.0/24`. `None` when the prefix says nothing (none, or a
/// host route: `/32`, `/128`).
pub fn network(ip: &IpAddr, prefix: u8) -> Option<String> {
    match ip {
        IpAddr::V4(a) if (1..32).contains(&prefix) => {
            let mask = u32::MAX << (32 - u32::from(prefix));
            Some(format!("{}/{prefix}", Ipv4Addr::from(u32::from(*a) & mask)))
        }
        IpAddr::V6(a) if (1..128).contains(&prefix) => {
            let mask = u128::MAX << (128 - u32::from(prefix));
            Some(format!("{}/{prefix}", Ipv6Addr::from(u128::from(*a) & mask)))
        }
        _ => None,
    }
}

/// The offered addresses, WireGuard first, then other interfaces, loopback last. Pure.
pub fn arrange(mut list: Vec<NetInterface>) -> Vec<NetInterface> {
    list.sort_by(|a, b| (!a.wireguard, a.loopback, &a.name, &a.address).cmp(&(!b.wireguard, b.loopback, &b.name, &b.address)));
    list.dedup();
    list.truncate(MAX);
    list
}

/// This machine's interfaces. An empty list when the system will not say.
pub fn list() -> Vec<NetInterface> {
    let Ok(all) = if_addrs::get_if_addrs() else { return Vec::new() };
    let found = all
        .into_iter()
        .filter(|i| !matches!(i.oper_status, if_addrs::IfOperStatus::Down | if_addrs::IfOperStatus::NotPresent | if_addrs::IfOperStatus::LowerLayerDown))
        .filter(|i| offered(&i.ip()))
        .map(|i| {
            let prefix = match &i.addr {
                if_addrs::IfAddr::V4(a) => a.prefixlen,
                if_addrs::IfAddr::V6(a) => a.prefixlen,
            };
            let (wireguard, loopback, address) = (looks_like_wireguard(&i.name), i.is_loopback(), i.ip().to_string());
            NetInterface { wireguard, loopback, network: network(&i.ip(), prefix), name: i.name, address }
        })
        .collect();
    arrange(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ni(name: &str, address: &str, wireguard: bool, loopback: bool) -> NetInterface {
        NetInterface { name: name.into(), address: address.into(), network: None, wireguard, loopback }
    }

    #[test]
    fn wireguard_is_recognised_by_name() {
        for yes in ["wg", "wg0", "wg-office", "WireGuard Tunnel", "my wireguard", "WG1"] {
            assert!(looks_like_wireguard(yes), "{yes}");
        }
        for no in ["Ethernet", "eth0", "wlan0", "vEthernet (WSL)", "lo", "wget0", "wgx"] {
            assert!(!looks_like_wireguard(no), "{no}");
        }
    }

    #[test]
    fn networks_come_from_the_prefix() {
        let n = |a: &str, p| network(&a.parse().unwrap(), p);
        assert_eq!(n("10.8.0.2", 24).as_deref(), Some("10.8.0.0/24"));
        assert_eq!(n("192.168.77.130", 25).as_deref(), Some("192.168.77.128/25"));
        assert_eq!(n("fd00:1:2:3::5", 64).as_deref(), Some("fd00:1:2:3::/64"));
        assert_eq!(n("10.8.0.2", 32), None);
        assert_eq!(n("10.8.0.2", 0), None);
        assert_eq!(n("::1", 128), None);
    }

    #[test]
    fn the_wireguard_address_comes_first_and_loopback_last() {
        let out = arrange(vec![ni("lo", "127.0.0.1", false, true), ni("eth0", "192.168.1.20", false, false), ni("wg0", "10.8.0.2", true, false)]);
        let names: Vec<&str> = out.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["wg0", "eth0", "lo"]);
    }

    #[test]
    fn link_local_and_wildcard_addresses_are_not_offered() {
        for no in ["0.0.0.0", "::", "169.254.3.4", "fe80::1", "224.0.0.1", "ff02::1"] {
            assert!(!offered(&no.parse().unwrap()), "{no}");
        }
        for yes in ["10.8.0.2", "192.168.1.20", "127.0.0.1", "::1", "fd00::5"] {
            assert!(offered(&yes.parse().unwrap()), "{yes}");
        }
    }

    #[test]
    fn this_machine_lists_without_a_wildcard() {
        for i in list() {
            let ip: IpAddr = i.address.parse().unwrap();
            assert!(!ip.is_unspecified(), "{i:?}");
        }
    }
}

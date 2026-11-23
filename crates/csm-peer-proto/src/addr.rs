//! The address a host listens on and a pairing link names: `tcp://host:port`, with an IP
//! literal or a DNS name. The server binds only where the person chose; [`check_bind`] is the one
//! place that refuses `0.0.0.0` and `::` unless the person asked for "all interfaces".

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddrError {
    /// Not `tcp://host:port`, or the host or port is malformed.
    Format,
    /// `0.0.0.0` or `::`: listens on every interface (or connects to nothing in particular).
    Unspecified,
    /// A multicast or broadcast address.
    NotUnicast,
    /// Port 0.
    Port,
}

impl std::fmt::Display for AddrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Format => "the address is not tcp://host:port",
            Self::Unspecified => "0.0.0.0 and :: are not an address to listen on or connect to",
            Self::NotUnicast => "a multicast or broadcast address is not a host",
            Self::Port => "the port must be 1-65535",
        })
    }
}

impl std::error::Error for AddrError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    Ip(IpAddr),
    /// A DNS name, lowercase.
    Name(String),
}

/// A host and port, as a pairing link carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAddr {
    pub host: Host,
    pub port: u16,
}

const SCHEME: &str = "tcp://";

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|l| {
            !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn ip_ok(ip: &IpAddr) -> Result<(), AddrError> {
    // `::ffff:0.0.0.0` is IPv4 `0.0.0.0` on a dual-stack socket: judge the IPv4 form.
    let ip = &ip.to_canonical();
    if ip.is_unspecified() {
        return Err(AddrError::Unspecified);
    }
    if ip.is_multicast() || *ip == IpAddr::V4(Ipv4Addr::BROADCAST) {
        return Err(AddrError::NotUnicast);
    }
    Ok(())
}

impl PeerAddr {
    /// Parses `tcp://host:port`. An IPv6 literal is bracketed, with no zone. A name is letters,
    /// digits, dots and hyphens. Unspecified and multicast addresses and port 0 are refused.
    pub fn parse(input: &str) -> Result<Self, AddrError> {
        let rest = input.strip_prefix(SCHEME).ok_or(AddrError::Format)?;
        if rest.is_empty() || rest.len() > 300 {
            return Err(AddrError::Format);
        }
        let (host_text, port_text) = if let Some(v6) = rest.strip_prefix('[') {
            let (inner, after) = v6.split_once(']').ok_or(AddrError::Format)?;
            (inner, after.strip_prefix(':').ok_or(AddrError::Format)?)
        } else {
            rest.rsplit_once(':').ok_or(AddrError::Format)?
        };
        if port_text.is_empty() || port_text.len() > 5 || !port_text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(AddrError::Format);
        }
        let port: u16 = port_text.parse().map_err(|_| AddrError::Format)?;
        if port == 0 {
            return Err(AddrError::Port);
        }
        let bracketed = rest.starts_with('[');
        let host = if bracketed {
            let ip: IpAddr = host_text.parse().map_err(|_| AddrError::Format)?;
            if !ip.is_ipv6() {
                return Err(AddrError::Format);
            }
            Host::Ip(ip)
        } else if let Ok(ip) = host_text.parse::<Ipv4Addr>() {
            Host::Ip(IpAddr::V4(ip))
        } else if host_text.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            // Looks like an IPv4 literal that did not parse (`1.2.3`, `1.2.3.4.5`, `256.1.1.1`).
            return Err(AddrError::Format);
        } else if is_name(host_text) {
            Host::Name(host_text.to_ascii_lowercase())
        } else {
            return Err(AddrError::Format);
        };
        if let Host::Ip(ip) = &host {
            ip_ok(ip)?;
        }
        Ok(Self { host, port })
    }

    pub fn from_socket(addr: SocketAddr) -> Result<Self, AddrError> {
        check_bind(&addr, false)?;
        Ok(Self { host: Host::Ip(addr.ip()), port: addr.port() })
    }

    /// `tcp://host:port`, which [`PeerAddr::parse`] reads back.
    pub fn to_url(&self) -> String {
        match &self.host {
            Host::Ip(IpAddr::V6(ip)) => format!("{SCHEME}[{ip}]:{}", self.port),
            Host::Ip(ip) => format!("{SCHEME}{ip}:{}", self.port),
            Host::Name(n) => format!("{SCHEME}{n}:{}", self.port),
        }
    }

    /// The socket address, when the host is an IP literal.
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        match &self.host {
            Host::Ip(ip) => Some(SocketAddr::new(*ip, self.port)),
            Host::Name(_) => None,
        }
    }
}

/// Whether the server may listen on `addr`. The person chose the address; an unspecified one
/// (every interface) is refused unless `allow_all` is set, which is the "Advanced" choice.
/// Multicast addresses and port 0 are always refused.
pub fn check_bind(addr: &SocketAddr, allow_all: bool) -> Result<(), AddrError> {
    if addr.port() == 0 {
        return Err(AddrError::Port);
    }
    let ip = addr.ip().to_canonical();
    if ip.is_multicast() || ip == IpAddr::V4(Ipv4Addr::BROADCAST) {
        return Err(AddrError::NotUnicast);
    }
    if ip.is_unspecified() && !allow_all {
        return Err(AddrError::Unspecified);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_addresses_round_trip() {
        for s in ["tcp://10.8.0.2:47300", "tcp://[fd00::2]:1", "tcp://office.example.test:65535", "tcp://127.0.0.1:9", "tcp://localhost:80"] {
            let a = PeerAddr::parse(s).unwrap();
            assert_eq!(a.to_url(), s);
            assert_eq!(PeerAddr::parse(&a.to_url()).unwrap(), a);
        }
        assert_eq!(PeerAddr::parse("tcp://OFFICE.Test:5").unwrap().to_url(), "tcp://office.test:5");
        assert_eq!(PeerAddr::parse("tcp://10.8.0.2:47300").unwrap().socket_addr(), Some("10.8.0.2:47300".parse().unwrap()));
        assert_eq!(PeerAddr::parse("tcp://office:1").unwrap().socket_addr(), None);
    }

    #[test]
    fn unspecified_multicast_and_malformed_are_refused() {
        assert_eq!(PeerAddr::parse("tcp://0.0.0.0:1"), Err(AddrError::Unspecified));
        assert_eq!(PeerAddr::parse("tcp://[::]:1"), Err(AddrError::Unspecified));
        assert_eq!(PeerAddr::parse("tcp://224.0.0.1:1"), Err(AddrError::NotUnicast));
        assert_eq!(PeerAddr::parse("tcp://255.255.255.255:1"), Err(AddrError::NotUnicast));
        assert_eq!(PeerAddr::parse("tcp://[ff02::1]:1"), Err(AddrError::NotUnicast));
        assert_eq!(PeerAddr::parse("tcp://10.0.0.1:0"), Err(AddrError::Port));
        for bad in [
            "",
            "10.0.0.1:1",
            "http://10.0.0.1:1",
            "tcp://",
            "tcp://10.0.0.1",
            "tcp://10.0.0.1:",
            "tcp://10.0.0.1:65536",
            "tcp://10.0.0.1:+1",
            "tcp://10.0.0.1:1/",
            "tcp://user@10.0.0.1:1",
            "tcp://10.0.0.1:1 ",
            "tcp://fd00::2:1",
            "tcp://[10.0.0.1]:1",
            "tcp://[fe80::1%eth0]:1",
            "tcp://1.2.3:1",
            "tcp://256.1.1.1:1",
            "tcp://-bad.test:1",
            "tcp://bad_name.test:1",
            "tcp://a..b:1",
            "tcp://\u{e9}.test:1",
        ] {
            assert!(PeerAddr::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn bind_refuses_all_interfaces_unless_asked() {
        let any4: SocketAddr = "0.0.0.0:47300".parse().unwrap();
        let any6: SocketAddr = "[::]:47300".parse().unwrap();
        assert_eq!(check_bind(&any4, false), Err(AddrError::Unspecified));
        assert_eq!(check_bind(&any6, false), Err(AddrError::Unspecified));
        assert_eq!(check_bind(&any4, true), Ok(()), "the Advanced choice");
        assert_eq!(check_bind(&"10.8.0.2:47300".parse().unwrap(), false), Ok(()));
        assert_eq!(check_bind(&"127.0.0.1:47300".parse().unwrap(), false), Ok(()));
        assert_eq!(check_bind(&"10.8.0.2:0".parse().unwrap(), false), Err(AddrError::Port));
        assert_eq!(check_bind(&"224.0.0.1:5".parse().unwrap(), true), Err(AddrError::NotUnicast));
        assert!(PeerAddr::from_socket(any4).is_err());
    }

    #[test]
    fn ipv4_mapped_ipv6_is_judged_as_ipv4() {
        for s in ["[::ffff:0.0.0.0]:47300", "[::ffff:224.0.0.1]:47300", "[::ffff:255.255.255.255]:47300"] {
            let sock: SocketAddr = s.parse().unwrap();
            assert!(check_bind(&sock, false).is_err(), "{s}");
            assert!(PeerAddr::parse(&format!("tcp://{s}")).is_err(), "{s}");
        }
        assert_eq!(check_bind(&"[::ffff:0.0.0.0]:47300".parse().unwrap(), true), Ok(()), "Advanced");
        assert_eq!(check_bind(&"[::ffff:224.0.0.1]:5".parse().unwrap(), true), Err(AddrError::NotUnicast));
        assert_eq!(check_bind(&"[::ffff:10.8.0.2]:47300".parse().unwrap(), false), Ok(()));
    }
}

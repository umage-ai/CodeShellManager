//! "Open the URL when the server is listening" (the WPF app's readiness watcher, `RunInstance`
//! commit 34d1ba0). A run command's URL is usually a dev server's (`npm run dev`), and a server
//! never exits, so "open it after exit 0" alone never opened it.
//!
//! While a run is alive, a worker probes a **loopback** URL's host and port every
//! [`POLL_INTERVAL`] for up to [`READY_TIMEOUT`]; the first accepted TCP connection opens the URL
//! (once; a successful exit still opens it for one-shot commands). Remote URLs are never probed
//! (a deploy target answers before the deploy is done), nor are SSH parents' runs (their server
//! listens on the remote machine's loopback). Every resolved address is tried at once: `localhost`
//! is both `::1` and `127.0.0.1`, a server usually binds one, and a refused loopback connect on
//! Windows takes about 2 s.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often a loopback URL is probed.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How long a run's URL is probed before giving up (the exit-0 rule still applies).
pub const READY_TIMEOUT: Duration = Duration::from_secs(120);
/// One connection attempt's limit.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Where to probe: a loopback host (a name or an address) and its port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
}

/// The loopback host and port of a launchable `http(s)` URL, or `None` (not launchable, or not
/// loopback). `localhost`, `127.0.0.0/8` and `[::1]` are loopback, and so are `0.0.0.0` and `[::]`,
/// which dev servers print (probed as `127.0.0.1` and `::1`). No port: 80 or 443.
pub fn loopback_target(url: &str) -> Option<Target> {
    let u = super::launchable_url(url)?;
    let lower = u.to_ascii_lowercase();
    let (rest, default_port) = match lower.strip_prefix("https://") {
        Some(r) => (r, 443),
        None => (lower.strip_prefix("http://")?, 80),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or("");
    let (host, port) = if let Some(after) = hostport.strip_prefix('[') {
        let (h, tail) = after.split_once(']')?;
        (h, tail.strip_prefix(':'))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    let port = match port {
        None | Some("") => default_port,
        Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0)?,
    };
    let host = host.trim_end_matches('.');
    let probe = if host == "localhost" {
        "localhost".to_owned()
    } else {
        match host.parse::<IpAddr>().ok()? {
            IpAddr::V4(a) if a.is_unspecified() => Ipv4Addr::LOCALHOST.to_string(),
            IpAddr::V6(a) if a.is_unspecified() => Ipv6Addr::LOCALHOST.to_string(),
            a if a.is_loopback() => a.to_string(),
            _ => return None,
        }
    };
    Some(Target { host: probe, port })
}

/// Whether something accepts a TCP connection at `t` on any of its addresses (tried at once).
/// Blocking (name resolution, connects): worker threads only.
pub fn is_listening(t: &Target) -> bool {
    let addrs: Vec<SocketAddr> = match (t.host.as_str(), t.port).to_socket_addrs() {
        Ok(a) => a.filter(|a| a.ip().is_loopback()).take(8).collect(),
        Err(_) => return false,
    };
    std::thread::scope(|s| {
        let tries: Vec<_> = addrs.iter().map(|a| s.spawn(move || TcpStream::connect_timeout(a, CONNECT_TIMEOUT).is_ok())).collect();
        tries.into_iter().any(|h| h.join().unwrap_or(false))
    })
}

/// Probes `t` every `interval` until it listens (true), `stop` is set, or `timeout` passes
/// (false). Blocking: worker threads only.
pub fn wait_until_listening(t: &Target, interval: Duration, timeout: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + timeout;
    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        if is_listening(t) {
            return !stop.load(Ordering::Acquire);
        }
        // Sleep in short steps so a stop is noticed quickly.
        let next = Instant::now() + interval;
        while Instant::now() < next {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50).min(interval));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;

    use super::*;

    fn t(host: &str, port: u16) -> Option<Target> {
        Some(Target { host: host.into(), port })
    }

    #[test]
    fn only_loopback_urls_have_a_target() {
        assert_eq!(loopback_target("http://localhost:5173"), t("localhost", 5173));
        assert_eq!(loopback_target("http://LOCALHOST:3000/app?x=1#y"), t("localhost", 3000));
        assert_eq!(loopback_target("http://localhost"), t("localhost", 80));
        assert_eq!(loopback_target("https://localhost/"), t("localhost", 443));
        assert_eq!(loopback_target("http://127.0.0.1:8080"), t("127.0.0.1", 8080));
        assert_eq!(loopback_target("http://127.4.5.6:81"), t("127.4.5.6", 81));
        assert_eq!(loopback_target("http://[::1]:5173/"), t("::1", 5173));
        assert_eq!(loopback_target("http://[::1]"), t("::1", 80));
        assert_eq!(loopback_target("http://0.0.0.0:4000"), t("127.0.0.1", 4000));
        assert_eq!(loopback_target("http://[::]:4000"), t("::1", 4000));
        assert_eq!(loopback_target("http://user@localhost:9000"), t("localhost", 9000));
        for remote in [
            "https://example.com",
            "http://10.0.0.5:3000",
            "http://192.168.1.2",
            "http://[2001:db8::1]:80",
            "http://localhost.example.com:3000",
            "http://mylocalhost:3000",
            "http://localhost:99999",
            "http://localhost:0",
            "http://localhost:x",
            "localhost:5173",
            "file:///C:/x",
            "",
        ] {
            assert_eq!(loopback_target(remote), None, "{remote}");
        }
    }

    #[test]
    fn a_listening_port_is_seen_and_a_closed_one_is_not() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(is_listening(&Target { host: "127.0.0.1".into(), port }));
        assert!(is_listening(&Target { host: "localhost".into(), port }), "localhost tries 127.0.0.1 too, not only ::1");
        drop(l);
        assert!(!is_listening(&Target { host: "127.0.0.1".into(), port }));
        assert!(!is_listening(&Target { host: "no-such-host.invalid".into(), port }));
    }

    #[test]
    fn the_watcher_fires_once_the_server_starts_listening() {
        // Reserve a port, free it, and bind it again a little later (the server starting).
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let target = Target { host: "localhost".into(), port };
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
            std::thread::sleep(Duration::from_secs(5));
            drop(l);
        });
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        assert!(wait_until_listening(&target, Duration::from_millis(100), Duration::from_secs(10), &stop));
        assert!(started.elapsed() < Duration::from_secs(5));
        server.join().unwrap();
    }

    #[test]
    fn the_watcher_gives_up_on_stop_or_timeout() {
        // Port 0 never accepts a connection, unlike a freed ephemeral port that a parallel test
        // may bind again (this test was flaky with one).
        let port = 0;
        let target = Target { host: "127.0.0.1".into(), port };
        let stop = AtomicBool::new(false);
        assert!(!wait_until_listening(&target, Duration::from_millis(50), Duration::from_millis(300), &stop));
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let h = std::thread::spawn(move || wait_until_listening(&target, Duration::from_millis(50), Duration::from_secs(60), &s2));
        std::thread::sleep(Duration::from_millis(200));
        stop.store(true, Ordering::Release);
        let started = Instant::now();
        assert!(!h.join().unwrap());
        assert!(started.elapsed() < Duration::from_secs(5), "a stop ends the polling promptly");
    }
}

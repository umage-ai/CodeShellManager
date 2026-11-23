//! The host's outbound connection to the relay: one WebSocket, `wss://` (TLS, with the public roots
//! bundled in `webpki-roots`, so a relay behind a public certificate works the same on every PC) or
//! plain `ws://` to a loopback relay (the development setup). Reconnects with the backoff of
//! `relay/README.md` (1, 2, 5, 10, 30 seconds).
//!
//! One thread does everything: it reads with a one-second timeout, runs the time-driven work,
//! applies the person's commands (start or stop pairing, allow or deny, revoke, change level) and
//! writes what [`Host`] asks for. After every connect it sends the allow list again, and the open
//! pairing slot, because the relay forgets both when the host disconnects.
//!
//! The connect and the handshake each have a timeout, so a relay that accepts the TCP connection
//! and then says nothing cannot hold the thread (and so cannot stop the add-on from stopping).
//!
//! A relay URL that is neither `wss://` nor loopback `ws://` is refused here with a reason, never
//! silently downgraded to plain text.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use csm_remote_proto::msg::Level;
use csm_remote_proto::relay::{HostToRelay, RelayToHost, to_text};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, Message, WebSocket};

use crate::config::is_loopback_ws_relay;
use crate::host::{DeviceView, Host, Out};

pub const BACKOFF_SECS: [u64; 5] = [1, 2, 5, 10, 30];
pub const PING_SECS: u64 = 30;
/// A connection that lasted this long counts as healthy: the backoff starts again.
const HEALTHY_SECS: u64 = 60;
/// The TCP connect and the WebSocket handshake each give up after this long.
pub const CONNECT_TIMEOUT_SECS: u64 = 5;
/// Socket read timeout once connected: the loop wakes this often to run time-driven work and to
/// send the answers the ops worker has finished.
const POLL_MS: u64 = 100;

/// What the person asked for, sent from the tool handlers to the thread that owns the host. The
/// ones that answer carry the sender for the answer.
#[derive(Debug)]
pub enum Command {
    /// Open the pairing slot. `app_version` goes into the offer link; the reply is the link.
    StartPairing { app_version: String, reply: Sender<Result<String, String>> },
    /// Close the slot; a waiting phone is denied.
    StopPairing,
    /// The person's answer for the waiting phone: `Some(level)` allows it, `None` denies it.
    Decide(Option<Level>),
    Revoke(String),
    SetLevel(String, Level),
    /// The paired devices, for the devices screen and `phone-remote.status`.
    Devices(Sender<Vec<DeviceView>>),
}

/// Unix seconds now.
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Runs until `stop` is set. `on_out` receives what is not a frame or a control message (the offer
/// link, pending pairings, problems, devices changed), so the screen and the log can show it.
/// Commands wait in `commands` while the relay is unreachable and are applied after a reconnect.
pub fn run(host: &mut Host, url: &str, secret: &[u8; 32], stop: &AtomicBool, commands: &Receiver<Command>, on_out: &mut dyn FnMut(&Out)) {
    if !url.starts_with("wss://") && !is_loopback_ws_relay(url) {
        on_out(&Out::Problem("the relay must be wss://, or ws:// to 127.0.0.1 for a local test; the relay is not used".into()));
        return;
    }
    let socket_url = host_socket_url(url);
    let mut attempt = 0usize;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        match session(host, &socket_url, secret, stop, commands, on_out) {
            Ended::Stopped => return,
            Ended::Lost(why) => on_out(&Out::Problem(why)),
        }
        if started.elapsed() >= Duration::from_secs(HEALTHY_SECS) {
            attempt = 0;
        }
        let wait = BACKOFF_SECS.get(attempt).copied().unwrap_or(30);
        attempt = attempt.saturating_add(1);
        offline(host, stop, commands, Duration::from_secs(wait), on_out);
    }
}

/// The add-on's loop when it must not connect (not enabled, or no usable relay): nothing is dialled,
/// but the person's commands are still answered (devices, revoke, level). Pairing is refused by
/// [`Host::start_pairing`] because no relay is set.
pub fn serve_offline(host: &mut Host, stop: &AtomicBool, commands: &Receiver<Command>, on_out: &mut dyn FnMut(&Out)) {
    while !stop.load(Ordering::SeqCst) {
        offline(host, stop, commands, Duration::from_millis(500), on_out);
    }
}

enum Ended {
    Stopped,
    Lost(String),
}

/// Waits for `total` (or until `stop`) while applying the person's commands as they come. A
/// command that needs the relay (a decision, a pairing) is still applied: the host refuses what it
/// cannot do, and the reply says so.
fn offline(host: &mut Host, stop: &AtomicBool, commands: &Receiver<Command>, total: Duration, on_out: &mut dyn FnMut(&Out)) {
    let until = Instant::now() + total;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        match commands.recv_timeout(Duration::from_millis(200)) {
            Ok(cmd) => {
                // Frames and control messages are not sent while there is no connection; the
                // next connect sends the allow list and the open slot from the host's state.
                for out in apply(host, &cmd, now_secs(), on_out) {
                    if !matches!(out, Out::Frame(_) | Out::Control(_)) {
                        on_out(&out);
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

/// The socket a host connects to, from the relay's base URL (`ws://127.0.0.1:port`, the same base
/// the pairing offer carries): the relay serves hosts at `/v1/host`.
pub fn host_socket_url(relay: &str) -> String {
    let base = relay.trim_end_matches('/');
    if base.ends_with("/v1/host") { base.to_owned() } else { format!("{base}/v1/host") }
}

/// The TLS settings for `wss://`: the ring provider, named here so that no process-wide default is
/// needed, and the public roots bundled in `webpki-roots`. A self-hosted relay needs a certificate
/// from a public CA (the relay's Caddy setup gets one); a private CA is not trusted.
fn tls_connector() -> Result<Connector, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| "the TLS settings could not be made".to_owned())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}

/// The TCP connect with a timeout, then the WebSocket handshake (TLS first for `wss://`) with the
/// same timeout. Only `wss://` and loopback `ws://` URLs get here (checked by the caller).
fn connect(url: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, String> {
    let (tls, rest) = match (url.strip_prefix("wss://"), url.strip_prefix("ws://")) {
        (Some(r), _) => (true, r),
        (None, Some(r)) => (false, r),
        (None, None) => return Err("the relay address is not usable".to_owned()),
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let default_port = if tls { 443 } else { 80 };
    let with_port = if authority.contains(':') { authority.to_owned() } else { format!("{authority}:{default_port}") };
    let timeout = Duration::from_secs(CONNECT_TIMEOUT_SECS);
    let addr = with_port.to_socket_addrs().ok().and_then(|mut it| it.next()).ok_or_else(|| "the relay address is not usable".to_owned())?;
    let tcp = TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("the relay did not accept the connection: {e}"))?;
    tcp.set_read_timeout(Some(timeout)).map_err(|_| "the socket could not be set up".to_owned())?;
    tcp.set_write_timeout(Some(timeout)).map_err(|_| "the socket could not be set up".to_owned())?;
    // `wss://` gets the TLS connector (certificates checked against the bundled public roots); `ws://`
    // (loopback only) is a plain WebSocket handshake.
    let connector = if tls { Some(tls_connector()?) } else { None };
    let (mut ws, _response) = tungstenite::client_tls_with_config(url, tcp, None, connector).map_err(|e| format!("the relay did not complete the handshake: {e}"))?;
    let read = Duration::from_millis(POLL_MS);
    match ws.get_mut() {
        MaybeTlsStream::Plain(tcp) => tcp.set_read_timeout(Some(read)),
        MaybeTlsStream::Rustls(tls) => tls.get_ref().set_read_timeout(Some(read)),
        _ => Ok(()),
    }
    .map_err(|_| "the socket could not be set up".to_owned())?;
    Ok(ws)
}

fn session(host: &mut Host, url: &str, secret: &[u8; 32], stop: &AtomicBool, commands: &Receiver<Command>, on_out: &mut dyn FnMut(&Out)) -> Ended {
    let mut ws = match connect(url) {
        Ok(ws) => ws,
        Err(why) => return Ended::Lost(why),
    };
    let hello = HostToRelay::Hello {
        v: csm_remote_proto::PROTOCOL_VERSION,
        host_id: host.host_id.clone(),
        host_secret: csm_remote_proto::ids::b64_encode(secret),
        access: None,
    };
    let mut pending = vec![Out::Control(hello), Out::Control(HostToRelay::Allow { tokens: host.allow_tokens() })];
    if let Some(slot) = host.slot.as_ref() {
        pending.push(Out::Control(HostToRelay::PairingOpen { pairing_id: csm_remote_proto::ids::b64_encode(&slot.pairing_id), expires_in: slot.seconds_left(now_secs()) }));
    }
    if let Err(e) = send_all(&mut ws, pending, on_out) {
        return Ended::Lost(e);
    }
    let mut last_ping = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = ws.close(None);
            return Ended::Stopped;
        }
        if last_ping.elapsed() >= Duration::from_secs(PING_SECS) {
            last_ping = Instant::now();
            if ws.send(Message::Ping(Vec::new().into())).is_err() {
                return Ended::Lost("the relay connection failed".into());
            }
        }
        let now = now_secs();
        let mut outs = host.tick(now);
        loop {
            match commands.try_recv() {
                Ok(cmd) => outs.extend(apply(host, &cmd, now, on_out)),
                Err(TryRecvError::Empty) => break,
                // The owner dropped its sender: no more commands, but the relay still runs.
                Err(TryRecvError::Disconnected) => break,
            }
        }
        if let Err(e) = send_all(&mut ws, outs, on_out) {
            return Ended::Lost(e);
        }
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                let outs = host.on_frame(&bytes, now);
                if let Err(e) = send_all(&mut ws, outs, on_out) {
                    return Ended::Lost(e);
                }
            }
            Ok(Message::Text(text)) => {
                if let Ok(msg) = serde_json::from_str::<RelayToHost>(&text) {
                    let outs = host.on_relay(&msg);
                    if let Err(e) = send_all(&mut ws, outs, on_out) {
                        return Ended::Lost(e);
                    }
                }
            }
            Ok(Message::Close(_)) => return Ended::Lost("the relay closed the connection".into()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Ended::Lost(format!("the relay connection failed: {e}")),
        }
    }
}

/// One of the person's commands, applied to the host. Its frames and control messages are returned
/// for sending; a failure is reported through `on_out`.
fn apply(host: &mut Host, cmd: &Command, now: u64, on_out: &mut dyn FnMut(&Out)) -> Vec<Out> {
    match cmd {
        Command::StartPairing { app_version, reply } => match host.start_pairing(now, app_version) {
            Ok((url, outs)) => {
                let _ = reply.send(Ok(url.clone()));
                on_out(&Out::PairingUrl(url));
                outs
            }
            Err(why) => {
                let _ = reply.send(Err(why));
                Vec::new()
            }
        },
        Command::StopPairing => host.stop_pairing(now),
        Command::Decide(level) => host.decide(now, *level),
        Command::Revoke(device_id) => host.revoke(device_id),
        Command::SetLevel(device_id, level) => host.set_level(device_id, *level),
        Command::Devices(reply) => {
            let _ = reply.send(host.devices_view());
            Vec::new()
        }
    }
}

fn send_all(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, outs: Vec<Out>, on_out: &mut dyn FnMut(&Out)) -> Result<(), String> {
    for out in outs {
        match &out {
            Out::Frame(bytes) => ws.send(Message::Binary(bytes.clone().into())).map_err(|e| format!("the relay connection failed: {e}"))?,
            Out::Control(c) => ws.send(Message::Text(to_text(c).into())).map_err(|e| format!("the relay connection failed: {e}"))?,
            _ => on_out(&out),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::config::Config;
    use crate::csm::{Csm, CsmError};
    use crate::store;
    use serde_json::Value;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    struct NoCsm;
    impl Csm for NoCsm {
        fn call(&self, _tool: &str, _params: Value) -> Result<Value, CsmError> {
            Err(CsmError::Unavailable)
        }
    }

    fn host_at(dir: &std::path::Path, relay: &str) -> Host {
        let opened = store::open(dir).unwrap();
        let cfg = Config { relay: relay.into(), app_origin: "http://127.0.0.1:8080".into(), connect: true, ..Config::default() };
        Host::new(dir.to_path_buf(), opened, cfg, &[5; 32], Box::new(NoCsm)).unwrap()
    }

    /// A relay that accepts two connections, records each hello and allow list, then drops the
    /// first one. The host must come back on its own.
    #[test]
    fn the_host_reconnects_after_the_relay_drops_it_and_sends_its_allow_list_again() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hellos = Arc::new(AtomicUsize::new(0));
        let allows = Arc::new(AtomicUsize::new(0));
        let (h2, a2) = (Arc::clone(&hellos), Arc::clone(&allows));
        let relay = std::thread::spawn(move || {
            for round in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut ws = tungstenite::accept(stream).unwrap();
                h2.fetch_add(1, Ordering::SeqCst);
                for _ in 0..2 {
                    if let Ok(Message::Text(t)) = ws.read() {
                        let v: Value = serde_json::from_str(&t).unwrap();
                        if v["t"] == "allow" {
                            a2.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
                if round == 0 {
                    drop(ws);
                } else {
                    let _ = ws.close(None);
                    break;
                }
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let mut host = host_at(dir.path(), &format!("ws://127.0.0.1:{port}"));
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let watchdog = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            stop2.store(true, Ordering::SeqCst);
        });
        let url = format!("ws://127.0.0.1:{port}");
        let stop_when_done = Arc::clone(&stop);
        let mut problems = Vec::new();
        let (_tx, rx) = mpsc::channel();
        run(&mut host, &url, &[5; 32], &stop, &rx, &mut |o| {
            if let Out::Problem(p) = o {
                problems.push(p.clone());
            }
            if hellos.load(Ordering::SeqCst) >= 2 && allows.load(Ordering::SeqCst) >= 2 {
                stop_when_done.store(true, Ordering::SeqCst);
            }
        });
        relay.join().unwrap();
        drop(watchdog);
        assert_eq!(hellos.load(Ordering::SeqCst), 2, "the host connected again after the drop");
        assert_eq!(allows.load(Ordering::SeqCst), 2, "the allow list is sent after every connect");
        assert!(problems.iter().any(|p| p.contains("relay closed") || p.contains("relay connection failed")), "the drop is reported: {problems:?}");
    }

    /// A relay that accepts the TCP connection and never answers the handshake must not hold the
    /// host: it gives up within the timeout and stop takes effect.
    #[test]
    fn a_relay_that_never_completes_the_handshake_is_given_up_on() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let silent = std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().take(2).flatten() {
                held.push(stream);
            }
            std::thread::sleep(Duration::from_secs(CONNECT_TIMEOUT_SECS + 3));
            drop(held);
        });
        let dir = tempfile::tempdir().unwrap();
        let mut host = host_at(dir.path(), &format!("ws://127.0.0.1:{port}"));
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let mut problems = Vec::new();
        let (_tx, rx) = mpsc::channel();
        run(&mut host, &format!("ws://127.0.0.1:{port}"), &[5; 32], &stop, &rx, &mut |o| {
            if let Out::Problem(p) = o {
                problems.push(p.clone());
                stop.store(true, Ordering::SeqCst);
            }
        });
        assert!(started.elapsed() < Duration::from_secs(CONNECT_TIMEOUT_SECS + 2), "the timeout held: {:?}", started.elapsed());
        assert!(problems.iter().any(|p| p.contains("handshake")), "reported: {problems:?}");
        drop(silent);
    }

    #[test]
    fn the_host_socket_is_the_relay_base_with_the_host_path() {
        assert_eq!(host_socket_url("ws://127.0.0.1:9000"), "ws://127.0.0.1:9000/v1/host");
        assert_eq!(host_socket_url("ws://127.0.0.1:9000/"), "ws://127.0.0.1:9000/v1/host");
        assert_eq!(host_socket_url("ws://127.0.0.1:9000/v1/host"), "ws://127.0.0.1:9000/v1/host", "not doubled");
    }

    /// `wss://` really negotiates TLS: a peer that answers with plain HTTP fails the TLS handshake,
    /// which is reported as a problem and given up on within the timeouts (the URL is not
    /// downgraded to plain text, and nothing panics).
    #[test]
    fn a_wss_relay_that_is_not_tls_is_reported_and_given_up_on() {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let url = format!("wss://127.0.0.1:{port}");
        let dir = tempfile::tempdir().unwrap();
        let mut host = host_at(dir.path(), &url);
        let stop = AtomicBool::new(false);
        let (_tx, rx) = mpsc::channel();
        let mut problems = Vec::new();
        let started = Instant::now();
        run(&mut host, &url, &[5; 32], &stop, &rx, &mut |o| {
            if let Out::Problem(p) = o {
                problems.push(p.clone());
                stop.store(true, Ordering::SeqCst);
            }
        });
        assert!(started.elapsed() < Duration::from_secs(CONNECT_TIMEOUT_SECS + 3), "given up on in time: {:?}", started.elapsed());
        assert!(problems.iter().any(|p| p.contains("handshake")), "the TLS failure is reported: {problems:?}");
        assert!(host.slot.is_none());
    }

    #[test]
    fn a_remote_plain_relay_is_refused_even_with_a_loopback_look_alike() {
        for url in ["ws://127.0.0.1.evil.example:9000/v1/host", "ws://127.0.0.1@evil.example/v1/host"] {
            let dir = tempfile::tempdir().unwrap();
            let mut host = host_at(dir.path(), url);
            let stop = AtomicBool::new(false);
            let mut seen = Vec::new();
            let (_tx, rx) = mpsc::channel();
            run(&mut host, url, &[5; 32], &stop, &rx, &mut |o| seen.push(o.clone()));
            assert!(seen.iter().any(|o| matches!(o, Out::Problem(_))), "{url} must be refused: {seen:?}");
            assert!(host.slot.is_none());
        }
    }
}

//! The host's outbound connection to the relay: one WebSocket, plain `ws://` only (the loopback
//! development setup). Reconnects with the backoff of `relay/README.md` (1, 2, 5, 10, 30 seconds).
//!
//! One thread does everything: it reads with a one-second timeout, runs the time-driven work and
//! writes what [`Host`] asks for. After every connect it sends the allow list again, and the open
//! pairing slot, because the relay forgets both when the host disconnects.
//!
//! Not built: `wss://` (needs a TLS feature of `tungstenite`, an owner decision on the crate set).
//! A production relay URL is refused here with a reason, not silently downgraded.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use csm_remote_proto::relay::{HostToRelay, RelayToHost, to_text};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::host::{Host, Out};

pub const BACKOFF_SECS: [u64; 5] = [1, 2, 5, 10, 30];
pub const PING_SECS: u64 = 30;
/// A connection that lasted this long counts as healthy: the backoff starts again.
const HEALTHY_SECS: u64 = 60;

/// Unix seconds now.
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Runs until `stop` is set. `on_out` receives what is not a frame or a control message (pending
/// pairings, problems, devices changed), so the screen and the log can show it.
pub fn run(host: &mut Host, url: &str, secret: &[u8; 32], stop: &AtomicBool, on_out: &mut dyn FnMut(&Out)) {
    if !url.starts_with("ws://") {
        on_out(&Out::Problem("only ws:// relays are supported in this build".into()));
        return;
    }
    let mut attempt = 0usize;
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        match session(host, url, secret, stop, on_out) {
            Ended::Stopped => return,
            Ended::Lost(why) => on_out(&Out::Problem(why)),
        }
        if started.elapsed() >= Duration::from_secs(HEALTHY_SECS) {
            attempt = 0;
        }
        let wait = BACKOFF_SECS.get(attempt).copied().unwrap_or(30);
        attempt = attempt.saturating_add(1);
        sleep_unless_stopped(Duration::from_secs(wait), stop);
    }
}

enum Ended {
    Stopped,
    Lost(String),
}

fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) {
    let until = Instant::now() + total;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn session(host: &mut Host, url: &str, secret: &[u8; 32], stop: &AtomicBool, on_out: &mut dyn FnMut(&Out)) -> Ended {
    let (mut ws, _response) = match tungstenite::connect(url) {
        Ok(pair) => pair,
        Err(e) => return Ended::Lost(format!("the relay did not accept the connection: {e}")),
    };
    if let MaybeTlsStream::Plain(tcp) = ws.get_ref()
        && tcp.set_read_timeout(Some(Duration::from_secs(1))).is_err()
    {
        return Ended::Lost("the socket could not be set up".into());
    }
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
        let outs = host.tick(now);
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
                    on_relay_message(&msg, on_out);
                }
            }
            Ok(Message::Close(_)) => return Ended::Lost("the relay closed the connection".into()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Ended::Lost(format!("the relay connection failed: {e}")),
        }
    }
}

/// What the relay says back. Refusals are reported; nothing here changes host state.
fn on_relay_message(msg: &RelayToHost, on_out: &mut dyn FnMut(&Out)) {
    match msg {
        RelayToHost::Welcome { .. } | RelayToHost::AllowOk { .. } | RelayToHost::PairingOk { .. } | RelayToHost::PushResult { .. } => {}
        RelayToHost::Error { code } => on_out(&Out::Problem(format!("the relay refused a request ({})", code.as_str()))),
    }
}

fn send_all(ws: &mut WebSocket<MaybeTlsStream<std::net::TcpStream>>, outs: Vec<Out>, on_out: &mut dyn FnMut(&Out)) -> Result<(), String> {
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

    struct NoCsm;
    impl Csm for NoCsm {
        fn call(&self, _tool: &str, _params: Value) -> Result<Value, CsmError> {
            Err(CsmError::Unavailable)
        }
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
        let opened = store::open(dir.path()).unwrap();
        let cfg = Config { relay: format!("ws://127.0.0.1:{port}"), app_origin: "http://127.0.0.1:8080".into(), connect: true, ..Config::default() };
        let mut host = Host::new(dir.path().to_path_buf(), opened, cfg, &[5; 32], Box::new(NoCsm)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let watchdog = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(20));
            stop2.store(true, Ordering::SeqCst);
        });
        let url = format!("ws://127.0.0.1:{port}");
        let stop_when_done = Arc::clone(&stop);
        let mut problems = Vec::new();
        run(&mut host, &url, &[5; 32], &stop, &mut |o| {
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

    #[test]
    fn a_wss_relay_is_refused_with_a_reason_in_this_build() {
        let dir = tempfile::tempdir().unwrap();
        let opened = store::open(dir.path()).unwrap();
        let mut host = Host::new(dir.path().to_path_buf(), opened, Config::default(), &[5; 32], Box::new(NoCsm)).unwrap();
        let stop = AtomicBool::new(false);
        let mut seen = Vec::new();
        run(&mut host, "wss://relay.example/v1/host", &[5; 32], &stop, &mut |o| seen.push(o.clone()));
        assert!(seen.iter().any(|o| matches!(o, Out::Problem(p) if p.contains("ws://"))));
    }
}

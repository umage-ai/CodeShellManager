//! The phone remote end to end, with no hosted service: `csm-relay` runs in process on 127.0.0.1, the
//! add-on's host runs its relay client against it, and the phone web app's own protocol code
//! (`phone/js`: pairing, the session channel, the Noise code) runs under Node 22+ as a separate
//! process, driven from here one command at a time (`phone/test/e2e/driver.mjs`).
//!
//! The story: a viewer pairs and reads the list; a viewer is refused `session.wake`; an operator
//! pairs and wakes a session, and both phones see the change as a delta; the operator subscribes to
//! Web Push with a fake endpoint (the host keeps it; no push service is contacted); the operator is
//! revoked and its channel ends with `revoked`, the relay refuses it after, and the viewer still
//! works.
//!
//! The fake CSM is stateful for the one session it has. Nothing listens beyond 127.0.0.1.
//!
//! Ignored by default because it needs Node 22 or later. Run it with `cargo xtask phone-e2e`
//! (or `cargo test -p csm-phone-it --test e2e -- --ignored`). `CSM_NODE` names another node binary.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command as Process, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_phone::config::Config;
use csm_phone::csm::{Csm, CsmError};
use csm_phone::host::{Host, Out};
use csm_phone::relay_client::{self, Command};
use csm_phone::store;
use csm_relay::config::Config as RelayConfig;
use csm_relay::{Relay, serve};
use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::msg::Level;
use serde_json::{Value, json};

const SECRET: [u8; 32] = [9; 32];
const WAIT: Duration = Duration::from_secs(20);
const PAGE_ORIGIN: &str = "http://127.0.0.1:8080";
const FAKE_PUSH_ENDPOINT: &str = "https://updates.push.services.mozilla.com/wpush/v2/e2e-fake-subscription";

// ---- the relay, in process -----------------------------------------------------------------------

fn open_relay_config() -> RelayConfig {
    let mut c = RelayConfig::default();
    c.access.open = true;
    c.limits.new_connections_per_ip_per_minute = 1000;
    c.limits.max_sockets_per_ip = 1000;
    c
}

/// A relay on 127.0.0.1 on its own thread and runtime. `stop` closes every socket first.
struct RelayRig {
    addr: SocketAddr,
    relay: Arc<Relay>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RelayRig {
    fn start() -> RelayRig {
        let (relay_tx, relay_rx) = mpsc::channel::<(Arc<Relay>, SocketAddr)>();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            rt.block_on(async move {
                let relay = Relay::new(open_relay_config().into_settings().unwrap(), None);
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                let addr = listener.local_addr().unwrap();
                let _ = relay_tx.send((Arc::clone(&relay), addr));
                let _ = serve(listener, relay, async {
                    let _ = stop_rx.await;
                })
                .await;
            });
        });
        let (relay, addr) = relay_rx.recv_timeout(WAIT).expect("the relay starts");
        RelayRig { addr, relay, stop: Some(stop_tx) }
    }

    fn base(&self) -> String {
        format!("ws://127.0.0.1:{}", self.addr.port())
    }

    fn stop(mut self) {
        self.relay.shutdown_all(RefusalCode::HostOffline);
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

// ---- the add-on's host, in process ---------------------------------------------------------------

/// A stateful CSM with one session. `session_wake` makes it idle; the list shows the change.
#[derive(Default)]
struct FakeCsm {
    status: Mutex<String>,
}

impl FakeCsm {
    fn new() -> FakeCsm {
        FakeCsm { status: Mutex::new("asleep".to_owned()) }
    }
}

impl Csm for FakeCsm {
    fn call(&self, tool: &str, _params: Value) -> Result<Value, CsmError> {
        match tool {
            "session_list" => {
                let status = self.status.lock().unwrap().clone();
                Ok(json!([{"id": "s1", "name": "Work", "folder": "/work/app", "status": status}]))
            }
            "session_wake" => {
                *self.status.lock().unwrap() = "idle".to_owned();
                Ok(json!({}))
            }
            _ => Ok(json!({})),
        }
    }
}

struct HostRig {
    cmd: Sender<Command>,
    outs: Receiver<Out>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

fn start_host(relay: &str, dir: &std::path::Path) -> HostRig {
    let opened = store::open(dir).unwrap();
    let cfg = Config { relay: relay.to_owned(), app_origin: PAGE_ORIGIN.into(), connect: true, ..Config::default() };
    let mut host = Host::new(dir.to_path_buf(), opened, cfg, &SECRET, Box::new(FakeCsm::new())).unwrap();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (out_tx, out_rx) = mpsc::channel::<Out>();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let url = relay.to_owned();
    std::thread::spawn(move || {
        relay_client::run(&mut host, &url, &SECRET, &thread_stop, &cmd_rx, &mut |o| {
            let _ = out_tx.send(o.clone());
        });
    });
    HostRig { cmd: cmd_tx, outs: out_rx, stop }
}

impl Drop for HostRig {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl HostRig {
    fn start_pairing(&self) -> String {
        let (tx, rx) = mpsc::channel();
        self.cmd.send(Command::StartPairing { app_version: "0.1.0".into(), reply: tx }).unwrap();
        rx.recv_timeout(WAIT).expect("the host answers").expect("pairing opens")
    }

    /// The next output `want` accepts; the others are skipped.
    fn wait_out(&self, mut want: impl FnMut(&Out) -> bool) -> Out {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.outs.recv_timeout(left) {
                Ok(o) if want(&o) => return o,
                Ok(_) => {}
                Err(_) => panic!("the host did not report it in time"),
            }
        }
    }

    /// The device id of the paired phone called `name`.
    fn device_id(&self, name: &str) -> String {
        let (tx, rx) = mpsc::channel();
        self.cmd.send(Command::Devices(tx)).unwrap();
        let devices = rx.recv_timeout(WAIT).expect("the host answers");
        devices.into_iter().find(|d| d.name == name).map(|d| d.device_id).expect("the phone is paired")
    }

    fn push_on(&self, name: &str) -> bool {
        let (tx, rx) = mpsc::channel();
        self.cmd.send(Command::Devices(tx)).unwrap();
        let devices = rx.recv_timeout(WAIT).expect("the host answers");
        devices.iter().find(|d| d.name == name).is_some_and(|d| d.push)
    }
}

// ---- the phone web app, under Node ---------------------------------------------------------------

/// The Node process running `phone/test/e2e/driver.mjs`. Each `call` sends one command and reads one
/// reply line; the reader runs on its own thread so a hang fails the test instead of blocking it.
struct Driver {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Driver {
    fn start(relay: &str) -> Driver {
        let node = std::env::var("CSM_NODE").unwrap_or_else(|_| "node".to_owned());
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../phone/test/e2e/driver.mjs");
        let mut child = Process::new(&node)
            .arg(&script)
            .env("RELAY", relay)
            .env("PAGE_ORIGIN", PAGE_ORIGIN)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("could not start `{node}` (Node 22 or later is needed): {e}"));
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, lines) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Driver { child, stdin, lines }
    }

    fn send(&mut self, cmd: &Value) {
        writeln!(self.stdin, "{cmd}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        let line = self.lines.recv_timeout(WAIT).expect("the driver replies");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("the driver replied with a non-JSON line ({e}): {line}"))
    }

    fn call(&mut self, cmd: Value) -> Value {
        self.send(&cmd);
        self.recv()
    }

    /// Pairs a phone: the phone sends its check and name, the PC allows it at `level`.
    fn pair(&mut self, host: &HostRig, name: &str, link: &str, level: Level) -> Value {
        self.send(&json!({"do": "pair", "name": name, "offer": link}));
        let Out::Pending { name: shown, .. } = host.wait_out(|o| matches!(o, Out::Pending { .. })) else { unreachable!() };
        assert_eq!(shown, name, "the PC shows the name the phone sent");
        host.cmd.send(Command::Decide(Some(level))).unwrap();
        self.recv()
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "needs Node 22 or later: run with `cargo xtask phone-e2e`"]
fn the_phone_web_app_pairs_operates_and_is_revoked_through_the_real_relay() {
    let dir = tempfile::tempdir().unwrap();
    let relay = RelayRig::start();
    let host = start_host(&relay.base(), dir.path());
    let mut phone = Driver::start(&relay.base());

    // A viewer pairs, reads the list, and is refused an operator op.
    let link = host.start_pairing();
    let paired = phone.pair(&host, "Viewer phone", &link, Level::Viewer);
    assert_eq!(paired["ok"], true, "{paired}");
    assert_eq!(paired["level"], "viewer");

    let viewer = phone.call(json!({"do": "connect", "name": "Viewer phone"}));
    assert_eq!(viewer["ok"], true, "{viewer}");
    assert_eq!(viewer["sessions"][0]["id"], "s1");
    assert_eq!(viewer["sessions"][0]["status"], "asleep");

    let refused = phone.call(json!({"do": "request", "name": "Viewer phone", "op": "session.wake", "args": {"session_id": "s1"}}));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["code"], "forbidden", "a viewer may not wake a session: {refused}");

    // An operator pairs, wakes the session, and both phones see the change as a delta.
    let link = host.start_pairing();
    let paired = phone.pair(&host, "Operator phone", &link, Level::Operator);
    assert_eq!(paired["ok"], true, "{paired}");
    assert_eq!(paired["level"], "operator");

    let operator = phone.call(json!({"do": "connect", "name": "Operator phone"}));
    assert_eq!(operator["ok"], true, "{operator}");

    let woken = phone.call(json!({"do": "request", "name": "Operator phone", "op": "session.wake", "args": {"session_id": "s1"}}));
    assert_eq!(woken["ok"], true, "{woken}");

    let seen_by_operator = phone.call(json!({"do": "wait_session", "name": "Operator phone", "id": "s1", "status": "idle"}));
    assert_eq!(seen_by_operator["ok"], true, "the operator sees the change: {seen_by_operator}");
    let seen_by_viewer = phone.call(json!({"do": "wait_session", "name": "Viewer phone", "id": "s1", "status": "idle"}));
    assert_eq!(seen_by_viewer["ok"], true, "the viewer sees the change: {seen_by_viewer}");

    // Web Push: a subscription on an allowed push service is kept; one off the list is refused.
    let subscribed = phone.call(json!({"do": "push_subscribe", "name": "Operator phone", "endpoint": FAKE_PUSH_ENDPOINT}));
    assert_eq!(subscribed["ok"], true, "{subscribed}");
    assert!(host.push_on("Operator phone"), "the host keeps the subscription");
    let off_list = phone.call(json!({"do": "push_subscribe", "name": "Operator phone", "endpoint": "https://evil.example/push"}));
    assert_eq!(off_list["ok"], false, "{off_list}");
    assert_eq!(off_list["code"], "bad_request");

    // Revoke the operator: its channel ends with `revoked`, and the relay refuses it after.
    let operator_id = host.device_id("Operator phone");
    phone.send(&json!({"do": "expect_bye", "name": "Operator phone"}));
    host.cmd.send(Command::Revoke(operator_id)).unwrap();
    let bye = phone.recv();
    assert_eq!(bye["ok"], true, "{bye}");
    assert_eq!(bye["reason"], "revoked", "{bye}");

    let after = phone.call(json!({"do": "connect", "name": "Operator phone"}));
    assert_eq!(after["ok"], false, "a revoked phone cannot connect again: {after}");

    // The viewer is unaffected by the revoke of another phone.
    let still = phone.call(json!({"do": "request", "name": "Viewer phone", "op": "session.list"}));
    assert_eq!(still["ok"], true, "{still}");

    let quit = phone.call(json!({"do": "quit"}));
    assert_eq!(quit["ok"], true);
    drop(host);
    relay.stop();
}

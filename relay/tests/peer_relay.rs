#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A CSM peer through the relay, in one process: the real relay on 127.0.0.1, the real peer server
//! (the office) and the real client (the laptop). Both reach the relay outbound only. The direct
//! address in the offer is made unreachable, so the laptop can only get in through the relay.

use std::net::SocketAddr;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, PeerHost, State, StateOptions};
use csm_peer::client::{self, ClientConfig, Connection, Incoming, PairedHost};
use csm_peer::identity::Identity;
use csm_peer::{PeerServer, RelayConfig, ServerConfig};
use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::msg::{Level, Outcome};
use csm_peer_proto::pairing::{PeerOffer, RelayRoute};
use csm_relay::config::Config;
use csm_relay::{Relay, serve};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);
/// The office's relay identity: its secret proves the relay host id.
const OFFICE_RELAY_SECRET: [u8; 32] = [0x42; 32];

/// The relay on an ephemeral loopback port, served on its own runtime thread.
struct RelayRig {
    addr: SocketAddr,
    _stop: mpsc::Sender<()>,
    _relay: Arc<Relay>,
}

fn start_relay() -> RelayRig {
    let mut c = Config::default();
    c.access.open = true;
    c.limits.new_connections_per_ip_per_minute = 1000;
    c.limits.max_sockets_per_ip = 1000;
    let relay = Relay::new(c.into_settings().unwrap(), None);
    let (addr_tx, addr_rx) = mpsc::channel();
    let (stop, stop_rx) = mpsc::channel::<()>();
    let r = relay.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = serve(listener, r, async move {
                let _ = tokio::task::spawn_blocking(move || stop_rx.recv()).await;
            })
            .await;
        });
    });
    RelayRig { addr: addr_rx.recv().unwrap(), _stop: stop, _relay: relay }
}

/// The office: a real engine with a real peer server, listening on loopback and reachable through
/// the relay as well.
struct Office {
    _tmp: tempfile::TempDir,
    server: PeerServer,
    engine: Engine,
}

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn office(relay: &RelayRig) -> Office {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = ServerConfig { handshake_timeout: Duration::from_secs(5), ..ServerConfig::default() };
    let server = PeerServer::with_config(tmp.path().join("peer"), cfg);
    let host: Arc<dyn PeerHost> = Arc::new(server.clone());
    let options = StateOptions { restore_on_start: false, peer_host: host, ..StateOptions::default() };
    let (state, _report) = State::load(options);
    let engine = Engine::start(state, vec![]).unwrap();
    server.attach(engine.clone());
    engine.call(Caller::Ui, "peer_server_enable", json!({"address": "127.0.0.1", "port": 0})).unwrap();
    let relay_cfg = RelayConfig { url: format!("ws://{}", relay.addr), host_secret: OFFICE_RELAY_SECRET, access: None };
    server.set_relay(Some(relay_cfg)).unwrap();
    wait_for("the office connected to the relay", || {
        server.relay_status().filter(|s| s.connected).map(|_| ())
    });
    Office { _tmp: tmp, server, engine }
}

impl Office {
    fn status(&self) -> Value {
        self.engine.call(Caller::Cli, "peer_status", json!({})).unwrap().data
    }
}

impl Drop for Office {
    fn drop(&mut self) {
        self.server.shutdown();
        self.engine.shutdown();
    }
}

/// The whole pairing from a laptop that can reach only the relay.
fn pair_through_relay(office: &Office) -> (Identity, PairedHost) {
    let out = office.engine.call(Caller::Ui, "peer_pair_start", json!({})).unwrap();
    let mut offer = PeerOffer::decode(out.data["link"].as_str().unwrap()).unwrap();
    assert!(offer.r.is_some(), "the offer names the relay");
    // Port 9 (discard) refuses on loopback: the direct address is unreachable from the laptop.
    offer.a = "tcp://127.0.0.1:9".to_owned();
    let me = Identity::generate().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    let cfg = ClientConfig { pair_wait: Duration::from_secs(20), ..ClientConfig::new("Laptop") };
    let me2 = me.clone();
    let handle = std::thread::spawn(move || client::pair(&offer, &me2, &cfg, &mut |c: &str| drop(tx.send(c.to_owned()))));
    let code = rx.recv_timeout(WAIT).expect("the laptop reached the check code through the relay");
    wait_for("the office shows the waiting device", || (office.status()["pending_pair"]["check_code"].as_str() == Some(code.as_str())).then_some(()));
    office.engine.call(Caller::Ui, "peer_pair_answer", json!({"allow": true, "check_code": code})).unwrap();
    let host = handle.join().unwrap().expect("the pairing finished");
    (me, host)
}

fn route(host: &PairedHost) -> RelayRoute {
    host.relay.clone().expect("the paired host has a relay route")
}

fn closed_by(conn: &Connection) -> bool {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(Incoming::Closed(_)) = conn.recv_timeout(Duration::from_millis(100)) {
            return true;
        }
    }
    false
}

#[test]
fn a_laptop_pairs_and_works_through_the_relay_only() {
    let relay = start_relay();
    let office = office(&relay);
    let (me, host) = pair_through_relay(&office);
    assert_eq!(host.level, Level::Full);
    let r = route(&host);
    assert_eq!(r.url, format!("ws://{}", relay.addr));
    assert_eq!(r.host_id, csm_remote_proto::ids::host_id(&OFFICE_RELAY_SECRET));

    // A later connection: the direct address is unreachable, so the relay carries it.
    let direct = PeerAddr::parse("tcp://127.0.0.1:9").unwrap();
    let cfg = ClientConfig::new("Laptop");
    let conn = client::connect_saved(Some(&direct), Some(&r), &host.host_public, &me, &cfg).expect("connected through the relay");
    assert_eq!(conn.welcome.device_id, host.device_id);
    let t = Duration::from_secs(10);
    assert!(matches!(conn.call_wait("session_list", json!({}), t).unwrap(), Outcome::Ok { .. }));

    // Revoking the device closes its relay channel and keeps it out.
    office.engine.call(Caller::Ui, "peer_device_revoke", json!({"id": host.device_id})).unwrap();
    assert!(closed_by(&conn), "the revoked connection ends");
    assert!(client::connect_saved(None, Some(&r), &host.host_public, &me, &cfg).is_err(), "a revoked device cannot come back");
}

#[test]
fn a_direct_address_that_answers_is_used_before_the_relay() {
    let relay = start_relay();
    let office = office(&relay);
    let (me, host) = pair_through_relay(&office);
    // The office's real direct address answers, so the laptop uses it (and the relay is not needed).
    let direct = PeerAddr::parse(&format!("tcp://{}", office.server.local_addr().unwrap())).unwrap();
    let conn = client::connect_saved(Some(&direct), Some(&route(&host)), &host.host_public, &me, &ClientConfig::new("Laptop")).unwrap();
    assert_eq!(conn.welcome.device_id, host.device_id);
}

//! The phone remote through the real relay: `csm-relay` runs in-process on 127.0.0.1, the add-on's
//! host core runs its relay client against it (`relay_client::run`), and a Rust test phone speaks
//! the wire protocol (WebSocket, Noise, the pairing and op messages) from `csm-remote-proto`.
//!
//! What these tests catch that the protocol tests cannot: a mismatch between the host's and the
//! relay's frame formats, the allow list and pairing slot as the relay really holds them, the order
//! in which the relay delivers a host's Accept and its Close, and reconnection after a relay
//! restart. Expiry of a pairing offer takes five minutes, so it is tested at the host core
//! (`apps/csm-phone/tests/protocol.rs`) with the clock passed in.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use csm_phone::config::Config;
use csm_phone::csm::{Csm, CsmError};
use csm_phone::host::{DeviceView, Host, Out};
use csm_phone::relay_client::{self, Command};
use csm_phone::store;
use csm_relay::config::Config as RelayConfig;
use csm_relay::{Relay, serve};
use csm_remote_proto::PROTOCOL_VERSION;
use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{b64_decode_fixed, b64_encode};
use csm_remote_proto::msg::{ByeReason, Event, Level, Op, Request, Response, ServerMsg};
use csm_remote_proto::noise::{MAX_NOISE_MESSAGE, PARAMS_PAIRING, PARAMS_SESSION, PSK_POSITION, check_code, prologue};
use csm_remote_proto::pairing::{DenyReason, PairMsg, PairingOffer};
use csm_remote_proto::relay::{ClientToRelay, RelayToClient};
use serde_json::{Value, json};
use snow::{Builder, HandshakeState, TransportState};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const SECRET: [u8; 32] = [7; 32];
const WAIT: Duration = Duration::from_secs(10);

type Sock = WebSocket<MaybeTlsStream<TcpStream>>;

// ---- the relay, in process ---------------------------------------------------------------------

fn open_relay_config() -> RelayConfig {
    let mut c = RelayConfig::default();
    c.access.open = true;
    // Plenty of headroom: these tests are about the protocol, not the limits.
    c.limits.new_connections_per_ip_per_minute = 1000;
    c.limits.max_sockets_per_ip = 1000;
    c
}

/// A relay on 127.0.0.1 on its own thread and runtime. Dropping it does not stop it; [`stop`]
/// does (and closes every socket it holds, so the host sees the relay go away).
struct RelayRig {
    addr: SocketAddr,
    relay: Arc<Relay>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RelayRig {
    /// Starts a relay on `port` (0 for any port). A restart reuses the port, so the bind is retried
    /// for a while (the old listener may take a moment to close).
    fn start(port: u16) -> RelayRig {
        let (relay_tx, relay_rx) = mpsc::channel::<(Arc<Relay>, SocketAddr)>();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            rt.block_on(async move {
                let relay = Relay::new(open_relay_config().into_settings().unwrap(), None);
                let listener = bind_retrying(port).await;
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

    fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Stops the relay: every socket it holds is closed first, then the listener.
    fn stop(mut self) {
        self.relay.shutdown_all(RefusalCode::HostOffline);
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

async fn bind_retrying(port: u16) -> tokio::net::TcpListener {
    let deadline = Instant::now() + WAIT;
    loop {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => return l,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => panic!("could not bind 127.0.0.1:{port}: {e}"),
        }
    }
}

// ---- the add-on's host, in process ---------------------------------------------------------------

/// The CSM the host talks to: a fixed answer for the one call these tests make.
struct FakeCsm;

impl Csm for FakeCsm {
    fn call(&self, tool: &str, _params: Value) -> Result<Value, CsmError> {
        match tool {
            "session_list" => Ok(json!([{"id": "s1", "name": "Work", "folder": "/work/app", "status": "idle"}])),
            _ => Ok(json!({})),
        }
    }
}

/// The host, running `relay_client::run` on its own thread, with the person's commands and the
/// host's outputs as channels.
struct HostRig {
    cmd: Sender<Command>,
    outs: Receiver<Out>,
    stop: Arc<AtomicBool>,
}

fn start_host(port: u16, dir: &Path) -> HostRig {
    start_host_with_window(port, dir, None)
}

/// As [`start_host`], with the pairing window shortened (seconds) when given, to see expiry.
fn start_host_with_window(port: u16, dir: &Path, window: Option<u64>) -> HostRig {
    let opened = store::open(dir).unwrap();
    let cfg = Config { relay: format!("ws://127.0.0.1:{port}"), app_origin: "http://127.0.0.1:8080".into(), connect: true, ..Config::default() };
    let mut host = Host::new(dir.to_path_buf(), opened, cfg, &SECRET, Box::new(FakeCsm)).unwrap();
    if let Some(w) = window {
        host.pairing_window = w;
    }
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (out_tx, out_rx) = mpsc::channel::<Out>();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let url = format!("ws://127.0.0.1:{port}");
    std::thread::spawn(move || {
        relay_client::run(&mut host, &url, &SECRET, &thread_stop, &cmd_rx, &mut |o| {
            let _ = out_tx.send(o.clone());
        });
    });
    HostRig { cmd: cmd_tx, outs: out_rx, stop }
}

impl Drop for HostRig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl HostRig {
    fn start_pairing(&self) -> String {
        let (tx, rx) = mpsc::channel();
        self.cmd.send(Command::StartPairing { app_version: "0.1.0".into(), reply: tx }).unwrap();
        rx.recv_timeout(WAIT).expect("the host answers").expect("pairing opens")
    }

    fn decide(&self, allow: Option<Level>) {
        self.cmd.send(Command::Decide(allow)).unwrap();
    }

    fn revoke(&self, device_id: &str) {
        self.cmd.send(Command::Revoke(device_id.to_owned())).unwrap();
    }

    fn devices(&self) -> Vec<DeviceView> {
        let (tx, rx) = mpsc::channel();
        self.cmd.send(Command::Devices(tx)).unwrap();
        rx.recv_timeout(WAIT).expect("the host answers")
    }

    /// Asserts that no output matching `bad` arrives within `wait` (outputs that do not match are
    /// skipped).
    fn none_matching(&self, bad: impl Fn(&Out) -> bool, wait: Duration) {
        let deadline = Instant::now() + wait;
        while let Ok(o) = self.outs.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            assert!(!bad(&o), "the host reported {o:?}");
        }
    }

    /// The next output that `want` accepts; the others are skipped.
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

}

// ---- the test phone -----------------------------------------------------------------------------

struct Phone {
    private: Vec<u8>,
}

impl Phone {
    fn new() -> Phone {
        let kp = Builder::new("Noise_IK_25519_AESGCM_SHA256".parse().unwrap()).generate_keypair().unwrap();
        Phone { private: kp.private }
    }

    fn pairing_initiator(&self, host_pub: &[u8; 32], host_id: &str, psk: &[u8; 32]) -> HandshakeState {
        Builder::new(PARAMS_PAIRING.parse().unwrap())
            .local_private_key(&self.private)
            .unwrap()
            .remote_public_key(host_pub)
            .unwrap()
            .prologue(&prologue(host_id))
            .unwrap()
            .psk(PSK_POSITION, psk)
            .unwrap()
            .build_initiator()
            .unwrap()
    }

    fn session_initiator(&self, host_pub: &[u8; 32], host_id: &str) -> HandshakeState {
        Builder::new(PARAMS_SESSION.parse().unwrap())
            .local_private_key(&self.private)
            .unwrap()
            .remote_public_key(host_pub)
            .unwrap()
            .prologue(&prologue(host_id))
            .unwrap()
            .build_initiator()
            .unwrap()
    }
}

fn parse_offer(url: &str) -> PairingOffer {
    let frag = url.split_once('#').expect("the link has a fragment").1;
    let payload = frag.strip_prefix("p=").expect("the fragment carries the offer");
    PairingOffer::decode(payload).expect("the offer decodes")
}

/// Opens a phone socket with `hello`. The relay answers `Ready` (then Noise messages flow) or an
/// error code, which is returned.
fn open_socket(addr: SocketAddr, hello: &ClientToRelay) -> Result<Sock, RefusalCode> {
    let url = format!("ws://{addr}/v1/client");
    let (mut ws, _) = tungstenite::connect(url.as_str()).expect("the relay accepts a phone socket");
    if let MaybeTlsStream::Plain(tcp) = ws.get_ref() {
        tcp.set_read_timeout(Some(WAIT)).unwrap();
    }
    ws.send(Message::Text(serde_json::to_string(hello).unwrap().into())).unwrap();
    match ws.read().expect("the relay answers the hello") {
        Message::Text(t) => match serde_json::from_str::<RelayToClient>(&t).expect("a relay message") {
            RelayToClient::Ready { .. } => Ok(ws),
            RelayToClient::Error { code } => Err(code),
        },
        other => panic!("expected a text answer, got {other:?}"),
    }
}

/// A pairing socket. The host connects to the relay on its own thread and sends the slot once it
/// has, so `HostOffline` (not connected yet) and `NotAllowed` (slot not open yet) are retried.
fn pairing_socket(addr: SocketAddr, host_id: &str, pairing_id: &[u8; 16]) -> Sock {
    let hello = ClientToRelay::Hello { v: PROTOCOL_VERSION, host_id: host_id.to_owned(), token: None, pairing_id: Some(b64_encode(pairing_id)) };
    let deadline = Instant::now() + WAIT;
    loop {
        match open_socket(addr, &hello) {
            Ok(ws) => return ws,
            Err(RefusalCode::NotAllowed | RefusalCode::HostOffline) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(code) => panic!("the pairing socket was refused: {code:?}"),
        }
    }
}

fn device_socket(addr: SocketAddr, host_id: &str, token: &[u8; 32]) -> Result<Sock, RefusalCode> {
    let hello = ClientToRelay::Hello { v: PROTOCOL_VERSION, host_id: host_id.to_owned(), token: Some(b64_encode(token)), pairing_id: None };
    open_socket(addr, &hello)
}

/// The next binary message from the relay. `None` when the socket closed (or sent text, which is
/// an error answer here).
fn read_binary(ws: &mut Sock) -> Option<Vec<u8>> {
    loop {
        match ws.read() {
            Ok(Message::Binary(b)) => return Some(b.to_vec()),
            Ok(Message::Text(_) | Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

fn seal(t: &mut TransportState, plain: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; plain.len() + 64];
    let n = t.write_message(plain, &mut buf).unwrap();
    buf.truncate(n);
    buf
}

fn open(t: &mut TransportState, cipher: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
    let n = t.read_message(cipher, &mut buf).unwrap();
    buf.truncate(n);
    buf
}

/// Runs the pairing handshake on `ws` with `psk`, then sends `PairMsg::Confirm` (the check code) and
/// `PairMsg::Hello` as transport messages. Returns the phone's transport and the check code it
/// computed from its own handshake.
fn pairing_exchange(ws: &mut Sock, phone: &Phone, offer: &PairingOffer, psk: &[u8; 32], name: &str) -> (TransportState, String) {
    let host_pub = offer.host_pub().expect("the offer has the host key");
    let mut hs = phone.pairing_initiator(&host_pub, &offer.h, psk);
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::Binary(buf[..n].to_vec().into())).unwrap();
    let answer = read_binary(ws).expect("the host answers the handshake");
    let mut payload = vec![0u8; 1024];
    hs.read_message(&answer, &mut payload).expect("the answer is the host's");
    let check = check_code(hs.get_handshake_hash());
    let mut t = hs.into_transport_mode().unwrap();
    let confirm = serde_json::to_vec(&PairMsg::Confirm { check: check.clone() }).unwrap();
    ws.send(Message::Binary(seal(&mut t, &confirm).into())).unwrap();
    let hello = serde_json::to_vec(&PairMsg::Hello { v: PROTOCOL_VERSION, name: name.into() }).unwrap();
    ws.send(Message::Binary(seal(&mut t, &hello).into())).unwrap();
    (t, check)
}

/// What a pairing leaves behind: the device, its routing token and the host's public key (from the
/// offer), which a session handshake needs.
struct Paired {
    device_id: String,
    token: [u8; 32],
    host_pub: [u8; 32],
}

/// Pairs one phone through the relay and allows it at `level`.
fn pair_allowed(host: &HostRig, relay: &RelayRig, phone: &Phone, name: &str, level: Level) -> Paired {
    let offer = parse_offer(&host.start_pairing());
    assert_eq!(offer.h, host_id(), "the offer names this host");
    pair_with_offer(host, relay, &offer, phone, name, level)
}

/// Pairs through an offer the test already holds (the slot it opened), allowing it at `level`.
fn pair_with_offer(host: &HostRig, relay: &RelayRig, offer: &PairingOffer, phone: &Phone, name: &str, level: Level) -> Paired {
    let psk = offer.psk().unwrap();
    let mut ws = pairing_socket(relay.addr, &offer.h, &offer.pairing_id().unwrap());
    let (mut t, phone_check) = pairing_exchange(&mut ws, phone, offer, &psk, name);
    let shown = host.wait_out(|o| matches!(o, Out::Pending { .. }));
    let Out::Pending { check, name: shown_name } = shown else { unreachable!() };
    assert_eq!(check, phone_check, "the PC shows the code the phone computed");
    assert_eq!(shown_name, name);
    host.decide(Some(level));
    let accept = read_binary(&mut ws).expect("the accept arrives before the close");
    let PairMsg::Accept { routing_token, level: got, device_id } = serde_json::from_slice(&open(&mut t, &accept)).unwrap() else {
        panic!("expected Accept")
    };
    assert_eq!(got, level);
    assert!(read_binary(&mut ws).is_none(), "the relay closes the pairing socket after the Accept");
    Paired { device_id, token: b64_decode_fixed::<32>(&routing_token).unwrap(), host_pub: offer.host_pub().unwrap() }
}

/// A paired phone's session: the relay opens a channel, then the Noise IK handshake runs on it.
fn connect_device(relay: &RelayRig, host_id: &str, host_pub: &[u8; 32], phone: &Phone, token: &[u8; 32]) -> Result<(Sock, TransportState), RefusalCode> {
    let mut ws = device_socket(relay.addr, host_id, token)?;
    let mut hs = phone.session_initiator(host_pub, host_id);
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::Binary(buf[..n].to_vec().into())).unwrap();
    let reply = read_binary(&mut ws).ok_or(RefusalCode::NotAllowed)?;
    let mut payload = vec![0u8; 1024];
    hs.read_message(&reply, &mut payload).expect("the host's handshake answer");
    Ok((ws, hs.into_transport_mode().unwrap()))
}

fn request(ws: &mut Sock, t: &mut TransportState, id: u64, op: Op) -> Response {
    let plain = serde_json::to_vec(&Request { id, op }).unwrap();
    ws.send(Message::Binary(seal(t, &plain).into())).unwrap();
    loop {
        let reply = read_binary(ws).expect("a reply to the request");
        match ServerMsg::parse(&open(t, &reply)).expect("a server message") {
            ServerMsg::Response(r) => return r,
            ServerMsg::Event(_) => {}
        }
    }
}

/// The host id the relay knows this host by: derived from the secret both sides use.
fn host_id() -> String {
    csm_remote_proto::ids::host_id(&SECRET)
}

// ---- the tests ------------------------------------------------------------------------------------

#[test]
fn pair_allow_then_the_phone_connects_and_reads_sessions_through_the_real_relay() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(relay.port(), dir.path());
    let phone = Phone::new();
    let paired = pair_allowed(&host, &relay, &phone, "Pixel 9", Level::Viewer);

    let devices = host.devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].device_id, paired.device_id);
    assert_eq!(devices[0].name, "Pixel 9");
    assert_eq!(devices[0].level, Level::Viewer);

    let (mut ws, mut t) = connect_device(&relay, &host_id(), &paired.host_pub, &phone, &paired.token).expect("the paired phone connects");
    let r = request(&mut ws, &mut t, 1, Op::SessionList);
    assert!(r.ok, "the viewer may read: {r:?}");
    assert!(r.data.unwrap().to_string().contains("Work"), "the CSM answer reaches the phone");
    relay.stop();
}

#[test]
fn deny_tells_the_phone_and_saves_nothing() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(relay.port(), dir.path());
    let offer = parse_offer(&host.start_pairing());
    let psk = offer.psk().unwrap();
    let phone = Phone::new();
    let mut ws = pairing_socket(relay.addr, &offer.h, &offer.pairing_id().unwrap());
    let (mut t, _check) = pairing_exchange(&mut ws, &phone, &offer, &psk, "Pixel 9");
    host.wait_out(|o| matches!(o, Out::Pending { .. }));
    host.decide(None);
    let deny = read_binary(&mut ws).expect("the deny arrives");
    let PairMsg::Deny { reason } = serde_json::from_slice(&open(&mut t, &deny)).unwrap() else { panic!("expected Deny") };
    assert_eq!(reason, DenyReason::Denied);
    assert!(read_binary(&mut ws).is_none(), "the pairing socket closes after the deny");
    assert!(host.devices().is_empty(), "a denied phone is not saved");
    relay.stop();
}

/// A phone with the wrong PSK cannot read the host's message 2, so it never sends the key
/// confirmation. Whatever it sends instead does not decrypt, so the host shows nothing to the
/// person and saves nothing, and the channel is closed.
#[test]
fn a_wrong_psk_shows_nothing_and_saves_nothing() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(relay.port(), dir.path());
    let offer = parse_offer(&host.start_pairing());
    let mut wrong = offer.psk().unwrap();
    wrong[0] ^= 0xFF;
    let phone = Phone::new();
    let mut ws = pairing_socket(relay.addr, &offer.h, &offer.pairing_id().unwrap());
    let host_pub = offer.host_pub().unwrap();
    let mut hs = phone.pairing_initiator(&host_pub, &offer.h, &wrong);
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::Binary(buf[..n].to_vec().into())).unwrap();
    let answer = read_binary(&mut ws).expect("the host answers message 1 (it cannot yet tell)");
    let mut payload = vec![0u8; 1024];
    assert!(hs.read_message(&answer, &mut payload).is_err(), "the phone with the wrong PSK refuses the host's message 2");
    ws.send(Message::Binary(vec![7u8; 90].into())).unwrap();
    assert!(read_binary(&mut ws).is_none(), "the host closes the channel");
    host.none_matching(|o| matches!(o, Out::Pending { .. }), Duration::from_millis(300));
    assert!(host.devices().is_empty(), "nothing is saved");
    relay.stop();
}

/// A pairing socket for a code the host did not open (another pairing id) is refused by the relay,
/// and it does not cost the open slot: the right code still pairs afterwards. The host saves
/// nothing and shows nothing for the wrong attempt.
#[test]
fn a_wrong_pairing_code_is_refused_by_the_relay_and_the_open_slot_still_pairs() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(relay.port(), dir.path());
    let offer = parse_offer(&host.start_pairing());
    // The relay learns of the open slot from the host's own connection; give it a moment.
    std::thread::sleep(Duration::from_millis(300));
    let mut wrong_id = offer.pairing_id().unwrap();
    wrong_id[0] ^= 0xFF;
    let wrong = ClientToRelay::Hello { v: PROTOCOL_VERSION, host_id: offer.h.clone(), token: None, pairing_id: Some(b64_encode(&wrong_id)) };
    match open_socket(relay.addr, &wrong) {
        Ok(_) => panic!("a code the host did not open must not reach the host"),
        Err(code) => assert_eq!(code, RefusalCode::NotAllowed),
    }
    assert!(host.devices().is_empty(), "nothing is saved for the wrong attempt");
    let phone = Phone::new();
    let paired = pair_with_offer(&host, &relay, &offer, &phone, "Pixel 9", Level::Viewer);
    assert_eq!(host.devices().len(), 1, "the right code pairs after the wrong one");
    assert_eq!(paired.device_id, host.devices()[0].device_id);
    relay.stop();
}

/// An offer that has run out is refused by the relay (it holds the slot's own expiry), and the
/// host ends the pairing on its side and saves nothing. Uses a 2 second window.
#[test]
fn an_expired_pairing_offer_is_refused_and_the_host_ends_the_pairing() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host_with_window(relay.port(), dir.path(), Some(2));
    let offer = parse_offer(&host.start_pairing());
    std::thread::sleep(Duration::from_millis(2500));
    let hello = ClientToRelay::Hello { v: PROTOCOL_VERSION, host_id: offer.h.clone(), token: None, pairing_id: Some(b64_encode(&offer.pairing_id().unwrap())) };
    match open_socket(relay.addr, &hello) {
        Ok(_) => panic!("an expired offer must not open a pairing channel"),
        Err(code) => assert_eq!(code, RefusalCode::NotAllowed),
    }
    host.wait_out(|o| matches!(o, Out::PairingEnded));
    assert!(host.devices().is_empty(), "an expired offer saves nothing");
    relay.stop();
}

#[test]
fn revoke_closes_the_live_channel_and_the_relay_refuses_the_phone_after() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(relay.port(), dir.path());
    let phone = Phone::new();
    let paired = pair_allowed(&host, &relay, &phone, "Pixel 9", Level::Operator);
    let (mut ws, mut t) = connect_device(&relay, &host_id(), &paired.host_pub, &phone, &paired.token).expect("connects before the revoke");

    host.revoke(&paired.device_id);
    // The snapshot that follows the handshake is read on the way (it is not the Bye).
    let reason = loop {
        let bye = read_binary(&mut ws).expect("the host says bye on the live channel");
        match ServerMsg::parse(&open(&mut t, &bye)).expect("a server message") {
            ServerMsg::Event(Event::Bye { reason }) => break reason,
            ServerMsg::Event(_) => {}
            ServerMsg::Response(_) => panic!("expected Bye"),
        }
    };
    assert_eq!(reason, ByeReason::Revoked);
    assert!(read_binary(&mut ws).is_none(), "the channel closes");

    // The relay's allow list catches up with the host's; until it does a reconnect can still open.
    let deadline = Instant::now() + WAIT;
    loop {
        match device_socket(relay.addr, &host_id(), &paired.token) {
            Err(code) => {
                assert_eq!(code, RefusalCode::NotAllowed);
                break;
            }
            Ok(_) => {
                assert!(Instant::now() < deadline, "the relay still admits the revoked phone");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    assert!(host.devices().is_empty());
    relay.stop();
}

#[test]
fn the_host_reconnects_after_a_relay_restart_and_paired_phones_connect_again() {
    let relay = RelayRig::start(0);
    let port = relay.port();
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(port, dir.path());
    let phone = Phone::new();
    let paired = pair_allowed(&host, &relay, &phone, "Pixel 9", Level::Viewer);
    relay.stop();

    // A new relay on the same port: it knows nothing until the host connects and sends its list.
    let relay2 = RelayRig::start(port);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut connected = None;
    while connected.is_none() {
        assert!(Instant::now() < deadline, "the host did not come back to the new relay");
        match connect_device(&relay2, &host_id(), &paired.host_pub, &phone, &paired.token) {
            Ok(pair) => connected = Some(pair),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
    let (mut ws, mut t) = connected.unwrap();
    let r = request(&mut ws, &mut t, 7, Op::SessionList);
    assert!(r.ok, "the paired phone works again after the restart: {r:?}");
    relay2.stop();
}

// ---- the screens, driven through their events ----------------------------------------------------
//
// The shell's side of the screens, without stdio: what the host reports is absorbed into `Live`, a
// screen event is turned into commands by `screens::event`, and the commands go to the host, as
// the shell does. So these tests press the same buttons the person presses, against the real relay.

use csm_phone::config as cfg_file;
use csm_phone::relay_client::now_secs;
use csm_phone::screens::{self, Live, Ui, View};

/// A screen (one of the add-on's three) on a running host.
struct ScreenRig<'a> {
    host: &'a HostRig,
    live: Live,
    ui: Ui,
}

impl<'a> ScreenRig<'a> {
    fn new(host: &'a HostRig) -> Self {
        Self { host, live: Live { enabled: true, ..Live::default() }, ui: Ui::default() }
    }

    /// Takes what the host has reported so far, and a fresh view of its config and devices.
    fn pump(&mut self) {
        while let Ok(out) = self.host.outs.try_recv() {
            self.live.absorb(&out);
        }
        let (tx, rx) = mpsc::channel();
        self.host.cmd.send(Command::View(tx)).unwrap();
        if let Ok(view) = rx.recv_timeout(WAIT) {
            self.live.host = Some(view);
        }
    }

    /// Pumps until `done` holds, or fails after the usual wait.
    fn wait_for(&mut self, what: &str, done: impl Fn(&Live) -> bool) {
        let deadline = Instant::now() + WAIT;
        loop {
            self.pump();
            if done(&self.live) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}: {:?}", self.live.notice);
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// One press of a control: the screen's state changes, and its commands go to the host.
    fn event(&mut self, screen: &str, key: &str, kind: &str, value: Value) {
        let view = View { live: &self.live, now: now_secs(), problem: None };
        let commands = screens::event(screen, &view, &mut self.ui, key, kind, &value);
        for c in commands {
            self.host.cmd.send(c).unwrap();
        }
    }

    fn tree(&self, screen: &str) -> String {
        let view = View { live: &self.live, now: now_secs(), problem: None };
        screens::tree(screen, &view, &self.ui).expect("a declared screen").to_string()
    }
}

/// A host that reads `config.json` the way the add-on's shell does at start.
fn start_host_reading_config(port: u16, dir: &Path) -> HostRig {
    let cfg = match std::fs::read_to_string(dir.join(store::CONFIG_FILE)) {
        Ok(text) => cfg_file::parse(&text),
        Err(_) => Config::default(),
    };
    let opened = store::open(dir).unwrap();
    let mut host = Host::new(dir.to_path_buf(), opened, cfg, &SECRET, Box::new(FakeCsm)).unwrap();
    host.pairing_window = 600;
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (out_tx, out_rx) = mpsc::channel::<Out>();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let url = format!("ws://127.0.0.1:{port}");
    std::thread::spawn(move || {
        relay_client::run(&mut host, &url, &SECRET, &thread_stop, &cmd_rx, &mut |o| {
            let _ = out_tx.send(o.clone());
        });
    });
    HostRig { cmd: cmd_tx, outs: out_rx, stop }
}

/// Reads the saved `config.json` as the add-on reads it at start.
fn saved_config(dir: &Path) -> Config {
    cfg_file::parse(&std::fs::read_to_string(dir.join(store::CONFIG_FILE)).expect("config.json is written"))
}

#[test]
fn the_pairing_screen_pairs_a_phone_through_its_buttons() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(relay.port(), dir.path());
    let mut screen = ScreenRig::new(&host);
    screen.wait_for("the host's view", |l| l.host.is_some());
    assert!(screen.tree(screens::PAIR).contains("Start pairing"));

    screen.event(screens::PAIR, "start", "click", Value::Null);
    screen.wait_for("the offer", |l| l.offer.is_some());
    assert!(screen.tree(screens::PAIR).contains("\"type\":\"qr\""), "the code is on the screen");
    let offer = parse_offer(screen.live.offer.as_deref().unwrap());
    let phone = Phone::new();
    let psk = offer.psk().unwrap();
    let mut ws = pairing_socket(relay.addr, &offer.h, &offer.pairing_id().unwrap());
    let (mut t, phone_check) = pairing_exchange(&mut ws, &phone, &offer, &psk, "Pixel 9");

    screen.wait_for("the waiting phone", |l| l.pending.is_some());
    let (check, name) = screen.live.pending.clone().unwrap();
    assert_eq!(check, phone_check, "the screen shows the code the phone computed");
    assert_eq!(name, "Pixel 9");
    assert!(screen.tree(screens::PAIR).contains(&check), "the check code is drawn");

    screen.event(screens::PAIR, "level", "select", json!({"value": "operator"}));
    screen.event(screens::PAIR, "allow", "click", Value::Null);
    let accept = read_binary(&mut ws).expect("the accept arrives");
    let PairMsg::Accept { level, .. } = serde_json::from_slice(&open(&mut t, &accept)).unwrap() else {
        panic!("expected Accept")
    };
    assert_eq!(level, Level::Operator, "the level chosen on the screen is the one granted");

    screen.wait_for("the end of the pairing", |l| l.offer.is_none() && l.pending.is_none());
    screen.wait_for("the link is up", |l| matches!(l.link, Some(csm_phone::host::Link::Up)));
    assert_eq!(host.devices().len(), 1);
    let status = screens::status_line(&View { live: &screen.live, now: now_secs(), problem: None });
    assert_eq!(status, ("Connected to relay, 1 phone".to_owned(), "ok"), "the indicator counts the paired phone");
    relay.stop();
}

#[test]
fn the_pairing_screen_denies_a_phone_and_saves_nothing() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(relay.port(), dir.path());
    let mut screen = ScreenRig::new(&host);
    screen.event(screens::PAIR, "start", "click", Value::Null);
    screen.wait_for("the offer", |l| l.offer.is_some());
    let offer = parse_offer(screen.live.offer.as_deref().unwrap());
    let phone = Phone::new();
    let psk = offer.psk().unwrap();
    let mut ws = pairing_socket(relay.addr, &offer.h, &offer.pairing_id().unwrap());
    let _ = pairing_exchange(&mut ws, &phone, &offer, &psk, "Stranger");
    screen.wait_for("the waiting phone", |l| l.pending.is_some());

    screen.event(screens::PAIR, "deny", "click", Value::Null);
    screen.wait_for("the pairing to end", |l| l.offer.is_none());
    assert_eq!(screen.live.notice.as_deref(), Some("Pairing stopped."));
    assert!(host.devices().is_empty(), "a denied phone is not saved");
    relay.stop();
}

#[test]
fn an_expired_pairing_offer_ends_on_the_screen_with_its_reason() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host_with_window(relay.port(), dir.path(), Some(2));
    let mut screen = ScreenRig::new(&host);
    screen.event(screens::PAIR, "start", "click", Value::Null);
    screen.wait_for("the offer", |l| l.offer.is_some());
    screen.wait_for("the offer to expire", |l| l.offer.is_none());
    assert!(screen.live.notice.as_deref().is_some_and(|n| n.contains("expired")), "{:?}", screen.live.notice);
    relay.stop();
}

#[test]
fn revoke_on_the_phones_screen_cuts_the_live_channel_at_once() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host(relay.port(), dir.path());
    let phone = Phone::new();
    let paired = pair_allowed(&host, &relay, &phone, "Pixel 9", Level::Operator);
    let (mut ws, mut t) = connect_device(&relay, &host_id(), &paired.host_pub, &phone, &paired.token).expect("connects before the revoke");

    let mut screen = ScreenRig::new(&host);
    screen.wait_for("the phone in the list", |l| !l.devices().is_empty());
    screen.event(screens::DEVICES, "devices", "select", json!({"key": paired.device_id}));
    screen.event(screens::DEVICES, "revoke", "click", Value::Null);
    assert!(screen.tree(screens::DEVICES).contains("Yes, revoke"), "the first press only asks");
    assert_eq!(host.devices().len(), 1, "nothing is revoked by the first press");
    screen.event(screens::DEVICES, "revoke_yes", "click", Value::Null);

    // The host closes the channel itself: the phone gets Bye and then the close.
    let reason = loop {
        let bye = read_binary(&mut ws).expect("the host says bye on the live channel");
        match ServerMsg::parse(&open(&mut t, &bye)).expect("a server message") {
            ServerMsg::Event(Event::Bye { reason }) => break reason,
            ServerMsg::Event(_) => {}
            ServerMsg::Response(_) => panic!("expected Bye"),
        }
    };
    assert_eq!(reason, ByeReason::Revoked);
    assert!(read_binary(&mut ws).is_none(), "the channel closes");
    screen.wait_for("the phone gone from the list", |l| l.devices().is_empty());
    relay.stop();
}

#[test]
fn settings_round_trip_through_the_screen_and_the_file() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let host = start_host_reading_config(relay.port(), dir.path());
    let folder = tempfile::tempdir().unwrap();
    let folder_path = folder.path().to_string_lossy().to_string();
    let mut screen = ScreenRig::new(&host);
    screen.wait_for("the host's view", |l| l.host.is_some());

    screen.event(screens::SETTINGS, "relay", "submit", json!({"value": "wss://relay.example/"}));
    screen.wait_for("the relay saved", |l| l.notice.as_deref().is_some_and(|n| n.starts_with("Saved")));
    assert!(screen.live.notice.as_deref().is_some_and(|n| n.contains("restart")), "a relay change needs a restart");
    screen.event(screens::SETTINGS, "folder_add", "submit", json!({"value": folder_path}));
    screen.wait_for("the folder saved", |l| l.host.as_ref().is_some_and(|h| !h.cfg.folders.is_empty()));
    screen.event(screens::SETTINGS, "push", "toggle", json!({"checked": true}));
    screen.event(screens::SETTINGS, "max_sessions", "select", json!({"value": "9"}));
    screen.event(screens::SETTINGS, "origin", "submit", json!({"value": "https://phone.example"}));
    screen.wait_for("the last change saved", |l| l.host.as_ref().is_some_and(|h| h.cfg.max_phone_sessions == 9));

    // The file is what the next start reads: every value comes back.
    let saved = saved_config(dir.path());
    assert_eq!(saved.relay, "wss://relay.example/");
    assert_eq!(saved.folders.len(), 1);
    assert!(saved.push);
    assert_eq!(saved.max_phone_sessions, 9);
    assert_eq!(saved.app_origin, "https://phone.example");

    // A refused value is refused on the screen and the file keeps the good one.
    screen.event(screens::SETTINGS, "relay", "submit", json!({"value": "ws://relay.example/"}));
    screen.wait_for("the refusal", |l| l.notice.as_deref().is_some_and(|n| n.contains("relay must be wss")));
    assert_eq!(saved_config(dir.path()).relay, "wss://relay.example/");
    relay.stop();
}

#[test]
fn a_damaged_config_is_kept_when_the_settings_are_first_written() {
    let relay = RelayRig::start(0);
    let dir = tempfile::tempdir().unwrap();
    let damaged = "{ this is not json";
    std::fs::write(dir.path().join(store::CONFIG_FILE), damaged).unwrap();
    let host = start_host_reading_config(relay.port(), dir.path());
    let mut screen = ScreenRig::new(&host);
    screen.wait_for("the host's view", |l| l.host.is_some());
    assert!(screen.tree(screens::SETTINGS).contains("config.json is damaged"), "the screen says so");
    assert_eq!(std::fs::read_to_string(dir.path().join(store::CONFIG_FILE)).unwrap(), damaged, "nothing is written at start");

    screen.event(screens::SETTINGS, "push", "toggle", json!({"checked": true}));
    screen.wait_for("the first change saved", |l| l.notice.as_deref().is_some_and(|n| n.starts_with("Saved")));

    let kept: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("config.json.damaged-"))
        .collect();
    assert_eq!(kept.len(), 1, "the damaged file is kept under its own name");
    assert_eq!(std::fs::read_to_string(kept[0].path()).unwrap(), damaged, "its bytes are untouched");
    assert!(saved_config(dir.path()).push, "the new file holds the change");
    relay.stop();
}

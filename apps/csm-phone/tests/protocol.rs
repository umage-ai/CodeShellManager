//! The host's protocol core, driven by a test phone that speaks the real Noise patterns with
//! `snow` and the shared wire types of `csm-remote-proto`. No sockets: frames go straight in and
//! out of [`Host`], and CSM is a fake that records every call.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_phone::config::{Config, FolderEntry, ProfileEntry};
use csm_phone::csm::{Csm, CsmError};
use csm_phone::host::{Host, Out};
use csm_phone::push;
use csm_phone::store::{self, PushSub};
use csm_phone::tracker;
use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{b64_decode, b64_decode_fixed, b64_encode, token_hash};
use csm_remote_proto::limits::MAX_APP_MESSAGE_BYTES;
use csm_remote_proto::msg::{self, ByeReason, Event, Level, Op, Request, Response, ServerMsg};
use csm_remote_proto::noise::{MAX_NOISE_MESSAGE, PARAMS_PAIRING, PARAMS_SESSION, PSK_POSITION, check_code, prologue};
use csm_remote_proto::pairing::{DenyReason, PairMsg};
use csm_remote_proto::relay::{FrameKind, HostToRelay, OpenInfo, PushRequest, RelayToHost, decode_frame, encode_frame, parse_close_payload};
use serde_json::{Value, json};
use snow::{Builder, HandshakeState, TransportState};

const NOW: u64 = 1_000_000;
const SECRET: [u8; 32] = [42; 32];

// ---- fake CSM ----------------------------------------------------------------------------

#[derive(Default)]
struct Calls {
    log: Mutex<Vec<(String, Value)>>,
    sessions: Mutex<Vec<Value>>,
    next_id: Mutex<u32>,
    /// Milliseconds `session_list` sleeps before it answers (0 = at once). Set after pairing.
    delay_ms: AtomicU64,
}

struct Fake(Arc<Calls>);

impl Csm for Fake {
    fn call(&self, tool: &str, params: Value) -> Result<Value, CsmError> {
        self.0.log.lock().unwrap().push((tool.to_owned(), params.clone()));
        let delay = self.0.delay_ms.load(Ordering::SeqCst);
        if delay > 0 && tool == "session_list" {
            std::thread::sleep(Duration::from_millis(delay));
        }
        match tool {
            "session_list" => Ok(Value::Array(self.0.sessions.lock().unwrap().clone())),
            "session_new" => {
                let mut n = self.0.next_id.lock().unwrap();
                *n += 1;
                let id = format!("s{n}");
                self.0.sessions.lock().unwrap().push(json!({"id": id, "name": "New", "folder": params["folder"], "status": "working"}));
                Ok(json!({ "id": id }))
            }
            "settings_get" => Ok(json!({ "value": ["/remote-control", "/clear"] })),
            "task_get" => Ok(json!({"id": params["task_id"], "title": "Fix it", "state": "backlog", "body": "must not leave"})),
            "task_list" => Ok(json!([{"id": "t1", "title": "Fix it", "state": "backlog"}])),
            "inbox_list" => Ok(json!([])),
            // CSM's real shape: the view under `session`, the program's own text in its status_message.
            "session_get" => Ok(json!({
                "session": {"id": params["session_id"], "name": "A", "folder": "/work/app", "status": "idle", "status_message": "Waiting for you"},
                "config": {"command": "claude"},
                "program_status": {"message": "Waiting for you"},
                "busy": null,
                "cwd": "/work/app"
            })),
            "session_list_hostile" => Ok(json!([])),
            _ => Ok(json!({})),
        }
    }
}

fn tools_called(c: &Calls) -> Vec<String> {
    c.log.lock().unwrap().iter().map(|(t, _)| t.clone()).collect()
}

fn count(c: &Calls, tool: &str) -> usize {
    tools_called(c).iter().filter(|t| *t == tool).count()
}

fn config() -> Config {
    Config {
        relay: "wss://relay.example".into(),
        connect: true,
        folders: vec![FolderEntry { path: "/work/app".into(), pinned: true }],
        profiles: vec![ProfileEntry { name: "claude".into(), label: "Claude".into(), command: "claude".into() }],
        ..Config::default()
    }
}

fn host_in(dir: &std::path::Path, cfg: Config) -> (Host, Arc<Calls>) {
    let opened = store::open(dir).unwrap();
    let calls = Arc::new(Calls::default());
    let host = Host::new(dir.to_path_buf(), opened, cfg, &SECRET, Box::new(Fake(Arc::clone(&calls)))).unwrap();
    (host, calls)
}

// ---- test phone ----------------------------------------------------------------------------

struct Phone {
    private: [u8; 32],

}

fn new_phone() -> Phone {
    let kp = Builder::new("Noise_IK_25519_AESGCM_SHA256".parse().unwrap()).generate_keypair().unwrap();
    Phone { private: kp.private.try_into().unwrap() }
}

fn pair_initiator(phone: &Phone, host_pub: &[u8; 32], host_id: &str, psk: &[u8; 32]) -> HandshakeState {
    Builder::new(PARAMS_PAIRING.parse().unwrap())
        .local_private_key(&phone.private)
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

fn device_initiator(phone: &Phone, host_pub: &[u8; 32], host_id: &str) -> HandshakeState {
    Builder::new(PARAMS_SESSION.parse().unwrap())
        .local_private_key(&phone.private)
        .unwrap()
        .remote_public_key(host_pub)
        .unwrap()
        .prologue(&prologue(host_id))
        .unwrap()
        .build_initiator()
        .unwrap()
}

fn seal(t: &mut TransportState, plain: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; plain.len() + 64];
    let n = t.write_message(plain, &mut buf).unwrap();
    buf.truncate(n);
    buf
}

fn open_msg(t: &mut TransportState, cipher: &[u8]) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
    let n = t.read_message(cipher, &mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}

/// Frames the host sent, as (kind, channel, payload).
fn frames(outs: &[Out]) -> Vec<(FrameKind, u32, Vec<u8>)> {
    outs.iter()
        .filter_map(|o| match o {
            Out::Frame(b) => decode_frame(b).ok().map(|f| (f.kind, f.channel, f.payload.to_vec())),
            _ => None,
        })
        .collect()
}

fn data_on(outs: &[Out], ch: u32) -> Vec<Vec<u8>> {
    frames(outs).into_iter().filter(|(k, c, _)| *k == FrameKind::Data && *c == ch).map(|(_, _, p)| p).collect()
}

fn closed_on(outs: &[Out], ch: u32) -> Option<Option<RefusalCode>> {
    frames(outs).into_iter().find(|(k, c, _)| *k == FrameKind::Close && *c == ch).map(|(_, _, p)| parse_close_payload(&p))
}

fn open_frame(ch: u32, info: OpenInfo) -> Vec<u8> {
    encode_frame(FrameKind::Open, ch, &info.encode())
}

fn data_frame(ch: u32, payload: &[u8]) -> Vec<u8> {
    encode_frame(FrameKind::Data, ch, payload)
}

/// Opens and completes a pairing on channel `ch` the way the phone does: message 1 (no payload),
/// read the host's message 2, then `Confirm` (the check code), then `Hello` (the name). Returns the
/// phone's transport, the check code the phone computed, and what the host sent for the `Hello`
/// (the pending entry).
fn pair_until_waiting(host: &mut Host, phone: &Phone, ch: u32, name: &str, psk: &[u8; 32]) -> (TransportState, String, Vec<Out>) {
    let pid = host.slot.as_ref().unwrap().pairing_id;
    let outs = host.on_frame(&open_frame(ch, OpenInfo::Pairing(pid)), NOW);
    assert!(outs.is_empty(), "the open is accepted without an answer");
    let mut hs = pair_initiator(phone, &host.host_pub, &host.host_id, psk);
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf).unwrap();
    let outs = host.on_frame(&data_frame(ch, &buf[..n]), NOW);
    let answer = data_on(&outs, ch).first().cloned().expect("the host answers message 1");
    let mut payload = vec![0u8; 1024];
    hs.read_message(&answer, &mut payload).unwrap();
    let check = check_code(hs.get_handshake_hash());
    let mut t = hs.into_transport_mode().unwrap();
    let confirm = serde_json::to_vec(&PairMsg::Confirm { check: check.clone() }).unwrap();
    let outs = host.on_frame(&data_frame(ch, &seal(&mut t, &confirm)), NOW);
    assert!(!outs.iter().any(|o| matches!(o, Out::Pending { .. })), "the confirmation alone shows nothing to the person");
    let hello = serde_json::to_vec(&PairMsg::Hello { v: 1, name: name.into() }).unwrap();
    let outs = host.on_frame(&data_frame(ch, &seal(&mut t, &hello)), NOW);
    (t, check, outs)
}

fn accepted_device(host: &mut Host, phone: &Phone, level: Level) -> (String, [u8; 32]) {
    let psk = host.slot.as_ref().unwrap().psk;
    let (mut t, check, outs) = pair_until_waiting(host, phone, 1, "Pixel 9", &psk);
    assert!(outs.iter().any(|o| matches!(o, Out::Pending { check: c, name } if *c == check && name == "Pixel 9")), "the PC shows the same code the phone computed");
    let outs = host.decide(NOW, Some(level));
    let accept = data_on(&outs, 1);
    let plain = open_msg(&mut t, accept.first().unwrap()).unwrap();
    let PairMsg::Accept { routing_token, level: got, device_id } = serde_json::from_slice(&plain).unwrap() else { panic!("expected Accept") };
    assert_eq!(got, level);
    let token: [u8; 32] = csm_remote_proto::ids::b64_decode_fixed(&routing_token).unwrap();
    assert!(outs.iter().any(|o| matches!(o, Out::Control(HostToRelay::Allow { tokens }) if tokens.len() == 1)));
    assert!(outs.iter().any(|o| matches!(o, Out::DevicesChanged)));
    (device_id, token)
}

/// Connects a paired device on `ch`, returning its transport for requests.
fn connect_device(host: &mut Host, phone: &Phone, token: &[u8; 32], ch: u32) -> Option<TransportState> {
    connect_device_counted(host, phone, token, ch).map(|(t, _)| t)
}

/// As [`connect_device`], and the number of transport messages the phone has received so far. The
/// host sends a snapshot of its session list straight after the handshake; it is read here, so the
/// nonces stay in step, and it counts toward the rekey schedule.
fn connect_device_counted(host: &mut Host, phone: &Phone, token: &[u8; 32], ch: u32) -> Option<(TransportState, u64)> {
    let hash = token_hash(token);
    let outs = host.on_frame(&open_frame(ch, OpenInfo::Device(hash)), NOW);
    if closed_on(&outs, ch).is_some() {
        return None;
    }
    let mut hs = device_initiator(phone, &host.host_pub, &host.host_id);
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf).unwrap();
    let outs = host.on_frame(&data_frame(ch, &buf[..n]), NOW);
    if closed_on(&outs, ch).is_some() {
        return None;
    }
    let frames = data_on(&outs, ch);
    let reply = frames.first()?;
    let mut payload = vec![0u8; 1024];
    hs.read_message(reply, &mut payload).ok()?;
    let mut t = hs.into_transport_mode().ok()?;
    for snapshot in frames.iter().skip(1) {
        open_msg(&mut t, snapshot)?;
    }
    // The handshake reply is not a transport message; the snapshot parts are.
    Some((t, u64::try_from(frames.len().saturating_sub(1)).ok()?))
}

fn request(host: &mut Host, ch: u32, t: &mut TransportState, id: u64, op: Op) -> Option<Response> {
    let plain = serde_json::to_vec(&Request { id, op }).unwrap();
    let mut outs = host.on_frame(&data_frame(ch, &seal(t, &plain)), NOW);
    // The ops run on the worker: the answer comes when it is done, as the relay loop would send it.
    outs.extend(host.wait_idle());
    let reply = data_on(&outs, ch).first().cloned()?;
    match ServerMsg::parse(&open_msg(t, &reply)?)? {
        ServerMsg::Response(r) => Some(r),
        ServerMsg::Event(_) => None,
    }
}

fn host_with_device(level: Level, cfg: Config) -> (tempfile::TempDir, Host, Arc<Calls>, Phone, String, [u8; 32]) {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, calls) = host_in(dir.path(), cfg);
    let (_url, _) = host.start_pairing(NOW, "0.1.0").unwrap();
    let phone = new_phone();
    let (device_id, token) = accepted_device(&mut host, &phone, level);
    (dir, host, calls, phone, device_id, token)
}

// ---- pairing -------------------------------------------------------------------------------

#[test]
fn pairing_allow_saves_the_device_and_the_phone_can_connect() {
    let (dir, mut host, _calls, phone, device_id, token) = host_with_device(Level::Operator, config());
    assert!(host.slot.is_none(), "the slot closes after Allow");
    let saved = store::open(dir.path()).unwrap();
    assert_eq!(saved.devices.len(), 1, "the device is saved before the phone is told");
    assert_eq!(saved.devices[0].device_id, device_id);
    assert_eq!(saved.devices[0].token_hash, b64_encode(&token_hash(&token)));
    let mut t = connect_device(&mut host, &phone, &token, 2).expect("a paired phone connects");
    let r = request(&mut host, 2, &mut t, 1, Op::Ping).unwrap();
    assert!(r.ok);
}

#[test]
fn pairing_deny_sends_denied_and_saves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let psk = host.slot.as_ref().unwrap().psk;
    let phone = new_phone();
    let (mut t, _check, _) = pair_until_waiting(&mut host, &phone, 1, "Phone", &psk);
    let outs = host.decide(NOW, None);
    let plain = open_msg(&mut t, &data_on(&outs, 1)[0]).unwrap();
    assert_eq!(serde_json::from_slice::<PairMsg>(&plain).unwrap(), PairMsg::Deny { reason: DenyReason::Denied });
    assert!(outs.iter().any(|o| matches!(o, Out::Control(HostToRelay::PairingClose))));
    assert!(host.devices.is_empty());
    assert!(store::open(dir.path()).unwrap().devices.is_empty());
}

#[test]
fn pairing_expiry_denies_the_waiting_phone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let psk = host.slot.as_ref().unwrap().psk;
    let phone = new_phone();
    let (mut t, _, _) = pair_until_waiting(&mut host, &phone, 1, "Phone", &psk);
    let outs = host.tick(NOW + 601);
    let plain = open_msg(&mut t, &data_on(&outs, 1)[0]).unwrap();
    assert_eq!(serde_json::from_slice::<PairMsg>(&plain).unwrap(), PairMsg::Deny { reason: DenyReason::Expired });
    assert!(host.slot.is_none());
}

/// A phone with the wrong PSK cannot read the host's message 2, so it can never send `Confirm`. The
/// host must show nothing to the person, and its pairing slot must be free again once the channel
/// is refused (a second phone can then pair).
#[test]
fn a_wrong_psk_never_reaches_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let phone = new_phone();
    let wrong = [9u8; 32];
    let pid = host.slot.as_ref().unwrap().pairing_id;
    host.on_frame(&open_frame(1, OpenInfo::Pairing(pid)), NOW);
    let mut hs = pair_initiator(&phone, &host.host_pub, &host.host_id, &wrong);
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf).unwrap();
    let outs = host.on_frame(&data_frame(1, &buf[..n]), NOW);
    let answer = data_on(&outs, 1).first().cloned().expect("the host answers message 1");
    let mut payload = vec![0u8; 1024];
    assert!(hs.read_message(&answer, &mut payload).is_err(), "the phone refuses message 2 under the wrong PSK");
    assert!(!outs.iter().any(|o| matches!(o, Out::Pending { .. })), "nothing is shown after message 1");
    // The best such a phone can do is send something else: the host refuses it, shows nothing, and
    // closes the channel.
    let outs = host.on_frame(&data_frame(1, &[7u8; 90]), NOW);
    assert!(!outs.iter().any(|o| matches!(o, Out::Pending { .. })), "garbage shows nothing");
    assert_eq!(closed_on(&outs, 1), Some(Some(RefusalCode::BadRequest)));
    assert!(host.devices.is_empty());
    // The slot is free: a second phone can open a channel on it.
    let outs = host.on_frame(&open_frame(2, OpenInfo::Pairing(pid)), NOW);
    assert!(closed_on(&outs, 2).is_none(), "the slot admits a second phone after the refusal");
}

/// The host refuses a `Confirm` that does not match its own check code, and a `Hello` sent before
/// the `Confirm`. Neither shows anything.
#[test]
fn a_confirm_with_the_wrong_code_or_a_hello_before_it_shows_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let psk = host.slot.as_ref().unwrap().psk;
    let phone = new_phone();
    let pid = host.slot.as_ref().unwrap().pairing_id;
    host.on_frame(&open_frame(1, OpenInfo::Pairing(pid)), NOW);
    let mut hs = pair_initiator(&phone, &host.host_pub, &host.host_id, &psk);
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf).unwrap();
    let outs = host.on_frame(&data_frame(1, &buf[..n]), NOW);
    let answer = data_on(&outs, 1).first().cloned().unwrap();
    let mut payload = vec![0u8; 1024];
    hs.read_message(&answer, &mut payload).unwrap();
    let mut t = hs.into_transport_mode().unwrap();
    let hello = serde_json::to_vec(&PairMsg::Hello { v: 1, name: "Early".into() }).unwrap();
    let outs = host.on_frame(&data_frame(1, &seal(&mut t, &hello)), NOW);
    assert!(!outs.iter().any(|o| matches!(o, Out::Pending { .. })), "a Hello before Confirm shows nothing");
    assert_eq!(closed_on(&outs, 1), Some(Some(RefusalCode::BadRequest)));

    host.on_frame(&open_frame(2, OpenInfo::Pairing(pid)), NOW);
    let mut hs = pair_initiator(&phone, &host.host_pub, &host.host_id, &psk);
    let n = hs.write_message(&[], &mut buf).unwrap();
    let outs = host.on_frame(&data_frame(2, &buf[..n]), NOW);
    let answer = data_on(&outs, 2).first().cloned().unwrap();
    hs.read_message(&answer, &mut payload).unwrap();
    let mut t = hs.into_transport_mode().unwrap();
    let wrong = serde_json::to_vec(&PairMsg::Confirm { check: "0000".into() }).unwrap();
    let outs = host.on_frame(&data_frame(2, &seal(&mut t, &wrong)), NOW);
    assert!(!outs.iter().any(|o| matches!(o, Out::Pending { .. })), "a Confirm with another code shows nothing");
    assert_eq!(closed_on(&outs, 2), Some(Some(RefusalCode::BadRequest)));
    assert!(host.devices.is_empty());
}

/// Message 1 must be empty now that the name comes after the confirmation. An old-style phone that
/// puts its Hello in message 1 is refused.
#[test]
fn a_hello_in_message_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let psk = host.slot.as_ref().unwrap().psk;
    let phone = new_phone();
    let pid = host.slot.as_ref().unwrap().pairing_id;
    host.on_frame(&open_frame(1, OpenInfo::Pairing(pid)), NOW);
    let mut hs = pair_initiator(&phone, &host.host_pub, &host.host_id, &psk);
    let mut buf = vec![0u8; 1024];
    let hello = serde_json::to_vec(&PairMsg::Hello { v: 1, name: "Old".into() }).unwrap();
    let n = hs.write_message(&hello, &mut buf).unwrap();
    let outs = host.on_frame(&data_frame(1, &buf[..n]), NOW);
    assert_eq!(closed_on(&outs, 1), Some(Some(RefusalCode::BadRequest)));
    assert!(!outs.iter().any(|o| matches!(o, Out::Pending { .. })));
}

/// Every message in each direction is counted, and both ends rekey on the same count. Lowered to 3
/// here so a short exchange crosses it several times; the product uses `REKEY_INTERVAL`.
#[test]
fn the_channel_rekeys_on_both_ends_at_the_same_count() {
    let (_dir, mut host, _calls, phone, _device_id, token) = host_with_device(Level::Operator, config());
    host.rekey_every = 3;
    let (mut t, snapshot_messages) = connect_device_counted(&mut host, &phone, &token, 2).expect("a paired phone connects");
    // The snapshot after the handshake is one transport message (one part), counted by the host
    // and by the phone alike.
    assert_eq!(snapshot_messages, 1, "one snapshot part arrives after the handshake");
    let (mut sent, mut received) = (0u64, snapshot_messages);
    for id in 1..=10u64 {
        let plain = serde_json::to_vec(&Request { id, op: Op::Ping }).unwrap();
        let cipher = seal(&mut t, &plain);
        sent += 1;
        if sent.is_multiple_of(3) {
            t.rekey_outgoing();
        }
        let mut outs = host.on_frame(&data_frame(2, &cipher), NOW);
        outs.extend(host.wait_idle());
        let reply = data_on(&outs, 2).first().cloned().unwrap_or_else(|| panic!("the host answers request {id}"));
        received += 1;
        let plain = open_msg(&mut t, &reply).unwrap_or_else(|| panic!("the phone reads answer {id} after the rekeys"));
        if received.is_multiple_of(3) {
            t.rekey_incoming();
        }
        match ServerMsg::parse(&plain) {
            Some(ServerMsg::Response(r)) => assert!(r.ok && r.id == id, "answer {id} is the answer to request {id}"),
            other => panic!("expected a response to {id}, got {other:?}"),
        }
    }
    assert_eq!((sent, received), (10, 11), "ten requests, and the snapshot plus ten answers received");
}

#[test]
fn a_second_pairing_channel_while_one_is_open_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let psk = host.slot.as_ref().unwrap().psk;
    let phone = new_phone();
    let (_t, _, _) = pair_until_waiting(&mut host, &phone, 1, "First", &psk);
    let pid = host.slot.as_ref().unwrap().pairing_id;
    let outs = host.on_frame(&open_frame(2, OpenInfo::Pairing(pid)), NOW);
    assert_eq!(closed_on(&outs, 2), Some(Some(RefusalCode::NotAllowed)), "one pairing channel at a time");
}

#[test]
fn the_sixth_attempt_on_a_slot_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let pid = host.slot.as_ref().unwrap().pairing_id;
    for ch in 1..=5u32 {
        let outs = host.on_frame(&open_frame(ch, OpenInfo::Pairing(pid)), NOW);
        assert!(closed_on(&outs, ch).is_none(), "attempt {ch} is admitted");
        host.on_frame(&encode_frame(FrameKind::Close, ch, &[0, 0]), NOW);
    }
    let outs = host.on_frame(&open_frame(6, OpenInfo::Pairing(pid)), NOW);
    assert_eq!(closed_on(&outs, 6), Some(Some(RefusalCode::NotAllowed)));
}

#[test]
fn an_expired_offer_admits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let pid = host.slot.as_ref().unwrap().pairing_id;
    let outs = host.on_frame(&open_frame(1, OpenInfo::Pairing(pid)), NOW + 601);
    assert_eq!(closed_on(&outs, 1), Some(Some(RefusalCode::NotAllowed)));
}

#[test]
fn the_offer_names_this_host_and_its_psk() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    let (url, outs) = host.start_pairing(NOW, "0.1.0").unwrap();
    assert!(outs.iter().any(|o| matches!(o, Out::Control(HostToRelay::PairingOpen { .. }))));
    let fragment = url.split_once("#p=").map(|(_, f)| f.to_owned()).unwrap_or_default();
    let offer = csm_remote_proto::pairing::PairingOffer::decode(&fragment).unwrap();
    assert_eq!(offer.psk().unwrap(), host.slot.as_ref().unwrap().psk);
    assert_eq!(offer.host_pub().unwrap(), host.host_pub);
    assert_eq!(offer.h, host.host_id);
}

// ---- levels, ops and the fixed mapping --------------------------------------------------

#[test]
fn a_viewer_is_refused_every_operator_op_and_nothing_reaches_csm() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Viewer, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let before = tools_called(&calls).len();
    let r = request(&mut host, 2, &mut t, 1, Op::SessionWake { session_id: "s1".into() }).unwrap();
    assert!(!r.ok);
    assert_eq!(r.error.unwrap().code, msg::ErrorCode::Forbidden);
    assert_eq!(tools_called(&calls).len(), before, "the level check runs before any CSM call");
    let r = request(&mut host, 2, &mut t, 2, Op::SessionList).unwrap();
    assert!(r.ok, "a viewer may read");
}

#[test]
fn each_read_op_makes_exactly_its_one_csm_call() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let cases: Vec<(Op, &str)> = vec![
        (Op::SessionList, "session_list"),
        (Op::SessionGet { session_id: "s1".into() }, "session_get"),
        (Op::InboxList, "inbox_list"),
        (Op::TaskList, "task_list"),
        (Op::TaskGet { task_id: "t1".into() }, "task_get"),
    ];
    for (i, (op, tool)) in cases.into_iter().enumerate() {
        calls.log.lock().unwrap().clear();
        let r = request(&mut host, 2, &mut t, 10 + i as u64, op).unwrap();
        assert!(r.ok, "{tool} answers");
        assert_eq!(tools_called(&calls), vec![tool.to_owned()], "one call, to {tool}");
    }
}

#[test]
fn session_get_returns_the_message_and_no_csm_json() {
    let (_dir, mut host, _calls, phone, _id, token) = host_with_device(Level::Viewer, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let r = request(&mut host, 2, &mut t, 1, Op::SessionGet { session_id: "s1".into() }).unwrap();
    let data = r.data.unwrap();
    assert_eq!(data["message"], "Waiting for you");
    assert!(data.get("folder").is_some());
}

#[test]
fn session_new_only_takes_an_allow_listed_folder_and_profile_and_passes_its_spelling() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let bad_folder = request(&mut host, 2, &mut t, 1, Op::SessionNew { folder: "/work/app/../secret".into(), profile: "claude".into(), name: None, remote_control: false }).unwrap();
    assert_eq!(bad_folder.error.unwrap().code, msg::ErrorCode::FolderNotAllowed);
    let prefix = request(&mut host, 2, &mut t, 2, Op::SessionNew { folder: "/work/app/sub".into(), profile: "claude".into(), name: None, remote_control: false }).unwrap();
    assert_eq!(prefix.error.unwrap().code, msg::ErrorCode::FolderNotAllowed, "a prefix never matches");
    let bad_profile = request(&mut host, 2, &mut t, 3, Op::SessionNew { folder: "/work/app".into(), profile: "shell; rm".into(), name: None, remote_control: false }).unwrap();
    assert_eq!(bad_profile.error.unwrap().code, msg::ErrorCode::UnknownProfile);
    assert_eq!(count(&calls, "session_new"), 0, "no launch for a refused request");
    let ok = request(&mut host, 2, &mut t, 4, Op::SessionNew { folder: "/work/app/".into(), profile: "claude".into(), name: Some("a\u{202E}b".into()), remote_control: false }).unwrap();
    assert!(ok.ok);
    let sent = calls.log.lock().unwrap().iter().find(|(t, _)| t == "session_new").map(|(_, p)| p.clone()).unwrap();
    assert_eq!(sent["folder"], "/work/app", "the allow-list entry's own spelling, never the phone's");
    assert_eq!(sent["command"], "claude", "the command comes from the profile");
    assert_eq!(sent["name"], "ab", "the name is cleaned of bidi characters");
}

#[test]
fn session_command_only_sends_commands_on_both_lists() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let refused = request(&mut host, 2, &mut t, 1, Op::SessionCommand { session_id: "s1".into(), command: "/clear".into(), force: false }).unwrap();
    assert_eq!(refused.error.unwrap().code, msg::ErrorCode::CommandNotAllowed, "/clear is on the PC but not on the add-on's list");
    assert_eq!(count(&calls, "session_send_command"), 0);
    let ok = request(&mut host, 2, &mut t, 2, Op::SessionCommand { session_id: "s1".into(), command: "/remote-control".into(), force: false }).unwrap();
    assert!(ok.ok);
    assert_eq!(count(&calls, "session_send_command"), 1);
}

#[test]
fn a_push_to_an_unlisted_host_is_refused_before_it_is_stored() {
    let (dir, mut host, _calls, phone, _id, token) = host_with_device(Level::Viewer, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let r = request(&mut host, 2, &mut t, 1, Op::PushSubscribe { endpoint: "https://evil.example/x".into(), p256dh: "AAAA".into(), auth: "AAAA".into() }).unwrap();
    assert!(!r.ok);
    assert!(host.devices[0].push.is_none());
    let r = request(&mut host, 2, &mut t, 2, Op::PushSubscribe { endpoint: "https://fcm.googleapis.com/x".into(), p256dh: "AAAA".into(), auth: "AAAA".into() }).unwrap();
    assert!(r.ok);
    assert!(store::open(dir.path()).unwrap().devices[0].push.is_some(), "the subscription is saved");
}

#[test]
fn the_launch_bucket_refuses_the_seventh_launch_and_never_calls_csm() {
    let mut cfg = config();
    cfg.max_phone_sessions = 20; // the live cap (5 by default) is raised: only the rate limit is in play
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Operator, cfg);
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let mut admitted = 0;
    for i in 0..7u64 {
        let r = request(&mut host, 2, &mut t, 100 + i, Op::SessionNew { folder: "/work/app".into(), profile: "claude".into(), name: None, remote_control: false }).unwrap();
        if r.ok {
            admitted += 1;
        } else {
            assert_eq!(r.error.unwrap().code, msg::ErrorCode::RateLimited);
        }
    }
    assert_eq!(admitted, 6);
    assert_eq!(count(&calls, "session_new"), 6, "the refused launch never reaches session_new");
}

#[test]
fn the_live_session_cap_refuses_a_new_phone_session_until_one_exits() {
    let mut cfg = config();
    cfg.max_phone_sessions = 1;
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Operator, cfg);
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let new = |host: &mut Host, t: &mut TransportState, id: u64| request(host, 2, t, id, Op::SessionNew { folder: "/work/app".into(), profile: "claude".into(), name: None, remote_control: false }).unwrap();
    assert!(new(&mut host, &mut t, 1).ok);
    let second = new(&mut host, &mut t, 2);
    assert_eq!(second.error.unwrap().code, msg::ErrorCode::RateLimited, "the cap of one is reached");
    assert_eq!(count(&calls, "session_new"), 1);
    calls.sessions.lock().unwrap()[0]["status"] = json!("exited");
    assert!(new(&mut host, &mut t, 3).ok, "a session that exited frees the cap");
}

#[test]
fn a_hostile_session_name_is_cleaned_before_it_reaches_the_phone() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Viewer, config());
    calls.sessions.lock().unwrap().push(json!({"id": "x", "name": "ok\u{202E}\u{200B}evil\u{0}", "folder": "/work/app", "status": "needs_you_approval"}));
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let r = request(&mut host, 2, &mut t, 1, Op::SessionList).unwrap();
    let names: Vec<String> = r.data.unwrap()["sessions"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(names, vec!["okevil".to_owned()]);
}

#[test]
fn an_oversize_request_is_answered_with_too_large_and_not_run() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let big = format!("{{\"id\":5,\"op\":\"session.list\",\"args\":{{\"x\":\"{}\"}}}}", "a".repeat(MAX_APP_MESSAGE_BYTES));
    let outs = host.on_frame(&data_frame(2, &seal(&mut t, big.as_bytes())), NOW);
    let reply = open_msg(&mut t, &data_on(&outs, 2)[0]).unwrap();
    let ServerMsg::Response(r) = ServerMsg::parse(&reply).unwrap() else { panic!("a response") };
    assert_eq!(r.error.unwrap().code, msg::ErrorCode::TooLarge);
    assert_eq!(count(&calls, "session_list"), 0);
}

// ---- revoke and level changes -------------------------------------------------------------

#[test]
fn revoke_says_bye_and_closes_the_channel_on_the_host_itself() {
    let (dir, mut host, _calls, phone, device_id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let outs = host.revoke(&device_id);
    let bye = open_msg(&mut t, &data_on(&outs, 2)[0]).unwrap();
    assert_eq!(ServerMsg::parse(&bye), Some(ServerMsg::Event(Event::Bye { reason: ByeReason::Revoked })));
    assert_eq!(closed_on(&outs, 2), Some(None), "the host closes the channel itself");
    assert!(outs.iter().any(|o| matches!(o, Out::Control(HostToRelay::Allow { tokens }) if tokens.is_empty())));
    assert!(store::open(dir.path()).unwrap().devices.is_empty());
    assert!(connect_device(&mut host, &phone, &token, 3).is_none(), "the revoked phone cannot reconnect");
}

#[test]
fn a_level_change_says_bye_level_changed_and_the_phone_reconnects_at_the_new_level() {
    let (_dir, mut host, calls, phone, device_id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let outs = host.set_level(&device_id, Level::Viewer);
    let bye = open_msg(&mut t, &data_on(&outs, 2)[0]).unwrap();
    assert_eq!(ServerMsg::parse(&bye), Some(ServerMsg::Event(Event::Bye { reason: ByeReason::LevelChanged })));
    let mut t2 = connect_device(&mut host, &phone, &token, 4).unwrap();
    let r = request(&mut host, 4, &mut t2, 1, Op::SessionWake { session_id: "s1".into() }).unwrap();
    assert_eq!(r.error.unwrap().code, msg::ErrorCode::Forbidden);
    assert_eq!(count(&calls, "session_wake"), 0);
}

#[test]
fn a_phone_with_a_different_key_is_refused_even_with_a_paired_token() {
    let (_dir, mut host, _calls, _phone, _id, token) = host_with_device(Level::Operator, config());
    let impostor = new_phone();
    assert!(connect_device(&mut host, &impostor, &token, 2).is_none(), "the static key must match the device's");
}

// ---- hostile input -------------------------------------------------------------------------

#[test]
fn hostile_frames_never_panic_and_leave_the_host_usable() {
    let (_dir, mut host, _calls, phone, _id, token) = host_with_device(Level::Operator, config());
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..4000u32 {
        let len = (next() % 300) as usize;
        let mut bytes: Vec<u8> = (0..len).map(|_| (next() & 0xff) as u8).collect();
        if i % 3 == 0 && bytes.len() >= 6 {
            bytes[0] = 1;
            bytes[1] = 1 + (i % 3) as u8;
        }
        let _ = host.on_frame(&bytes, NOW);
    }
    let _ = host.on_frame(&data_frame(77, &[0u8; 10]), NOW);
    let _ = host.on_frame(&open_frame(78, OpenInfo::Pairing([0; 16])), NOW);
    let big = vec![0u8; MAX_NOISE_MESSAGE + 1];
    let outs = host.on_frame(&data_frame(79, &big), NOW);
    assert!(outs.is_empty() || closed_on(&outs, 79).is_some());
    let mut t = connect_device(&mut host, &phone, &token, 3).expect("the host still serves paired phones");
    assert!(request(&mut host, 3, &mut t, 1, Op::Ping).unwrap().ok);
}

#[test]
fn a_damaged_plaintext_is_answered_not_crashed_on() {
    let (_dir, mut host, _calls, phone, _id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    for garbage in [&b"not json"[..], b"{\"id\":1}", b"{\"id\":1,\"op\":\"nope\"}", b"{\"op\":\"ping\"}", b"[]"] {
        let outs = host.on_frame(&data_frame(2, &seal(&mut t, garbage)), NOW);
        let reply = open_msg(&mut t, &data_on(&outs, 2)[0]).unwrap();
        assert!(ServerMsg::parse(&reply).is_some(), "a refusal is a well-formed response");
    }
}

#[test]
fn tick_closes_channels_older_than_the_maximum_age() {
    let (_dir, mut host, _calls, phone, _id, token) = host_with_device(Level::Operator, config());
    let _t = connect_device(&mut host, &phone, &token, 2).unwrap();
    let outs = host.tick(NOW + csm_remote_proto::noise::MAX_CHANNEL_SECS);
    assert_eq!(closed_on(&outs, 2), Some(None));
}

// ---- the ops worker: CSM never holds the relay loop -------------------------------------------

/// A slow CSM must not hold `on_frame`: the request is queued, and the answer comes later from
/// `tick` (or `wait_idle`). The third request in a row over the per-channel cap of 8 is refused
/// at once, not queued behind the slow calls.
#[test]
fn a_slow_csm_never_holds_the_relay_loop_and_the_cap_answers_at_once() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    // The worker runs the calls one after another, so eight of these take about 1.6 s in all.
    calls.delay_ms.store(200, Ordering::SeqCst);
    let started = Instant::now();
    let mut refused = 0;
    for id in 1..=10u64 {
        let plain = serde_json::to_vec(&Request { id, op: Op::SessionList }).unwrap();
        let outs = host.on_frame(&data_frame(2, &seal(&mut t, &plain)), NOW);
        for frame in data_on(&outs, 2) {
            let ServerMsg::Response(r) = ServerMsg::parse(&open_msg(&mut t, &frame).unwrap()).unwrap() else { panic!("a response") };
            if r.error.as_ref().map(|e| e.code) == Some(msg::ErrorCode::RateLimited) {
                refused += 1;
            }
        }
    }
    let mut answered = data_on(&host.tick(NOW), 2).len();
    assert!(started.elapsed() < Duration::from_millis(1000), "ten requests and a tick returned while CSM sleeps: {:?}", started.elapsed());
    assert_eq!(refused, 2, "the 9th and 10th requests are refused at once (cap of 8 per channel)");
    answered += data_on(&host.wait_idle(), 2).len();
    assert_eq!(answered, 8, "the eight queued requests are each answered once, when CSM answers");
}

/// A revoke while a call is running closes the channel itself. The answer that comes back later is
/// not sent on that channel, and the device is not brought back by the copy the worker returns.
#[test]
fn a_revoke_during_a_running_call_closes_the_channel_and_the_answer_is_dropped() {
    let (dir, mut host, calls, phone, device_id, token) = host_with_device(Level::Operator, config());
    let mut t = connect_device(&mut host, &phone, &token, 2).unwrap();
    calls.delay_ms.store(300, Ordering::SeqCst);
    let plain = serde_json::to_vec(&Request { id: 1, op: Op::SessionList }).unwrap();
    let outs = host.on_frame(&data_frame(2, &seal(&mut t, &plain)), NOW);
    assert!(data_on(&outs, 2).is_empty(), "no answer before the worker has one");
    let revoke_outs = host.revoke(&device_id);
    assert!(closed_on(&revoke_outs, 2).is_some(), "the revoke closes the channel on the host");
    let later = host.wait_idle();
    assert!(data_on(&later, 2).is_empty(), "no answer is sent on a channel the revoke closed");
    assert!(host.devices.is_empty(), "the answer does not bring the revoked device back");
    assert!(store::open(dir.path()).unwrap().devices.is_empty(), "nor the saved list");
}

// ---- the session tracker, deltas, snapshots and pushes -------------------------------------

/// The RFC 8291 example's phone keys (public: `UA_PUBLIC`, auth secret), so a test can open the body.
const UA_PUBLIC_B64: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const UA_PRIVATE_B64: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const AUTH_B64: &str = "BTBZMqHH6r4Tts7J_aSIgg";

fn session_json(id: &str, name: &str, status: &str) -> Value {
    json!({"id": id, "name": name, "folder": "/work/app", "status": status})
}

fn push_sub() -> PushSub {
    PushSub { endpoint: "https://push.example.net/send/abc".into(), p256dh: UA_PUBLIC_B64.into(), auth: AUTH_B64.into() }
}

/// Turns pushes on for this host, to one listed push service.
fn pushes_on(host: &mut Host) {
    host.cfg.push = true;
    host.cfg.push_hosts = vec!["push.example.net".into()];
}

/// Every app message the phone reads on `ch` from `outs`, in order (responses and events).
fn phone_reads(t: &mut TransportState, outs: &[Out], ch: u32) -> Vec<ServerMsg> {
    data_on(outs, ch).iter().filter_map(|c| open_msg(t, c)).filter_map(|p| ServerMsg::parse(&p)).collect()
}

fn pushes_in(outs: &[Out]) -> Vec<PushRequest> {
    outs.iter()
        .filter_map(|o| match o {
            Out::Control(HostToRelay::Push(p)) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

/// The payload a push carries, opened with the phone's own keys.
fn open_push(p: &PushRequest) -> Value {
    let body = b64_decode(&p.body).unwrap();
    let ua_private: [u8; 32] = b64_decode_fixed(UA_PRIVATE_B64).unwrap();
    let plain = push::decrypt(&body, &ua_private, &b64_decode(AUTH_B64).unwrap()).unwrap();
    serde_json::from_slice(&plain).unwrap()
}

#[test]
fn a_phone_gets_the_changes_to_the_session_list_and_nothing_else() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Viewer, config());
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "working")];
    let (mut t, _) = connect_device_counted(&mut host, &phone, &token, 2).expect("a paired phone connects");
    // The first read of CSM's list: the phone's view was empty, so the list is one delta, seq 2.
    host.tick(NOW);
    let outs = host.wait_idle();
    match phone_reads(&mut t, &outs, 2).as_slice() {
        [ServerMsg::Event(Event::Delta { seq: 2, upserts, removed })] => {
            assert_eq!(upserts.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["a"]);
            assert!(removed.is_empty());
        }
        other => panic!("expected the delta, got {other:?}"),
    }
    // A read that finds the same list sends nothing.
    host.tick(NOW + tracker::REFRESH_SECS);
    let outs = host.wait_idle();
    assert!(phone_reads(&mut t, &outs, 2).is_empty(), "an unchanged list sends nothing");
    // One session goes and another comes: one delta with both, the next seq.
    *calls.sessions.lock().unwrap() = vec![session_json("b", "Beta", "idle")];
    host.tick(NOW + 2 * tracker::REFRESH_SECS);
    let outs = host.wait_idle();
    match phone_reads(&mut t, &outs, 2).as_slice() {
        [ServerMsg::Event(Event::Delta { seq: 3, upserts, removed })] => {
            assert_eq!(upserts.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["b"]);
            assert_eq!(removed, &vec!["a".to_owned()]);
        }
        other => panic!("expected the second delta, got {other:?}"),
    }
}

#[test]
fn a_snapshot_request_is_answered_from_the_tracker_without_a_csm_call() {
    let (_dir, mut host, calls, phone, _id, token) = host_with_device(Level::Viewer, config());
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "working")];
    let (mut t, _) = connect_device_counted(&mut host, &phone, &token, 2).expect("a paired phone connects");
    host.tick(NOW);
    // The list reaches the phone as a delta; the phone reads it, so the transport stays in step.
    let delta = host.wait_idle();
    assert_eq!(phone_reads(&mut t, &delta, 2).len(), 1, "the phone reads the first delta");
    let before = count(&calls, "session_list");
    let plain = serde_json::to_vec(&Request { id: 9, op: Op::Snapshot }).unwrap();
    let mut outs = host.on_frame(&data_frame(2, &seal(&mut t, &plain)), NOW);
    outs.extend(host.wait_idle());
    let msgs = phone_reads(&mut t, &outs, 2);
    assert!(matches!(msgs.first(), Some(ServerMsg::Response(r)) if r.ok && r.id == 9), "the request is answered first: {msgs:?}");
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Event(Event::Snapshot { sessions, .. }) if sessions.len() == 1)), "then the list");
    assert_eq!(count(&calls, "session_list"), before, "a snapshot is served from the tracker, not from CSM");
}

#[test]
fn a_session_that_starts_to_need_you_is_pushed_once_to_a_device_that_asked() {
    let (_dir, mut host, calls, _phone, _id, _token) = host_with_device(Level::Viewer, config());
    pushes_on(&mut host);
    host.devices[0].push = Some(push_sub());
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "working")];
    host.tick(NOW);
    host.wait_idle();
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "needs_you_approval")];
    host.tick(NOW + 2);
    let pushes = pushes_in(&host.wait_idle());
    assert_eq!(pushes.len(), 1, "one push for the episode");
    let payload = open_push(&pushes[0]);
    assert_eq!(payload["t"], "needs_you");
    assert_eq!(payload["n"], 1);
    assert_eq!(payload["s"], "Alpha", "the newest session is named");
    assert_eq!(payload["sid"], "a");
    assert_eq!(pushes[0].topic.as_deref(), Some("needs-you"));
    assert_eq!(pushes[0].endpoint, "https://push.example.net/send/abc");
    // The same state again is the same episode: no second push.
    host.tick(NOW + 4);
    assert!(pushes_in(&host.wait_idle()).is_empty());
}

#[test]
fn a_device_that_hides_names_gets_a_count_and_no_name() {
    let (_dir, mut host, calls, _phone, _id, _token) = host_with_device(Level::Viewer, config());
    pushes_on(&mut host);
    host.devices[0].push = Some(push_sub());
    host.devices[0].hide_names = true;
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "working")];
    host.tick(NOW);
    host.wait_idle();
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "needs_you_question")];
    host.tick(NOW + 2);
    let pushes = pushes_in(&host.wait_idle());
    assert_eq!(pushes.len(), 1);
    let payload = open_push(&pushes[0]);
    assert_eq!(payload["n"], 1, "the count is always sent");
    assert!(payload.get("s").is_none() && payload.get("sid").is_none(), "no name and no id: {payload}");
}

#[test]
fn a_restart_does_not_push_for_a_session_already_waiting() {
    let (_dir, mut host, calls, _phone, _id, _token) = host_with_device(Level::Viewer, config());
    pushes_on(&mut host);
    host.devices[0].push = Some(push_sub());
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "needs_you_approval")];
    host.tick(NOW);
    assert!(pushes_in(&host.wait_idle()).is_empty(), "the first read is the baseline and sends nothing");
}

#[test]
fn a_second_episode_inside_the_window_is_held_and_sent_when_the_window_opens() {
    let (_dir, mut host, calls, _phone, _id, _token) = host_with_device(Level::Viewer, config());
    pushes_on(&mut host);
    host.devices[0].push = Some(push_sub());
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "working")];
    host.tick(NOW);
    host.wait_idle();
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "needs_you_approval")];
    host.tick(NOW + 2);
    assert_eq!(pushes_in(&host.wait_idle()).len(), 1, "the first episode is pushed at once");
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "working")];
    host.tick(NOW + 4);
    host.wait_idle();
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "needs_you_question")];
    host.tick(NOW + 6);
    assert!(pushes_in(&host.wait_idle()).is_empty(), "inside the 30 second window the push is held");
    let outs = host.tick(NOW + 2 + 30);
    let held = pushes_in(&outs);
    assert_eq!(held.len(), 1, "the held push is sent when the window opens");
    assert_eq!(open_push(&held[0])["sid"], "a");
}

#[test]
fn a_push_service_that_says_the_subscription_is_gone_drops_it() {
    let (dir, mut host, calls, _phone, _id, _token) = host_with_device(Level::Viewer, config());
    pushes_on(&mut host);
    host.devices[0].push = Some(push_sub());
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "working")];
    host.tick(NOW);
    host.wait_idle();
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "needs_you_approval")];
    host.tick(NOW + 2);
    let pushes = pushes_in(&host.wait_idle());
    assert_eq!(pushes.len(), 1);
    let outs = host.on_relay(&RelayToHost::PushResult { id: pushes[0].id, ok: false, status: Some(410), code: None });
    assert!(outs.iter().any(|o| matches!(o, Out::DevicesChanged)));
    assert!(host.devices[0].push.is_none(), "the device stops getting pushes");
    assert!(store::open(dir.path()).unwrap().devices[0].push.is_none(), "and that is saved");
}

#[test]
fn a_push_result_that_is_not_gone_keeps_the_subscription() {
    let (_dir, mut host, calls, _phone, _id, _token) = host_with_device(Level::Viewer, config());
    pushes_on(&mut host);
    host.devices[0].push = Some(push_sub());
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "working")];
    host.tick(NOW);
    host.wait_idle();
    *calls.sessions.lock().unwrap() = vec![session_json("a", "Alpha", "needs_you_approval")];
    host.tick(NOW + 2);
    let pushes = pushes_in(&host.wait_idle());
    host.on_relay(&RelayToHost::PushResult { id: pushes[0].id, ok: false, status: Some(500), code: None });
    assert!(host.devices[0].push.is_some(), "only 404 and 410 drop a subscription");
}

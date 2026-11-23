//! The host's protocol core, driven by a test phone that speaks the real Noise patterns with
//! `snow` and the shared wire types of `csm-remote-proto`. No sockets: frames go straight in and
//! out of [`Host`], and CSM is a fake that records every call.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use csm_phone::config::{Config, FolderEntry, ProfileEntry};
use csm_phone::csm::{Csm, CsmError};
use csm_phone::host::{Host, Out};
use csm_phone::store;
use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{b64_encode, token_hash};
use csm_remote_proto::limits::MAX_APP_MESSAGE_BYTES;
use csm_remote_proto::msg::{self, ByeReason, Event, Level, Op, Request, Response, ServerMsg};
use csm_remote_proto::noise::{MAX_NOISE_MESSAGE, PARAMS_PAIRING, PARAMS_SESSION, PSK_POSITION, check_code, prologue};
use csm_remote_proto::pairing::{DenyReason, PairMsg};
use csm_remote_proto::relay::{FrameKind, HostToRelay, OpenInfo, decode_frame, encode_frame, parse_close_payload};
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
}

struct Fake(Arc<Calls>);

impl Csm for Fake {
    fn call(&self, tool: &str, params: Value) -> Result<Value, CsmError> {
        self.0.log.lock().unwrap().push((tool.to_owned(), params.clone()));
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
            "session_get" => Ok(json!({"id": params["session_id"], "name": "A", "folder": "/work/app", "status": "idle", "message": "Waiting for you"})),
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

/// Opens and completes a pairing on channel `ch`; returns the phone's transport, the check code the
/// phone computed from its own handshake, and what the host sent.
fn pair_until_waiting(host: &mut Host, phone: &Phone, ch: u32, name: &str, psk: &[u8; 32]) -> (TransportState, String, Vec<Out>) {
    let pid = host.slot.as_ref().unwrap().pairing_id;
    let outs = host.on_frame(&open_frame(ch, OpenInfo::Pairing(pid)), NOW);
    assert!(outs.is_empty(), "the open is accepted without an answer");
    let mut hs = pair_initiator(phone, &host.host_pub, &host.host_id, psk);
    let mut buf = vec![0u8; 1024];
    let hello = serde_json::to_vec(&PairMsg::Hello { v: 1, name: name.into() }).unwrap();
    let n = hs.write_message(&hello, &mut buf).unwrap();
    let outs = host.on_frame(&data_frame(ch, &buf[..n]), NOW);
    let answer = data_on(&outs, ch).first().cloned().unwrap_or_default();
    let mut payload = vec![0u8; 1024];
    hs.read_message(&answer, &mut payload).unwrap();
    let check = check_code(hs.get_handshake_hash());
    let t = hs.into_transport_mode().unwrap();
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
    let reply = data_on(&outs, ch).first().cloned()?;
    let mut payload = vec![0u8; 1024];
    hs.read_message(&reply, &mut payload).ok()?;
    hs.into_transport_mode().ok()
}

fn request(host: &mut Host, ch: u32, t: &mut TransportState, id: u64, op: Op) -> Option<Response> {
    let plain = serde_json::to_vec(&Request { id, op }).unwrap();
    let outs = host.on_frame(&data_frame(ch, &seal(t, &plain)), NOW);
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

#[test]
fn a_wrong_psk_from_a_stale_or_forged_offer_is_refused_before_any_message() {
    let dir = tempfile::tempdir().unwrap();
    let (mut host, _calls) = host_in(dir.path(), config());
    host.start_pairing(NOW, "0.1.0").unwrap();
    let phone = new_phone();
    let wrong = [9u8; 32];
    let pid = host.slot.as_ref().unwrap().pairing_id;
    host.on_frame(&open_frame(1, OpenInfo::Pairing(pid)), NOW);
    let mut hs = pair_initiator(&phone, &host.host_pub, &host.host_id, &wrong);
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(b"{\"t\":\"hello\"}", &mut buf).unwrap();
    let outs = host.on_frame(&data_frame(1, &buf[..n]), NOW);
    assert_eq!(closed_on(&outs, 1), Some(Some(RefusalCode::BadRequest)));
    assert!(!outs.iter().any(|o| matches!(o, Out::Pending { .. })), "nothing is shown for a failed handshake");
    assert!(host.slot.as_ref().is_some_and(|s| !s.active), "the slot is free for the next attempt");
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

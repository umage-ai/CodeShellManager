//! Generates and checks `vectors/*.json`, the shared test vectors a JS implementation is checked
//! against. Run with `CSM_UPDATE_VECTORS=1` to rewrite them; otherwise the files on disk must equal
//! what the code produces now (so a protocol change cannot slip past the phone side unnoticed).
//!
//! The Noise vectors use fixed keys and fixed ephemerals (snow's testing hook), so every byte of a
//! handshake and transport run is reproducible.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{b64_encode, hex, host_id, sha256, token_hash};
use csm_remote_proto::msg::{
    ByeReason, CommandInfo, ErrorCode, Event, FolderInfo, HelloData, InboxItem, Level, PhoneStatus, ProfileInfo, PushKind, PushPayload, Request, Response,
    SessionInfo, TaskItem,
};
use csm_remote_proto::noise::{PARAMS_PAIRING, PARAMS_SESSION, REKEY_INTERVAL, check_code, prologue};
use csm_remote_proto::pairing::{DenyReason, PairMsg, PairingOffer};
use csm_remote_proto::relay::{FrameKind, HostToRelay, OpenInfo, PushRequest, RelayToClient, RelayToHost, Urgency, close_payload, encode_frame};
use serde_json::{Value, json};
use snow::Builder;
use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vectors").join(format!("{name}.json"))
}

fn check(name: &str, value: &Value) {
    let text = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
    let p = path(name);
    if std::env::var_os("CSM_UPDATE_VECTORS").is_some() {
        std::fs::write(&p, &text).unwrap();
        return;
    }
    let on_disk = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("{} is missing; run with CSM_UPDATE_VECTORS=1", p.display()));
    let parsed: Value = serde_json::from_str(&on_disk).unwrap();
    assert_eq!(&parsed, value, "{name}.json is out of date; run with CSM_UPDATE_VECTORS=1 and review the diff");
}

fn bytes(seed: u8, n: usize) -> Vec<u8> {
    (0..n).map(|i| seed.wrapping_mul(31).wrapping_add(u8::try_from(i % 251).unwrap_or(0)).wrapping_mul(7)).collect()
}

fn arr<const N: usize>(seed: u8) -> [u8; N] {
    bytes(seed, N).try_into().unwrap()
}

#[test]
fn ids_vectors() {
    let secrets = [arr::<32>(1), arr::<32>(2), [0u8; 32], [255u8; 32]];
    let hosts: Vec<Value> = secrets
        .iter()
        .map(|s| json!({"host_secret_hex": hex(s), "host_secret_b64": b64_encode(s), "host_id": host_id(s)}))
        .collect();
    let tokens: Vec<Value> = [arr::<32>(3), [0u8; 32]]
        .iter()
        .map(|t| json!({"routing_token_hex": hex(t), "routing_token_b64": b64_encode(t), "token_hash_hex": hex(&token_hash(t)), "token_hash_b64": b64_encode(&token_hash(t))}))
        .collect();
    let checks: Vec<Value> = [arr::<32>(4), arr::<32>(5), [0u8; 32]].iter().map(|h| json!({"handshake_hash_hex": hex(h), "check_code": check_code(h)})).collect();
    let v = json!({
        "doc": "host_id = base32_lower_nopad(SHA-256('csm-relay host' || host_secret))[..26]; token_hash = SHA-256('csm-relay token' || routing_token); check_code = 4 decimal digits of u32_be(SHA-256('csm-remote check' || handshake_hash)[0..4]) mod 10000; prologue = 'csm-remote/1' 0x00 host_id",
        "hosts": hosts,
        "tokens": tokens,
        "check_codes": checks,
        "prologue": {"host_id": host_id(&arr::<32>(1)), "prologue_hex": hex(&prologue(&host_id(&arr::<32>(1))))},
        "sha256_of_abc_hex": hex(&sha256(b"abc")),
    });
    check("ids", &v);
}

#[test]
fn codes_vectors() {
    let codes: Vec<Value> = RefusalCode::ALL.iter().map(|c| json!({"code": c.as_str(), "number": c.as_u16(), "ws_close_code": c.ws_close_code()})).collect();
    check("codes", &json!({"doc": "relay refusal codes; close frames carry the number, WebSocket close code = 4000 + number, reason = the name", "codes": codes}));
}

#[test]
fn frames_vectors() {
    let frames: Vec<Value> = [
        (FrameKind::Data, 1u32, b"opaque".to_vec()),
        (FrameKind::Data, 0xDEADBEEF, vec![]),
        (FrameKind::Open, 7, OpenInfo::Device(arr::<32>(9)).encode()),
        (FrameKind::Open, 8, OpenInfo::Pairing(arr::<16>(9)).encode()),
        (FrameKind::Close, 7, close_payload(None).to_vec()),
        (FrameKind::Close, 7, close_payload(Some(RefusalCode::Revoked)).to_vec()),
    ]
    .iter()
    .map(|(k, ch, payload)| {
        let kind = match k {
            FrameKind::Data => "data",
            FrameKind::Open => "open",
            FrameKind::Close => "close",
        };
        json!({"kind": kind, "channel": ch, "payload_hex": hex(payload), "frame_hex": hex(&encode_frame(*k, *ch, payload))})
    })
    .collect();
    let invalid = json!([
        {"frame_hex": "", "error": "too_short"},
        {"frame_hex": "0101000000", "error": "too_short"},
        {"frame_hex": "020100000001", "error": "version"},
        {"frame_hex": "01630000000100", "error": "unknown_kind"},
    ]);
    let control = json!({
        "host_hello": serde_json::to_value(HostToRelay::Hello { v: 1, host_id: host_id(&arr::<32>(1)), host_secret: b64_encode(&arr::<32>(1)), access: Some("enrol-secret".into()) }).unwrap(),
        "host_allow": serde_json::to_value(HostToRelay::Allow { tokens: vec![b64_encode(&token_hash(&arr::<32>(3)))] }).unwrap(),
        "host_pairing_open": serde_json::to_value(HostToRelay::PairingOpen { pairing_id: b64_encode(&arr::<16>(6)), expires_in: 600 }).unwrap(),
        "host_pairing_close": serde_json::to_value(HostToRelay::PairingClose).unwrap(),
        "host_push": serde_json::to_value(HostToRelay::Push(PushRequest { id: 1, endpoint: "https://fcm.googleapis.com/fcm/send/abc".into(), ttl: 60, urgency: Some(Urgency::High), topic: None, body: b64_encode(&bytes(7, 40)) })).unwrap(),
        "relay_push_result": serde_json::to_value(RelayToHost::PushResult { id: 1, ok: false, status: None, code: Some(RefusalCode::PushRefused) }).unwrap(),
        "relay_error": serde_json::to_value(RelayToHost::Error { code: RefusalCode::NotEnrolled }).unwrap(),
        "relay_ready": serde_json::to_value(RelayToClient::ready()).unwrap(),
        "relay_refusal": serde_json::to_value(RelayToClient::Error { code: RefusalCode::HostOffline }).unwrap(),
        "client_hello_token": json!({"t": "hello", "v": 1, "host_id": host_id(&arr::<32>(1)), "token": b64_encode(&arr::<32>(3))}),
        "client_hello_pairing": json!({"t": "hello", "v": 1, "host_id": host_id(&arr::<32>(1)), "pairing_id": b64_encode(&arr::<16>(6))}),
    });
    check(
        "frames",
        &json!({"doc": "host socket binary frame = version(1) kind(1: data, 2: open, 3: close) channel(u32 BE) payload; open payload = 0x00 token_hash(32) | 0x01 pairing_id(16); close payload = u16 BE refusal number or 0", "frames": frames, "invalid": invalid, "control": control}),
    );
}

#[test]
fn pairing_vectors() {
    let hid = host_id(&arr::<32>(1));
    let offer = PairingOffer::new("wss://relay.example.test", &hid, &arr::<32>(10), &arr::<16>(6), &arr::<32>(11));
    let enc = offer.encode();
    let pair_msgs = json!({
        "confirm": serde_json::to_value(PairMsg::Confirm { check: "0420".into() }).unwrap(),
        "hello": serde_json::to_value(PairMsg::Hello { v: 1, name: "Pixel 9".into() }).unwrap(),
        "accept": serde_json::to_value(PairMsg::Accept { routing_token: b64_encode(&arr::<32>(3)), level: Level::Operator, device_id: "d1".into() }).unwrap(),
        "deny": serde_json::to_value(PairMsg::Deny { reason: DenyReason::Denied }).unwrap(),
    });
    let v = json!({
        "doc": "QR payload = base64url_nopad(compact JSON of the offer, key order v,r,h,k,i,p); URL = <app_origin>/v/<version>/#p=<payload>",
        "offer": serde_json::to_value(&offer).unwrap(),
        "offer_json": serde_json::to_string(&offer).unwrap(),
        "encoded": enc,
        "url": offer.url("https://phone.example.test", "0.1.0"),
        "invalid": [
            {"input": "", "error": "format"},
            {"input": "p=!!!", "error": "format"},
            {"input": PairingOffer { r: "http://insecure.example".into(), ..offer.clone() }.encode(), "error": "field r"},
            {"input": PairingOffer { h: "short".into(), ..offer.clone() }.encode(), "error": "field h"},
            {"input": PairingOffer { v: 2, ..offer.clone() }.encode(), "error": "version 2"},
        ],
        "messages": pair_msgs,
    });
    check("pairing", &v);
}

fn session(id: &str, status: PhoneStatus) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        name: format!("api {id}"),
        folder: "C:/code/api".into(),
        status,
        branch: Some("main".into()),
        message: (status == PhoneStatus::NeedsApproval).then(|| "Allow edit?".into()),
        group: None,
        profile: Some("claude".into()),
        since: Some(1_760_000_000),
    }
}

#[test]
fn messages_vectors() {
    let valid = [
        r#"{"id":1,"op":"hello","args":{"client":"phone 0.1","protocol":1}}"#,
        r#"{"id":2,"op":"ping"}"#,
        r#"{"id":3,"op":"ping","args":{}}"#,
        r#"{"id":4,"op":"snapshot"}"#,
        r#"{"id":5,"op":"session.get","args":{"session_id":"s1"}}"#,
        r#"{"id":6,"op":"session.wake","args":{"session_id":"s1"}}"#,
        r#"{"id":7,"op":"session.close","args":{"session_id":"s1"}}"#,
        r#"{"id":8,"op":"session.close","args":{"session_id":"s1","force":true}}"#,
        r#"{"id":9,"op":"session.new","args":{"folder":"C:/code/api","profile":"claude","name":"fix","remote_control":true}}"#,
        r#"{"id":10,"op":"session.new","args":{"folder":"C:/code/api","profile":"claude","command":"calc.exe"}}"#,
        r#"{"id":11,"op":"session.command","args":{"session_id":"s1","command":"/remote-control"}}"#,
        r#"{"id":12,"op":"push.subscribe","args":{"endpoint":"https://fcm.googleapis.com/fcm/send/abc","p256dh":"BKey","auth":"sec"}}"#,
        r#"{"id":13,"op":"settings.set","args":{"hide_names":true,"notify":true}}"#,
        r#"{"id":14,"op":"folder.list"}"#,
    ];
    let valid: Vec<Value> = valid
        .iter()
        .map(|j| {
            let r = Request::parse(j.as_bytes()).unwrap();
            json!({"json": j, "parsed": serde_json::to_value(&r).unwrap(), "op": r.op.name(), "level": serde_json::to_value(r.op.required_level()).unwrap()})
        })
        .collect();
    let invalid_src = [
        "{",
        r#"{"op":"ping"}"#,
        r#"{"id":"x","op":"ping"}"#,
        r#"{"id":3,"op":"term.write","args":{"text":"rm -rf"}}"#,
        r#"{"id":4,"op":"session.wake","args":{}}"#,
        r#"{"id":5,"op":"session.wake","args":{"session_id":""}}"#,
        r#"{"id":6,"op":"session.command","args":{"session_id":"s","command":"remote-control"}}"#,
        r#"{"id":7,"op":"session.command","args":{"session_id":"s","command":"/a b"}}"#,
        r#"{"id":8,"op":"push.subscribe","args":{"endpoint":"http://x","p256dh":"k","auth":"a"}}"#,
    ];
    let invalid: Vec<Value> = invalid_src
        .iter()
        .map(|j| {
            let e = Request::parse(j.as_bytes()).unwrap_err();
            json!({"json": j, "id": e.id, "code": serde_json::to_value(e.code).unwrap()})
        })
        .collect();
    let responses = vec![
        serde_json::to_value(Response::ok(1, json!({"sessions": [serde_json::to_value(session("s1", PhoneStatus::Idle)).unwrap()]}))).unwrap(),
        serde_json::to_value(Response::err(2, ErrorCode::Forbidden, "operator level needed")).unwrap(),
        serde_json::to_value(Response::err(3, ErrorCode::Busy, "")).unwrap(),
    ];
    let hello = serde_json::to_value(HelloData { protocol: 1, level: Level::Viewer, host_name: "WORKPC".into(), hide_names: false, notify: true, push_ready: true, remote_control: true, max_message_bytes: 49152 }).unwrap();
    let events = vec![
        serde_json::to_value(Event::Snapshot { seq: 1, part: 1, parts: 1, sessions: vec![session("s1", PhoneStatus::NeedsApproval), session("s2", PhoneStatus::Asleep)], unread: Some(2) }).unwrap(),
        serde_json::to_value(Event::Delta { seq: 2, upserts: vec![session("s1", PhoneStatus::Working)], removed: vec!["s9".into()] }).unwrap(),
        serde_json::to_value(Event::Inbox { seq: 3, unread: 0 }).unwrap(),
        serde_json::to_value(Event::Bye { reason: ByeReason::Revoked }).unwrap(),
    ];
    let data = json!({
        "inbox_item": serde_json::to_value(InboxItem { id: "i1".into(), title: "api needs you".into(), session_id: Some("s1".into()), ts: 1_760_000_100 }).unwrap(),
        "task_item": serde_json::to_value(TaskItem { id: "t1".into(), title: "Fix login".into(), state: "in_progress".into(), session_id: None }).unwrap(),
        "folder": serde_json::to_value(FolderInfo { path: "C:/code/api".into(), name: "api".into(), pinned: true }).unwrap(),
        "profile": serde_json::to_value(ProfileInfo { name: "claude".into(), label: "Claude".into() }).unwrap(),
        "command": serde_json::to_value(CommandInfo { command: "/remote-control".into(), label: "Continue in the Claude app".into() }).unwrap(),
    });
    let push = json!({
        "with_name": serde_json::to_value(PushPayload { t: PushKind::NeedsYou, s: Some("api".into()), sid: Some("s1".into()), n: 2 }).unwrap(),
        "hidden": serde_json::to_value(PushPayload { t: PushKind::NeedsYou, s: None, sid: None, n: 2 }).unwrap(),
    });
    let statuses: Vec<Value> = [PhoneStatus::Working, PhoneStatus::NeedsApproval, PhoneStatus::NeedsQuestion, PhoneStatus::Idle, PhoneStatus::Done, PhoneStatus::Asleep, PhoneStatus::Exited]
        .iter()
        .map(|s| json!({"status": serde_json::to_value(s).unwrap(), "needs_you": s.needs_you()}))
        .collect();
    let v = json!({
        "doc": "app messages (JSON inside Noise transport messages). requests: parsed form is the canonical re-serialisation; level = least level allowed; invalid ones must be refused with that code (and answered with that id when not null)",
        "valid_requests": valid,
        "invalid_requests": invalid,
        "error_codes": ErrorCode::ALL.iter().map(|c| serde_json::to_value(c).unwrap()).collect::<Vec<_>>(),
        "ops": csm_remote_proto::msg::Op::NAMES,
        "responses": responses,
        "hello_data": hello,
        "events": events,
        "data": data,
        "push_payloads": push,
        "statuses": statuses,
        "max_message_bytes": csm_remote_proto::limits::MAX_APP_MESSAGE_BYTES,
    });
    check("messages", &v);
}

// ---- Noise -------------------------------------------------------------------------------

fn x25519_public(private: &[u8; 32]) -> Vec<u8> {
    let mut dh = DefaultResolver.resolve_dh(&DHChoice::Curve25519).unwrap();
    dh.set(private);
    dh.pubkey().to_vec()
}

struct Keys {
    is: [u8; 32],
    rs: [u8; 32],
    ei: [u8; 32],
    er: [u8; 32],
}

fn keys() -> Keys {
    Keys { is: arr(20), rs: arr(21), ei: arr(22), er: arr(23) }
}

fn builder<'a>(params: &str, k: &'a Keys, psk: Option<&'a [u8; 32]>, prologue: &'a [u8], initiator: bool, rs_pub: &'a [u8], ephemeral: &'a [u8; 32]) -> Builder<'a> {
    let mut b = Builder::new(params.parse().unwrap()).prologue(prologue).unwrap().fixed_ephemeral_key_for_testing_only(ephemeral);
    b = if initiator { b.local_private_key(&k.is).unwrap().remote_public_key(rs_pub).unwrap() } else { b.local_private_key(&k.rs).unwrap() };
    if let Some(psk) = psk {
        b = b.psk(2, psk).unwrap();
    }
    b
}

fn noise_vector(name: &str, params: &str, psk: Option<[u8; 32]>) -> Value {
    let k = keys();
    let hid = host_id(&arr::<32>(1));
    let pro = prologue(&hid);
    let rs_pub = x25519_public(&k.rs);
    let is_pub = x25519_public(&k.is);
    let mut init = builder(params, &k, psk.as_ref(), &pro, true, &rs_pub, &k.ei).build_initiator().unwrap();
    let mut resp = builder(params, &k, psk.as_ref(), &pro, false, &rs_pub, &k.er).build_responder().unwrap();

    let (p1, p2) = (b"phone".to_vec(), b"host".to_vec());
    let mut buf = vec![0u8; 65535];
    let mut out = vec![0u8; 65535];
    let n = init.write_message(&p1, &mut buf).unwrap();
    let msg1 = buf[..n].to_vec();
    let m = resp.read_message(&msg1, &mut out).unwrap();
    assert_eq!(&out[..m], &p1[..]);
    let n = resp.write_message(&p2, &mut buf).unwrap();
    let msg2 = buf[..n].to_vec();
    let m = init.read_message(&msg2, &mut out).unwrap();
    assert_eq!(&out[..m], &p2[..]);
    assert!(init.is_handshake_finished() && resp.is_handshake_finished());
    let hash = init.get_handshake_hash().to_vec();
    assert_eq!(hash, resp.get_handshake_hash());
    assert_eq!(init.get_remote_static().unwrap(), &rs_pub[..]);
    assert_eq!(resp.get_remote_static().unwrap(), &is_pub[..]);
    let mut ti = init.into_transport_mode().unwrap();
    let mut tr = resp.into_transport_mode().unwrap();

    let script: [(bool, Vec<u8>); 5] = [
        (true, b"hello".to_vec()),
        (false, b"world".to_vec()),
        (true, br#"{"id":1,"op":"hello","args":{"client":"phone 0.1","protocol":1}}"#.to_vec()),
        (false, vec![]),
        (true, bytes(5, 300)),
    ];
    let mut transport = Vec::new();
    for (from_init, plain) in &script {
        let (tx, rx) = if *from_init { (&mut ti, &mut tr) } else { (&mut tr, &mut ti) };
        let n = tx.write_message(plain, &mut buf).unwrap();
        let ct = buf[..n].to_vec();
        let m = rx.read_message(&ct, &mut out).unwrap();
        assert_eq!(&out[..m], &plain[..]);
        transport.push(json!({"from": if *from_init {"initiator"} else {"responder"}, "plaintext_hex": hex(plain), "ciphertext_hex": hex(&ct)}));
    }

    // Rekey: REKEY_INTERVAL + 2 initiator messages; both ends rekey that direction after message
    // number k * REKEY_INTERVAL. Record the ciphertexts around the boundary and a digest of all.
    let mut all = Vec::new();
    let mut around = Vec::new();
    for i in 1..=(REKEY_INTERVAL + 2) {
        let plain = format!("m{i}").into_bytes();
        let n = ti.write_message(&plain, &mut buf).unwrap();
        let ct = buf[..n].to_vec();
        let m = tr.read_message(&ct, &mut out).unwrap();
        assert_eq!(&out[..m], &plain[..]);
        if i == REKEY_INTERVAL {
            ti.rekey_outgoing();
            tr.rekey_incoming();
        }
        if i >= REKEY_INTERVAL - 1 {
            around.push(json!({"index": i, "plaintext_hex": hex(&plain), "ciphertext_hex": hex(&ct)}));
        }
        all.extend_from_slice(&ct);
    }

    json!({
        "params": params,
        "prologue_hex": hex(&pro),
        "psk_hex": psk.map(|p| hex(&p)),
        "initiator_static_private_hex": hex(&k.is),
        "initiator_static_public_hex": hex(&is_pub),
        "responder_static_private_hex": hex(&k.rs),
        "responder_static_public_hex": hex(&rs_pub),
        "initiator_ephemeral_private_hex": hex(&k.ei),
        "responder_ephemeral_private_hex": hex(&k.er),
        "message1_payload_hex": hex(&p1),
        "message1_hex": hex(&msg1),
        "message2_payload_hex": hex(&p2),
        "message2_hex": hex(&msg2),
        "handshake_hash_hex": hex(&hash),
        "check_code": check_code(&hash),
        "transport": transport,
        "rekey": {
            "doc": "initiator sends messages m1..m(N+2) (plaintext 'm' + decimal index) after the script above; both ends rekey the initiator->responder direction right after message N",
            "interval": REKEY_INTERVAL,
            "messages_sent": REKEY_INTERVAL + 2,
            "all_ciphertexts_sha256_hex": hex(&sha256(&all)),
            "around_boundary": around,
        },
        "name": name,
    })
}

#[test]
fn noise_vectors() {
    let v = json!({
        "doc": "Noise handshake + transport with fixed static and ephemeral keys (snow's fixed_ephemeral_key_for_testing_only). A JS implementation given the same private keys, ephemerals, prologue and psk must produce exactly these bytes. The psk goes at position 2 (IKpsk2). Handshake payloads are message1_payload / message2_payload.",
        "pairing": noise_vector("pairing", PARAMS_PAIRING, Some(arr::<32>(24))),
        "session": noise_vector("session", PARAMS_SESSION, None),
    });
    check("noise", &v);
}

#[test]
fn hand_written_invalid_vectors_really_fail_that_way() {
    use csm_remote_proto::ids::unhex;
    use csm_remote_proto::pairing::OfferError;
    use csm_remote_proto::relay::{FrameError, decode_frame};
    let frames: Value = serde_json::from_str(&std::fs::read_to_string(path("frames")).unwrap()).unwrap();
    for case in frames["invalid"].as_array().unwrap() {
        let raw = unhex(case["frame_hex"].as_str().unwrap()).unwrap();
        let name = match decode_frame(&raw).unwrap_err() {
            FrameError::TooShort => "too_short",
            FrameError::Version(_) => "version",
            FrameError::UnknownKind(_) => "unknown_kind",
        };
        assert_eq!(name, case["error"].as_str().unwrap());
    }
    let pairing: Value = serde_json::from_str(&std::fs::read_to_string(path("pairing")).unwrap()).unwrap();
    for case in pairing["invalid"].as_array().unwrap() {
        let name = match PairingOffer::decode(case["input"].as_str().unwrap()).unwrap_err() {
            OfferError::Format => "format".to_owned(),
            OfferError::Version(v) => format!("version {v}"),
            OfferError::Field(f) => format!("field {f}"),
        };
        assert_eq!(name, case["error"].as_str().unwrap());
    }
}

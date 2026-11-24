//! Generates and checks `vectors/*.json`, the shared test vectors a second implementation is
//! checked against. Run with `CSM_UPDATE_VECTORS=1` to rewrite them; otherwise the files on disk
//! must equal what the code produces now, so a protocol change cannot go unnoticed.
//!
//! The Noise vectors use fixed keys and fixed ephemerals (snow's testing hook), so every byte of a
//! handshake and a transport run is reproducible.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use csm_peer_proto::addr::{AddrError, PeerAddr};
use csm_peer_proto::codec::{CodecError, Decoder, Frame, Input};
use csm_peer_proto::delta::{Delta, DeltaOp, SmallFields};
use csm_peer_proto::frame::{FrameDecoder, FrameError, encode_frame};
use csm_peer_proto::ids::{b64_encode, device_id, hex, host_id, sha256, unhex};
use csm_peer_proto::limits::{MAX_FRAME_LEN, MAX_PLAINTEXT};
use csm_peer_proto::msg::{ByeReason, ConsentOutcome, ErrorKind, Level, Msg, Outcome, PROTOCOL_VERSION, TermClosedReason, UploadKind, UploadRefused};
use csm_peer_proto::noise::{MAX_PAIR_ATTEMPTS, PARAMS_PAIRING, PARAMS_SESSION, Preamble, REKEY_INTERVAL, Route, check_code, prologue, route};
use csm_peer_proto::pairing::{DenyReason, OfferError, PairError, PairMsg, PeerOffer};
use csm_peer_proto::{Event, GroupInfo, Snapshot};
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
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
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

fn x25519_public(private: &[u8; 32]) -> [u8; 32] {
    let mut dh = DefaultResolver.resolve_dh(&DHChoice::Curve25519).unwrap();
    dh.set(private);
    dh.pubkey().try_into().unwrap()
}

// ---- ids ---------------------------------------------------------------------------------

#[test]
fn ids_vectors() {
    let keys = [arr::<32>(1), arr::<32>(2), [0u8; 32], [255u8; 32]];
    let ids: Vec<Value> =
        keys.iter().map(|k| json!({"public_key_hex": hex(k), "public_key_b64": b64_encode(k), "host_id": host_id(k), "device_id": device_id(k)})).collect();
    let checks: Vec<Value> = [arr::<32>(4), arr::<32>(5), [0u8; 32]].iter().map(|h| json!({"handshake_hash_hex": hex(h), "check_code": check_code(h)})).collect();
    let hid = host_id(&arr::<32>(1));
    let v = json!({
        "doc": "host_id = base32_lower_nopad(SHA-256('csm-peer host' || host_static_public))[..26]; device_id = the same with 'csm-peer device'; check_code = 4 decimal digits of u32_be(SHA-256('csm-peer check' || handshake_hash)[0..4]) mod 10000; prologue = 'csm-peer/1' 0x00 host_id",
        "keys": ids,
        "check_codes": checks,
        "prologue": {"host_id": hid, "prologue_hex": hex(&prologue(&hid))},
        "sha256_of_abc_hex": hex(&sha256(b"abc")),
    });
    check("ids", &v);
}

// ---- frames ------------------------------------------------------------------------------

#[test]
fn frames_vectors() {
    let valid: Vec<Value> = [b"x".to_vec(), b"hello".to_vec(), bytes(3, 300)]
        .iter()
        .map(|p| json!({"payload_hex": hex(p), "frame_hex": hex(&encode_frame(p).unwrap())}))
        .collect();
    let invalid = vec![
        json!({"frame_hex": "00000000", "error": "empty"}),
        json!({"frame_hex": hex(&((MAX_FRAME_LEN as u32) + 1).to_be_bytes()), "error": "too_large"}),
        json!({"frame_hex": "ffffffff", "error": "too_large"}),
        json!({"frame_hex": "ffffffff00112233", "error": "too_large"}),
    ];
    let v = json!({
        "doc": "u32 BE length || payload; the payload is one Noise message, at most max_frame_len bytes, never empty; a bad length is refused on the four header bytes alone and the connection is closed",
        "max_frame_len": MAX_FRAME_LEN,
        "valid": valid,
        "invalid": invalid,
    });
    check("frames", &v);
}

#[test]
fn hand_written_invalid_frames_fail_that_way() {
    let frames: Value = serde_json::from_str(&std::fs::read_to_string(path("frames")).unwrap()).unwrap();
    for case in frames["invalid"].as_array().unwrap() {
        let mut d = FrameDecoder::new();
        d.push(&unhex(case["frame_hex"].as_str().unwrap()).unwrap());
        let name = match d.next_frame().unwrap_err() {
            FrameError::Empty => "empty",
            FrameError::TooLarge(_) => "too_large",
        };
        assert_eq!(name, case["error"].as_str().unwrap());
    }
    for case in frames["valid"].as_array().unwrap() {
        let mut d = FrameDecoder::new();
        d.push(&unhex(case["frame_hex"].as_str().unwrap()).unwrap());
        assert_eq!(hex(&d.next_frame().unwrap().unwrap()), case["payload_hex"].as_str().unwrap());
    }
}

// ---- pairing -----------------------------------------------------------------------------

#[test]
fn pairing_vectors() {
    let host_pub = arr::<32>(10);
    let mk = |addr: &str| PeerOffer::new(&PeerAddr::parse(addr).unwrap(), &host_pub, &arr::<16>(11), &arr::<32>(12));
    let valid: Vec<Value> = ["tcp://10.8.0.2:47300", "tcp://[fd00::2]:47300", "tcp://office.example.test:1"]
        .iter()
        .map(|a| {
            let o = mk(a);
            json!({"address": a, "offer": serde_json::to_value(&o).unwrap(), "payload": o.encode(), "link": o.link()})
        })
        .collect();
    let mut invalid = Vec::new();
    let bad = |f: &dyn Fn(&mut PeerOffer)| {
        let mut o = mk("tcp://10.8.0.2:47300");
        f(&mut o);
        o.encode()
    };
    for input in [
        String::new(),
        "p=!!!".to_owned(),
        bad(&|o| o.a = "tcp://0.0.0.0:47300".into()),
        bad(&|o| o.a = "tcp://[::]:47300".into()),
        bad(&|o| o.a = "wss://relay.example/".into()),
        bad(&|o| o.h = host_id(&[9; 32])),
        bad(&|o| o.v = 9),
        bad(&|o| o.p = b64_encode(&[0; 31])),
    ] {
        let name = match PeerOffer::decode(&input).unwrap_err() {
            OfferError::Format => "format".to_owned(),
            OfferError::Version(v) => format!("version {v}"),
            OfferError::Field(f) => format!("field {f}"),
        };
        invalid.push(json!({"input": input, "error": name}));
    }
    let addrs_ok: Vec<Value> = ["tcp://10.8.0.2:47300", "tcp://[fd00::2]:1", "tcp://Office.Test:5"].iter().map(|a| json!({"input": a, "url": PeerAddr::parse(a).unwrap().to_url()})).collect();
    let addrs_bad: Vec<Value> = ["tcp://0.0.0.0:1", "tcp://[::]:1", "tcp://224.0.0.1:1", "tcp://10.0.0.1:0", "tcp://10.0.0.1", "http://10.0.0.1:1", "tcp://1.2.3:1"]
        .iter()
        .map(|a| {
            let e = match PeerAddr::parse(a).unwrap_err() {
                AddrError::Format => "format",
                AddrError::Unspecified => "unspecified",
                AddrError::NotUnicast => "not_unicast",
                AddrError::Port => "port",
            };
            json!({"input": a, "error": e})
        })
        .collect();
    let pair_msgs = [
        PairMsg::Hello { v: PROTOCOL_VERSION, name: "Laptop".into() },
        PairMsg::Accept { device_id: device_id(&arr::<32>(13)), level: Level::Full },
        PairMsg::Deny { reason: DenyReason::Denied },
        PairMsg::Deny { reason: DenyReason::Expired },
    ];
    let pair_valid: Vec<Value> = pair_msgs.iter().map(|m| json!({"json": serde_json::to_value(m).unwrap(), "bytes_hex": hex(&m.encode().unwrap())})).collect();
    let pair_invalid: Vec<Value> = [
        (br#"{"t":"hello","v":1,"name":"x"}"#.as_slice(), PairError::Version(1)),
        (br#"{"t":"hello","v":2,"name":"a\nb"}"#.as_slice(), PairError::Name),
        (br#"{"t":"accept","device_id":"x","level":"full"}"#.as_slice(), PairError::DeviceId),
        (br#"{"t":"nope"}"#.as_slice(), PairError::Format),
    ]
    .iter()
    .map(|(b, e)| {
        assert_eq!(&PairMsg::decode(b).unwrap_err(), e);
        json!({"bytes_hex": hex(b), "error": format!("{e:?}")})
    })
    .collect();
    let v = json!({
        "doc": "pairing link = 'csm://pair#' + base64url(compact JSON offer {v,a,h,k,i,p}); the address is tcp://host:port and is never 0.0.0.0 or ::; h must equal host_id(k); decode also accepts the bare payload and '#payload'",
        "host_static_public_hex": hex(&host_pub),
        "valid": valid,
        "invalid": invalid,
        "addresses_valid": addrs_ok,
        "addresses_invalid": addrs_bad,
        "pair_messages": pair_valid,
        "pair_messages_invalid": pair_invalid,
    });
    check("pairing", &v);
}

#[test]
fn hand_written_invalid_offers_really_fail_that_way() {
    let pairing: Value = serde_json::from_str(&std::fs::read_to_string(path("pairing")).unwrap()).unwrap();
    for case in pairing["valid"].as_array().unwrap() {
        let o = PeerOffer::decode(case["link"].as_str().unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&o).unwrap(), case["offer"]);
    }
    for case in pairing["invalid"].as_array().unwrap() {
        let name = match PeerOffer::decode(case["input"].as_str().unwrap()).unwrap_err() {
            OfferError::Format => "format".to_owned(),
            OfferError::Version(v) => format!("version {v}"),
            OfferError::Field(f) => format!("field {f}"),
        };
        assert_eq!(name, case["error"].as_str().unwrap());
    }
}

// ---- messages ----------------------------------------------------------------------------

fn error_name(e: &CodecError) -> String {
    match e {
        CodecError::TooLarge => "too_large".into(),
        CodecError::Version(v) => format!("version {v}"),
        CodecError::UnknownKind(k) => format!("unknown_kind {k}"),
        CodecError::Malformed(w) => format!("malformed: {w}"),
        CodecError::Json => "json".into(),
        CodecError::Invalid(w) => format!("invalid: {w}"),
    }
}

fn sample_frames() -> Vec<(&'static str, Frame)> {
    let snapshot = Snapshot {
        version: 3,
        groups: vec![GroupInfo { id: "g1".into(), name: "Work".into(), expanded: true, torn_off: false, session_count: 2, needs_you: 1, unseen: 0 }],
        layout_key: "__ALL__".into(),
        inbox: vec!["s1".into()],
        focused: Some("s1".into()),
        inbox_open: true,
        ..Snapshot::default()
    };
    let small = SmallFields::of(&snapshot);
    let delta = Delta {
        from: 3,
        to: 4,
        ops: vec![DeltaOp::Remove { id: "s2".into() }, DeltaOp::Order { ids: vec!["s1".into(), "s3".into()] }, DeltaOp::Small { fields: Box::new(small) }],
    };
    let c = Frame::Control;
    vec![
        ("hello", c(Msg::Hello { protocol: PROTOCOL_VERSION, client: "Laptop".into(), app_version: "2.0.0".into(), term_version: "0.26.0".into() })),
        (
            "welcome",
            c(Msg::Welcome {
                protocol: PROTOCOL_VERSION,
                host_name: "OFFICE".into(),
                server_version: "2.0.0".into(),
                term_version: "0.26.0".into(),
                level: Level::Full,
                device_id: device_id(&arr::<32>(20)),
            }),
        ),
        ("snapshot", c(Msg::Snapshot { snapshot: Box::new(snapshot) })),
        ("delta", c(Msg::Delta { delta })),
        ("event_toast", c(Msg::Event { event: Some(Event::Toast { message: "Done".into() }) })),
        ("event_consent_resolved", c(Msg::Event { event: Some(Event::ConsentResolved { request_id: 7 }) })),
        ("call", c(Msg::Call { id: 1, tool: "session_send_text".into(), params: json!({"session_id": "s1", "text": "hi"}) })),
        ("reply_ok", c(Msg::Reply { id: 1, outcome: Outcome::Ok { message: "Sent.".into(), data: json!({"bytes": 2}) } })),
        ("reply_err", c(Msg::Reply { id: 2, outcome: Outcome::Err { kind: ErrorKind::Forbidden, message: "not at this level".into() } })),
        ("term_subscribe", c(Msg::TermSubscribe { stream: 1, session: "s1".into(), instance: None, from_offset: None })),
        ("term_subscribe_resume", c(Msg::TermSubscribe { stream: 1, session: "s1".into(), instance: Some(2), from_offset: Some(4096) })),
        ("term_unsubscribe", c(Msg::TermUnsubscribe { stream: 1 })),
        ("term_resync", c(Msg::TermResync { stream: 1 })),
        ("term_resize", c(Msg::TermResize { stream: 1, cols: 120, rows: 30, driver: true })),
        ("term_resized", c(Msg::TermResized { stream: 1, offset: 8192, cols: 100, rows: 28 })),
        ("term_closed", c(Msg::TermClosed { stream: 1, reason: TermClosedReason::NotRunning })),
        ("consent_card", c(Msg::ConsentCard { request: json!({"id": 7, "tool": "session_send_text"}) })),
        ("consent_answer", c(Msg::ConsentAnswer { request_id: 7, allow: true })),
        ("snapshot_request", c(Msg::SnapshotRequest)),
        ("consent_ack", c(Msg::ConsentAck { request_id: 7, outcome: ConsentOutcome::AlreadyResolved })),
        ("ping", c(Msg::Ping { n: 9 })),
        ("pong", c(Msg::Pong { n: 9 })),
        ("bye", c(Msg::Bye { reason: ByeReason::ServerOff })),
        ("term_data", Frame::TermData { stream: 1, instance: 2, offset: 4096, bytes: b"hello\r\n\x1b[32mok\x1b[0m".to_vec() }),
        ("term_attach", Frame::TermAttach { stream: 1, instance: 2, cols: 120, rows: 30, offset: 4096, more: false, vt: b"\x1b[2J\x1b[Hprompt> ".to_vec() }),
        ("term_attach_more", Frame::TermAttach { stream: 1, instance: 2, cols: 120, rows: 30, offset: 4096, more: true, vt: bytes(6, 40) }),
        ("term_input_key", Frame::TermInput { stream: 1, input: Input::Key(b"\x1b[A".to_vec()) }),
        ("term_input_mouse", Frame::TermInput { stream: 1, input: Input::Mouse(b"\x1b[<0;10;5M".to_vec()) }),
        ("term_input_focus", Frame::TermInput { stream: 1, input: Input::Focus(true) }),
        ("term_input_paste", Frame::TermInput { stream: 1, input: Input::Paste("two\nlines \u{e9}".into()) }),
        (
            "upload_begin",
            c(Msg::UploadBegin { id: 11, session: "s1".into(), name: "report.pdf".into(), size: 4096, kind: UploadKind::File }),
        ),
        (
            "upload_begin_image",
            c(Msg::UploadBegin { id: 12, session: "s1".into(), name: "clipboard.png".into(), size: 2_000_000, kind: UploadKind::Image }),
        ),
        // Over the limit is a valid frame: the office refuses it `too_large` and keeps the link.
        (
            "upload_begin_oversize",
            c(Msg::UploadBegin { id: 1, session: "s1".into(), name: "a.bin".into(), size: 67_108_865, kind: UploadKind::File }),
        ),
        ("upload_end", c(Msg::UploadEnd { id: 11, sha256: "ab".repeat(32) })),
        ("upload_cancel", c(Msg::UploadCancel { id: 11 })),
        ("upload_accepted", c(Msg::UploadAccepted { id: 11 })),
        ("upload_ack", c(Msg::UploadAck { id: 11, received: 131_072 })),
        ("upload_done", c(Msg::UploadDone { id: 11, path: "C:\\data\\peer\\uploads\\s1\\1795800000123-report.pdf".into() })),
        ("upload_refused", c(Msg::UploadRefused { id: 12, reason: UploadRefused::NotAnImage })),
        ("upload_chunk", Frame::UploadChunk { id: 11, offset: 262_144, bytes: b"%PDF-1.7\n".to_vec() }),
    ]
}

#[test]
fn messages_vectors() {
    let mut valid = Vec::new();
    for (name, frame) in sample_frames() {
        let parts = frame.encode().unwrap();
        assert_eq!(parts.len(), 1, "{name}");
        let mut d = Decoder::new();
        assert_eq!(d.decode(&parts[0]).unwrap().as_ref(), Some(&frame), "{name} decodes back");
        let json = match &frame {
            Frame::Control(m) => serde_json::to_value(m).unwrap(),
            _ => Value::Null,
        };
        valid.push(json!({"name": name, "json": json, "plaintext_hex": hex(&parts[0])}));
    }
    // A fragmented control message: its parts and the digest of the joined JSON.
    let big = Frame::Control(Msg::Reply { id: 5, outcome: Outcome::Ok { message: "big".into(), data: json!({"blob": "z".repeat(MAX_PLAINTEXT + 10)}) } });
    let parts = big.encode().unwrap();
    assert!(parts.len() >= 2);
    let mut d = Decoder::new();
    let mut last = None;
    for p in &parts {
        last = d.decode(p).unwrap();
    }
    assert_eq!(last, Some(big));
    let fragments = json!({
        "doc": "a control message whose JSON does not fit one Noise message is sent as consecutive kind-05 fragments: 05, last (0 or 1), bytes; the joined JSON is a control message",
        "count": parts.len(),
        "sizes": parts.iter().map(Vec::len).collect::<Vec<_>>(),
        "first_hex_prefix": hex(&parts[0][..12]),
        "last_flags": parts.iter().map(|p| p[1]).collect::<Vec<_>>(),
        "all_parts_sha256_hex": hex(&sha256(&parts.concat())),
    });

    let invalid_inputs: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("unknown_kind_00", vec![0x00]),
        ("unknown_kind_7f", vec![0x7f, 1, 2]),
        ("oversize", vec![0x01; MAX_PLAINTEXT + 1]),
        ("control_without_json", vec![0x01]),
        ("control_unknown_type", b"\x01{\"t\":\"nope\"}".to_vec()),
        ("hello_wrong_protocol", b"\x01{\"t\":\"hello\",\"protocol\":1,\"client\":\"c\",\"app_version\":\"1\",\"term_version\":\"0.26.0\"}".to_vec()),
        ("hello_control_char_in_name", b"\x01{\"t\":\"hello\",\"protocol\":1,\"client\":\"a\\nb\",\"app_version\":\"1\",\"term_version\":\"0.26.0\"}".to_vec()),
        ("call_bad_tool_id", b"\x01{\"t\":\"call\",\"id\":1,\"tool\":\"Not A Tool\",\"params\":{}}".to_vec()),
        ("resize_zero", b"\x01{\"t\":\"term_resize\",\"stream\":1,\"cols\":0,\"rows\":5,\"driver\":true}".to_vec()),
        ("term_data_truncated", vec![0x02, 0, 0, 0, 1]),
        ("term_data_empty", [vec![0x02], vec![0; 20]].concat()),
        ("term_attach_bad_flag", [vec![0x03], vec![0, 0, 0, 1], vec![0; 8], vec![0, 80, 0, 24], vec![0; 8], vec![2]].concat()),
        ("term_input_bad_kind", vec![0x04, 0, 0, 0, 1, 9, 1]),
        ("term_input_paste_not_utf8", vec![0x04, 0, 0, 0, 1, 3, 0xff]),
        ("term_input_bad_focus", vec![0x04, 0, 0, 0, 1, 2, 7]),
        ("fragment_bad_flag", vec![0x05, 2, 1]),
        ("fragment_empty", vec![0x05, 1]),
        ("upload_chunk_empty", [vec![0x06], vec![0; 16]].concat()),
        ("upload_chunk_truncated", vec![0x06, 0, 0, 0, 1]),
        ("upload_begin_bad_name", b"\x01{\"t\":\"upload_begin\",\"id\":1,\"session\":\"s1\",\"name\":\"a\\nb\",\"size\":1,\"kind\":\"file\"}".to_vec()),
        ("upload_end_bad_digest", b"\x01{\"t\":\"upload_end\",\"id\":1,\"sha256\":\"not-hex\"}".to_vec()),
    ];
    let invalid: Vec<Value> = invalid_inputs
        .iter()
        .map(|(name, input)| {
            let err = Decoder::new().decode(input).unwrap_err();
            json!({"name": name, "plaintext_hex": hex(input), "error": error_name(&err)})
        })
        .collect();
    let v = json!({
        "doc": "one Noise plaintext = one frame; first byte is the kind: 01 control JSON, 02 term_data, 03 term_attach, 04 term_input, 05 control fragment; integers are big endian. 'valid' entries are canonical encodings (json is the control message). 'invalid' plaintexts must be refused with that error and the connection closed.",
        "valid": valid,
        "fragments": fragments,
        "invalid": invalid,
        "max_plaintext": MAX_PLAINTEXT,
    });
    check("messages", &v);
}

#[test]
fn control_json_decodes_from_the_vector_text() {
    let m: Value = serde_json::from_str(&std::fs::read_to_string(path("messages")).unwrap()).unwrap();
    for case in m["valid"].as_array().unwrap() {
        let plain = unhex(case["plaintext_hex"].as_str().unwrap()).unwrap();
        let got = Decoder::new().decode(&plain).unwrap().unwrap();
        if let Frame::Control(msg) = got {
            assert_eq!(serde_json::to_value(&msg).unwrap(), case["json"], "{}", case["name"]);
        }
    }
    for case in m["invalid"].as_array().unwrap() {
        let plain = unhex(case["plaintext_hex"].as_str().unwrap()).unwrap();
        assert_eq!(error_name(&Decoder::new().decode(&plain).unwrap_err()), case["error"].as_str().unwrap(), "{}", case["name"]);
    }
}

// ---- Noise -------------------------------------------------------------------------------

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
    let rs_pub = x25519_public(&k.rs);
    let is_pub = x25519_public(&k.is);
    let hid = host_id(&rs_pub);
    let pro = prologue(&hid);
    let mut init = builder(params, &k, psk.as_ref(), &pro, true, &rs_pub, &k.ei).build_initiator().unwrap();
    let mut resp = builder(params, &k, psk.as_ref(), &pro, false, &rs_pub, &k.er).build_responder().unwrap();

    let (p1, p2) = (b"client".to_vec(), b"host".to_vec());
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

    // The script is real frames: a Hello, a Welcome, terminal bytes and a ping.
    let frames = sample_frames();
    let pick = |n: &str| frames.iter().find(|(name, _)| *name == n).map(|(_, f)| f.encode().unwrap().remove(0)).unwrap();
    let script: [(bool, Vec<u8>); 5] =
        [(true, pick("hello")), (false, pick("welcome")), (false, pick("term_data")), (true, pick("term_input_key")), (true, bytes(5, 300))];
    let mut transport = Vec::new();
    for (from_init, plain) in &script {
        let (tx, rx) = if *from_init { (&mut ti, &mut tr) } else { (&mut tr, &mut ti) };
        let n = tx.write_message(plain, &mut buf).unwrap();
        let ct = buf[..n].to_vec();
        let m = rx.read_message(&ct, &mut out).unwrap();
        assert_eq!(&out[..m], &plain[..]);
        transport.push(json!({"from": if *from_init {"initiator"} else {"responder"}, "plaintext_hex": hex(plain), "ciphertext_hex": hex(&ct), "frame_hex": hex(&encode_frame(&ct).unwrap())}));
    }

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
        "host_id": hid,
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
        "message1_frame_hex": hex(&encode_frame(&msg1).unwrap()),
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
        "doc": "Noise handshake + transport of the peer profile, with fixed static and ephemeral keys (snow's fixed_ephemeral_key_for_testing_only). The prologue is 'csm-peer/1' 0x00 host_id, where host_id derives from the responder's static public key. The psk goes at position 2 (IKpsk2). Transport plaintexts are real frames (see messages.json); every Noise message travels in one length-prefixed frame. Before message 1 the client sends a cleartext preamble frame that selects the pattern (01 = IK, 02 || pairing_id = IKpsk2); see preamble.json.",
        "pairing": noise_vector("pairing", PARAMS_PAIRING, Some(arr::<32>(24))),
        "session": noise_vector("session", PARAMS_SESSION, None),
    });
    check("noise", &v);
}

#[test]
fn the_phone_profile_cannot_complete_a_peer_handshake() {
    // The prologues differ, so a phone-profile initiator fails against a peer-profile responder.
    let k = keys();
    let rs_pub = x25519_public(&k.rs);
    let peer = prologue(&host_id(&rs_pub));
    let phone = b"csm-remote/1\0abc".to_vec();
    let mut init = builder(PARAMS_SESSION, &k, None, &phone, true, &rs_pub, &k.ei).build_initiator().unwrap();
    let mut resp = builder(PARAMS_SESSION, &k, None, &peer, false, &rs_pub, &k.er).build_responder().unwrap();
    let mut buf = vec![0u8; 65535];
    let mut out = vec![0u8; 65535];
    let n = init.write_message(&[], &mut buf).unwrap();
    assert!(resp.read_message(&buf[..n], &mut out).is_err(), "a handshake of another profile is refused");
}

#[test]
fn a_session_message_one_does_not_open_under_the_pairing_pattern_and_back() {
    let k = keys();
    let rs_pub = x25519_public(&k.rs);
    let pro = prologue(&host_id(&rs_pub));
    let psk = arr::<32>(24);
    let mut buf = vec![0u8; 65535];
    let mut out = vec![0u8; 65535];

    let mut ik = builder(PARAMS_SESSION, &k, None, &pro, true, &rs_pub, &k.ei).build_initiator().unwrap();
    let n = ik.write_message(&[], &mut buf).unwrap();
    let mut psk_resp = builder(PARAMS_PAIRING, &k, Some(&psk), &pro, false, &rs_pub, &k.er).build_responder().unwrap();
    assert!(psk_resp.read_message(&buf[..n], &mut out).is_err(), "a session client cannot reach the pairing path");

    let mut ikpsk = builder(PARAMS_PAIRING, &k, Some(&psk), &pro, true, &rs_pub, &k.ei).build_initiator().unwrap();
    let n = ikpsk.write_message(&[], &mut buf).unwrap();
    let mut ik_resp = builder(PARAMS_SESSION, &k, None, &pro, false, &rs_pub, &k.er).build_responder().unwrap();
    assert!(ik_resp.read_message(&buf[..n], &mut out).is_err(), "a pairing client cannot reach the session path");
}

#[test]
fn preamble_vectors() {
    let id = arr::<16>(30);
    let slot = id;
    let cases: Vec<Value> = [
        ("session", Preamble::Session.encode(), Some(slot)),
        ("pairing_open_slot", Preamble::Pairing(id).encode(), Some(slot)),
        ("pairing_wrong_id", Preamble::Pairing(arr::<16>(31)).encode(), Some(slot)),
        ("pairing_no_slot", Preamble::Pairing(id).encode(), None),
        ("session_no_slot", Preamble::Session.encode(), None),
        ("empty", vec![], Some(slot)),
        ("unknown_tag", vec![3], Some(slot)),
        ("session_with_trailing_byte", vec![1, 0], Some(slot)),
        ("pairing_short_id", [vec![2], id[..15].to_vec()].concat(), Some(slot)),
        ("pairing_long_id", [vec![2], id.to_vec(), vec![0]].concat(), Some(slot)),
    ]
    .into_iter()
    .map(|(name, pre, open)| {
        let r = match route(&pre, open.as_ref()) {
            Route::Session => "session",
            Route::Pairing => "pairing",
            Route::Refuse => "refuse",
        };
        json!({"name": name, "preamble_hex": hex(&pre), "frame_hex": hex(&encode_frame(&pre).unwrap_or_default()), "open_slot_hex": open.map(|s| hex(&s)), "route": r})
    })
    .collect();
    let v = json!({
        "doc": "Every connection starts with one cleartext frame before Noise message 1: 01 = session (IK), 02 || pairing_id (16 bytes) = pairing (IKpsk2). The host routes to pairing only when a slot is open and the id matches (constant-time compare); everything else is refused by closing without writing. A slot is consumed when the first transport message decrypts, and allows 3 handshakes.",
        "max_pair_attempts": MAX_PAIR_ATTEMPTS,
        "cases": cases,
    });
    check("preamble", &v);
}

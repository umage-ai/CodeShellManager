//! Fuzz-style property tests: the decoders take hostile bytes and must answer with a value or an
//! error, never a panic, and never allocate past the limits. A fixed-seed generator keeps every
//! run the same (no new dependency); raise `ROUNDS` to dig deeper.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::codec::{AttachAssembler, Decoder, Frame, Input};
use csm_peer_proto::delta::{Delta, DeltaOp};
use csm_peer_proto::frame::{FrameDecoder, encode_frame};
use csm_peer_proto::limits::{MAX_FRAME_LEN, MAX_PLAINTEXT};
use csm_peer_proto::msg::{ByeReason, ConsentOutcome, ErrorKind, Level, Msg, Outcome, PROTOCOL_VERSION, TermClosedReason};
use csm_peer_proto::pairing::{PairMsg, PeerOffer};
use csm_peer_proto::{Event, Snapshot};
use serde_json::json;

const ROUNDS: usize = 20_000;

/// xorshift64*: small, fixed, good enough to spray bytes.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % (n as u64).max(1)) as usize
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
    fn some_bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max);
        self.bytes(n)
    }
}

/// Valid plaintexts of every frame kind, to mutate.
fn corpus() -> Vec<Vec<u8>> {
    let c = Frame::Control;
    let frames = vec![
        c(Msg::Hello { protocol: PROTOCOL_VERSION, client: "Laptop".into(), app_version: "2.0.0".into(), term_version: "0.26.0".into() }),
        c(Msg::Welcome {
            protocol: PROTOCOL_VERSION,
            host_name: "OFFICE".into(),
            server_version: "2.0.0".into(),
            term_version: "0.26.0".into(),
            level: Level::Full,
            device_id: "abcdefghijklmnopqrstuvwxyz".chars().take(26).collect::<String>().to_lowercase(),
        }),
        c(Msg::Snapshot { snapshot: Box::new(Snapshot { version: 2, layout_key: "__ALL__".into(), ..Snapshot::default() }) }),
        c(Msg::Delta { delta: Delta { from: 1, to: 2, ops: vec![DeltaOp::Remove { id: "s1".into() }, DeltaOp::Order { ids: vec!["a".into()] }] } }),
        c(Msg::Event { event: Some(Event::Toast { message: "x".into() }) }),
        c(Msg::Call { id: 9, tool: "session_list".into(), params: json!({"a": [1, 2, {"b": null}]}) }),
        c(Msg::Reply { id: 9, outcome: Outcome::Err { kind: ErrorKind::NotFound, message: "no".into() } }),
        c(Msg::TermSubscribe { stream: 1, session: "s".into(), instance: Some(1), from_offset: Some(5) }),
        c(Msg::TermResize { stream: 1, cols: 80, rows: 24, driver: false }),
        c(Msg::TermResized { stream: 1, offset: 1, cols: 80, rows: 24 }),
        c(Msg::TermClosed { stream: 1, reason: TermClosedReason::SessionGone }),
        c(Msg::ConsentAnswer { request_id: 1, allow: false }),
        c(Msg::ConsentAck { request_id: 1, outcome: ConsentOutcome::Applied }),
        c(Msg::Bye { reason: ByeReason::Timeout }),
        Frame::TermData { stream: 1, instance: 1, offset: 1, bytes: b"abc".to_vec() },
        Frame::TermAttach { stream: 1, instance: 1, cols: 80, rows: 24, offset: 1, more: false, vt: b"x".to_vec() },
        Frame::TermInput { stream: 1, input: Input::Paste("hi".into()) },
        Frame::TermInput { stream: 1, input: Input::Focus(true) },
    ];
    frames.iter().flat_map(|f| f.encode().unwrap()).collect()
}

fn mutate(rng: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut v = src.to_vec();
    for _ in 0..=rng.below(4) {
        match rng.below(6) {
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] ^= 1 << rng.below(8);
            }
            1 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] = rng.byte();
            }
            2 => v.truncate(rng.below(v.len() + 1)),
            3 => {
                let n = rng.below(20);
                v.extend(rng.bytes(n));
            }
            4 if !v.is_empty() => {
                let i = rng.below(v.len());
                v.remove(i);
            }
            _ => {
                let i = rng.below(v.len() + 1);
                v.insert(i, rng.byte());
            }
        }
    }
    v
}

#[test]
fn the_frame_decoder_survives_random_streams() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..ROUNDS / 4 {
        let n = rng.below(400);
        let stream = rng.bytes(n);
        let mut d = FrameDecoder::new();
        let mut at = 0;
        while at < stream.len() {
            let step = 1 + rng.below(70);
            let end = (at + step).min(stream.len());
            d.push(&stream[at..end]);
            at = end;
            loop {
                match d.next_frame() {
                    Ok(Some(f)) => assert!(!f.is_empty() && f.len() <= MAX_FRAME_LEN),
                    Ok(None) => break,
                    Err(_) => {
                        assert_eq!(d.buffered(), 0, "an error drops what was buffered");
                        at = stream.len();
                        break;
                    }
                }
            }
        }
    }
}

#[test]
fn valid_frames_split_anywhere_come_out_whole() {
    let mut rng = Rng(7);
    for _ in 0..500 {
        let payloads: Vec<Vec<u8>> = (0..1 + rng.below(5)).map(|_| 1 + rng.below(2000)).collect::<Vec<_>>().into_iter().map(|n| rng.bytes(n)).collect();
        let wire: Vec<u8> = payloads.iter().flat_map(|p| encode_frame(p).unwrap()).collect();
        let mut d = FrameDecoder::new();
        let mut got = Vec::new();
        let mut at = 0;
        while at < wire.len() {
            let end = (at + 1 + rng.below(900)).min(wire.len());
            d.push(&wire[at..end]);
            at = end;
            while let Some(f) = d.next_frame().unwrap() {
                got.push(f);
            }
        }
        assert_eq!(got, payloads);
    }
}

#[test]
fn the_message_decoder_survives_random_plaintexts() {
    let mut rng = Rng(0xdead_beef_1234_5678);
    let mut d = Decoder::new();
    for _ in 0..ROUNDS {
        let n = rng.below(200);
        let mut p = rng.bytes(n);
        if !p.is_empty() && rng.below(4) != 0 {
            p[0] = 1 + (rng.byte() % 5); // mostly real kinds
        }
        let _ = d.decode(&p);
    }
}

#[test]
fn mutated_valid_messages_never_panic_and_stay_within_limits() {
    let corpus = corpus();
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let mut accepted = 0usize;
    for _ in 0..ROUNDS {
        let src = &corpus[rng.below(corpus.len())];
        let m = mutate(&mut rng, src);
        let mut d = Decoder::new();
        if let Ok(Some(frame)) = d.decode(&m) {
            accepted += 1;
            // Whatever it accepted, it can encode again within limits (strictness is symmetric).
            for part in frame.encode().unwrap() {
                assert!(part.len() <= MAX_PLAINTEXT);
            }
        }
    }
    assert!(accepted > 0, "the mutator should sometimes leave a message valid");
    // The unmutated corpus decodes (a guard against a corpus that tests nothing).
    for p in &corpus {
        assert!(Decoder::new().decode(p).unwrap().is_some());
    }
}

#[test]
fn random_frames_round_trip() {
    let mut rng = Rng(42);
    for _ in 0..5000 {
        let n = 1 + rng.below(3000);
        let bytes = rng.bytes(n);
        let f = match rng.below(5) {
            0 => Frame::TermData { stream: rng.next() as u32, instance: rng.next(), offset: rng.next(), bytes },
            1 => Frame::TermAttach {
                stream: rng.next() as u32,
                instance: rng.next(),
                cols: 1 + rng.below(2000) as u16,
                rows: 1 + rng.below(2000) as u16,
                offset: rng.next(),
                more: rng.below(2) == 0,
                vt: bytes,
            },
            2 => Frame::TermInput { stream: rng.next() as u32, input: Input::Key(bytes) },
            3 => Frame::TermInput { stream: rng.next() as u32, input: Input::Mouse(bytes) },
            _ => Frame::Control(Msg::TermResized { stream: rng.next() as u32, offset: rng.next(), cols: 1 + rng.below(2000) as u16, rows: 1 + rng.below(2000) as u16 }),
        };
        let parts = f.encode().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(Decoder::new().decode(&parts[0]).unwrap(), Some(f));
    }
}

#[test]
fn hostile_attach_sequences_do_not_panic() {
    let mut rng = Rng(99);
    let mut asm = AttachAssembler::new();
    for _ in 0..5000 {
        let f = Frame::TermAttach {
            stream: rng.below(100) as u32,
            instance: rng.below(3) as u64,
            cols: 1 + rng.below(3) as u16,
            rows: 1 + rng.below(3) as u16,
            offset: rng.below(3) as u64,
            more: rng.below(2) == 0,
            vt: rng.some_bytes(50),
        };
        let _ = asm.push(&f);
        if rng.below(10) == 0 {
            asm.cancel(rng.below(100) as u32);
        }
    }
}

#[test]
fn pairing_decoders_survive_random_text_and_bytes() {
    let mut rng = Rng(0xabad_1dea);
    let addr = PeerAddr::parse("tcp://10.8.0.2:47300").unwrap();
    let link = PeerOffer::new(&addr, &[2; 32], &[3; 16], &[4; 32]).link();
    let pair = PairMsg::Hello { v: PROTOCOL_VERSION, name: "Laptop".into() }.encode().unwrap();
    for _ in 0..ROUNDS {
        let raw = rng.some_bytes(120);
        let _ = PairMsg::decode(&raw);
        let text = String::from_utf8_lossy(&raw).into_owned();
        let _ = PeerOffer::decode(&text);
        let _ = PeerAddr::parse(&text);
        let _ = PairMsg::decode(&mutate(&mut rng, &pair));
        let mutated = String::from_utf8_lossy(&mutate(&mut rng, link.as_bytes())).into_owned();
        if let Ok(o) = PeerOffer::decode(&mutated) {
            // Anything it accepts is a valid offer: never an unspecified address.
            assert!(o.validate().is_ok());
            assert!(o.addr().is_some());
        }
        let addr_text = format!("tcp://{}", String::from_utf8_lossy(&mutate(&mut rng, b"10.8.0.2:47300")));
        if let Ok(a) = PeerAddr::parse(&addr_text) {
            if let Some(s) = a.socket_addr() {
                assert!(!s.ip().is_unspecified() && s.port() != 0);
            }
            assert_eq!(PeerAddr::parse(&a.to_url()).unwrap(), a);
        }
    }
}

#[test]
fn deeply_nested_json_is_refused_not_a_stack_overflow() {
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let plain = format!("\u{1}{{\"t\":\"call\",\"id\":1,\"tool\":\"x\",\"params\":{deep}}}");
    assert!(Decoder::new().decode(plain.as_bytes()).is_err());
    let mut small = vec![1u8];
    small.extend_from_slice(format!("{{\"t\":\"call\",\"id\":1,\"tool\":\"x\",\"params\":{}}}", "[".repeat(5000)).as_bytes());
    assert!(Decoder::new().decode(&small).is_err());
}

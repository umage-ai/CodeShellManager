//! Integration tests: the real relay on an ephemeral port, driven by in-process WebSocket clients
//! playing the add-on (host) and phones.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use csm_relay::config::Config;
use csm_relay::push::{BoxFuture, PushJob, PushOutcome, PushSender};
use csm_relay::{Kind, Relay, serve};
use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{b64_encode, host_id, token_hash};
use csm_remote_proto::relay::{FrameKind, HostToRelay, OpenInfo, PushRequest, RelayToClient, RelayToHost, decode_frame, encode_frame, parse_close_payload};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WAIT: Duration = Duration::from_secs(5);

// ---- rig ---------------------------------------------------------------------------------

struct Rig {
    addr: SocketAddr,
    relay: Arc<Relay>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

#[derive(Default)]
struct FakePush {
    jobs: Mutex<Vec<PushJob>>,
}

impl PushSender for FakePush {
    fn send(&self, job: PushJob) -> BoxFuture<'_, PushOutcome> {
        self.jobs.lock().unwrap().push(job);
        Box::pin(async { PushOutcome { status: Some(201), ok: true } })
    }
    fn public_key(&self) -> Option<String> {
        Some("BPUBLICKEY".to_owned())
    }
}

fn open_config() -> Config {
    let mut c = Config::default();
    c.access.open = true;
    // Plenty of headroom unless a test is about a limit.
    c.limits.new_connections_per_ip_per_minute = 1000;
    c.limits.max_sockets_per_ip = 1000;
    c
}

async fn start(config: Config, push: Option<Arc<dyn PushSender>>) -> Rig {
    let relay = Relay::new(config.into_settings().unwrap(), push);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, rx) = tokio::sync::oneshot::channel::<()>();
    let r = relay.clone();
    tokio::spawn(async move {
        let _ = serve(listener, r, async {
            let _ = rx.await;
        })
        .await;
    });
    Rig { addr, relay, _stop: stop }
}

async fn start_open() -> Rig {
    start(open_config(), None).await
}

// ---- clients -----------------------------------------------------------------------------

#[derive(Debug)]
enum Got {
    Text(String),
    Bin(Vec<u8>),
    Close(Option<u16>),
    Gone,
}

async fn next(ws: &mut Ws) -> Got {
    loop {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("timed out waiting for the relay") {
            None | Some(Err(_)) => return Got::Gone,
            Some(Ok(Message::Text(t))) => return Got::Text(t.as_str().to_owned()),
            Some(Ok(Message::Binary(b))) => return Got::Bin(b.to_vec()),
            Some(Ok(Message::Close(f))) => return Got::Close(f.map(|f| u16::from(f.code))),
            Some(Ok(_)) => {}
        }
    }
}

fn refusal(close_code: Option<u16>) -> Option<RefusalCode> {
    close_code.and_then(|c| c.checked_sub(4000)).and_then(RefusalCode::from_u16)
}

async fn ws_connect(rig: &Rig, path: &str, headers: &[(&str, &str)]) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://{}{path}", rig.addr).into_client_request().unwrap();
    for (k, v) in headers {
        req.headers_mut().insert(tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
    }
    Ok(tokio_tungstenite::connect_async(req).await?.0)
}

async fn send_text(ws: &mut Ws, text: &str) {
    ws.send(Message::Text(text.to_owned().into())).await.unwrap();
}

async fn send_bin(ws: &mut Ws, bytes: Vec<u8>) {
    ws.send(Message::Binary(bytes.into())).await.unwrap();
}

struct Host {
    ws: Ws,
    id: String,
}

fn secret(seed: u8) -> [u8; 32] {
    [seed; 32]
}

impl Host {
    async fn try_connect(rig: &Rig, seed: u8, access: Option<&str>, headers: &[(&str, &str)]) -> Result<Host, RefusalCode> {
        Host::try_connect_at(rig, "/v1/host", seed, access, headers).await
    }

    async fn try_connect_at(rig: &Rig, path: &str, seed: u8, access: Option<&str>, headers: &[(&str, &str)]) -> Result<Host, RefusalCode> {
        let secret = secret(seed);
        let id = host_id(&secret);
        let mut ws = ws_connect(rig, path, headers).await.unwrap();
        let hello = HostToRelay::Hello { v: 1, host_id: id.clone(), host_secret: b64_encode(&secret), access: access.map(str::to_owned) };
        send_text(&mut ws, &csm_remote_proto::relay::to_text(&hello)).await;
        match next(&mut ws).await {
            Got::Text(t) => match serde_json::from_str::<RelayToHost>(&t).unwrap() {
                RelayToHost::Welcome { v, limits } => {
                    assert_eq!(v, 1);
                    assert!(limits.max_frame_bytes > 0);
                    Ok(Host { ws, id })
                }
                RelayToHost::Error { code } => {
                    // A refusal is followed by a close frame with the same code.
                    let close = next(&mut ws).await;
                    assert!(matches!(close, Got::Close(c) if refusal(c) == Some(code)), "{close:?}");
                    Err(code)
                }
                other => panic!("unexpected {other:?}"),
            },
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn connect(rig: &Rig, seed: u8) -> Host {
        Host::try_connect(rig, seed, None, &[]).await.unwrap()
    }

    async fn control(&mut self, msg: &HostToRelay) -> RelayToHost {
        send_text(&mut self.ws, &csm_remote_proto::relay::to_text(msg)).await;
        self.reply().await
    }

    async fn reply(&mut self) -> RelayToHost {
        match next(&mut self.ws).await {
            Got::Text(t) => serde_json::from_str(&t).unwrap(),
            other => panic!("expected a control reply, got {other:?}"),
        }
    }

    async fn allow(&mut self, tokens: &[[u8; 32]]) {
        let hashes = tokens.iter().map(|t| b64_encode(&token_hash(t))).collect::<Vec<_>>();
        let r = self.control(&HostToRelay::Allow { tokens: hashes }).await;
        assert_eq!(r, RelayToHost::AllowOk { count: u32::try_from(tokens.len()).unwrap() });
    }

    async fn open_slot(&mut self, pairing_id: [u8; 16], secs: u32) {
        let r = self.control(&HostToRelay::PairingOpen { pairing_id: b64_encode(&pairing_id), expires_in: secs }).await;
        assert_eq!(r, RelayToHost::PairingOk { expires_in: secs.min(600) });
    }

    async fn frame(&mut self) -> (FrameKind, u32, Vec<u8>) {
        match next(&mut self.ws).await {
            Got::Bin(b) => {
                let f = decode_frame(&b).unwrap();
                (f.kind, f.channel, f.payload.to_vec())
            }
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    async fn send_data(&mut self, channel: u32, payload: &[u8]) {
        send_bin(&mut self.ws, encode_frame(FrameKind::Data, channel, payload)).await;
    }
}

struct Phone {
    ws: Ws,
}

fn hello_token(host: &str, token: &[u8; 32]) -> String {
    format!(r#"{{"t":"hello","v":1,"host_id":"{host}","token":"{}"}}"#, b64_encode(token))
}

fn hello_pairing(host: &str, id: &[u8; 16]) -> String {
    format!(r#"{{"t":"hello","v":1,"host_id":"{host}","pairing_id":"{}"}}"#, b64_encode(id))
}

impl Phone {
    async fn connect_raw(rig: &Rig, hello: &str, headers: &[(&str, &str)]) -> Result<Phone, RefusalCode> {
        Phone::connect_at(rig, "/v1/client", hello, headers).await
    }

    async fn connect_at(rig: &Rig, path: &str, hello: &str, headers: &[(&str, &str)]) -> Result<Phone, RefusalCode> {
        let mut ws = ws_connect(rig, path, headers).await.unwrap();
        send_text(&mut ws, hello).await;
        match next(&mut ws).await {
            Got::Text(t) => match serde_json::from_str::<RelayToClient>(&t).unwrap() {
                RelayToClient::Ready { v } => {
                    assert_eq!(v, 1);
                    Ok(Phone { ws })
                }
                RelayToClient::Error { code } => {
                    let close = next(&mut ws).await;
                    assert!(matches!(close, Got::Close(c) if refusal(c) == Some(code)), "{close:?}");
                    Err(code)
                }
            },
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn token(rig: &Rig, host: &str, token: &[u8; 32]) -> Result<Phone, RefusalCode> {
        Phone::connect_raw(rig, &hello_token(host, token), &[]).await
    }

    async fn pairing(rig: &Rig, host: &str, id: &[u8; 16]) -> Result<Phone, RefusalCode> {
        Phone::connect_raw(rig, &hello_pairing(host, id), &[]).await
    }

    async fn send(&mut self, bytes: &[u8]) {
        send_bin(&mut self.ws, bytes.to_vec()).await;
    }

    async fn recv(&mut self) -> Got {
        next(&mut self.ws).await
    }
}

fn tok(seed: u8) -> [u8; 32] {
    [seed; 32]
}

// ---- pairing and routing ---------------------------------------------------------------------

#[tokio::test]
async fn pairing_then_routing_is_byte_identical_both_ways() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[]).await;
    let pid = [9u8; 16];
    host.open_slot(pid, 600).await;

    let mut phone = Phone::pairing(&rig, &host.id, &pid).await.unwrap();
    let (kind, ch, payload) = host.frame().await;
    assert_eq!(kind, FrameKind::Open);
    assert_eq!(OpenInfo::decode(&payload), Some(OpenInfo::Pairing(pid)));

    // Opaque bytes (not valid UTF-8, not valid anything) cross unchanged in both directions.
    let up: Vec<u8> = (0..=255u8).chain([0xff, 0xfe, 0x00]).collect();
    phone.send(&up).await;
    assert_eq!(host.frame().await, (FrameKind::Data, ch, up.clone()));
    let down: Vec<u8> = up.iter().rev().copied().collect();
    host.send_data(ch, &down).await;
    assert!(matches!(phone.recv().await, Got::Bin(b) if b == down));

    // The PC confirms: it issues a routing token, then closes the slot. The pairing channel ends.
    let token = tok(5);
    host.allow(&[token]).await;
    send_text(&mut host.ws, &csm_remote_proto::relay::to_text(&HostToRelay::PairingClose)).await;
    assert!(matches!(phone.recv().await, Got::Close(Some(1000))));
    assert!(!rig.relay.has_slot(Kind::Phone, &host.id));

    // The paired phone now connects with its routing token.
    let mut again = Phone::token(&rig, &host.id, &token).await.unwrap();
    let (kind, ch2, payload) = host.frame().await;
    assert_eq!(kind, FrameKind::Open);
    assert_eq!(OpenInfo::decode(&payload), Some(OpenInfo::Device(token_hash(&token))));
    assert_ne!(ch, ch2);
    again.send(b"hi").await;
    assert_eq!(host.frame().await, (FrameKind::Data, ch2, b"hi".to_vec()));
}

#[tokio::test]
async fn channels_are_isolated_from_each_other() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1), tok(2)]).await;
    let mut a = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    let (_, ca, _) = host.frame().await;
    let mut b = Phone::token(&rig, &host.id, &tok(2)).await.unwrap();
    let (_, cb, _) = host.frame().await;

    host.send_data(cb, b"for b").await;
    host.send_data(ca, b"for a").await;
    assert!(matches!(a.recv().await, Got::Bin(x) if x == b"for a"));
    assert!(matches!(b.recv().await, Got::Bin(x) if x == b"for b"));

    a.send(b"from a").await;
    b.send(b"from b").await;
    let mut seen = vec![host.frame().await, host.frame().await];
    seen.sort_by_key(|f| f.1);
    let mut want = vec![(FrameKind::Data, ca, b"from a".to_vec()), (FrameKind::Data, cb, b"from b".to_vec())];
    want.sort_by_key(|f| f.1);
    assert_eq!(seen, want);
}

#[tokio::test]
async fn a_phone_leaving_is_reported_to_the_host_and_a_host_close_reaches_the_phone() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let phone = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    let (_, ch, _) = host.frame().await;
    drop(phone);
    let (kind, ch_closed, payload) = host.frame().await;
    assert_eq!((kind, ch_closed, parse_close_payload(&payload)), (FrameKind::Close, ch, None));

    let mut phone = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    let (_, ch, _) = host.frame().await;
    send_bin(&mut host.ws, encode_frame(FrameKind::Close, ch, &csm_remote_proto::relay::close_payload(Some(RefusalCode::Revoked)))).await;
    assert!(matches!(phone.recv().await, Got::Close(c) if refusal(c) == Some(RefusalCode::Revoked)));
}

#[tokio::test]
async fn host_disconnect_closes_phones_with_host_offline_and_new_phones_are_refused() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let mut phone = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    let id = host.id.clone();
    drop(host);
    assert!(matches!(phone.recv().await, Got::Close(c) if refusal(c) == Some(RefusalCode::HostOffline)));
    assert_eq!(Phone::token(&rig, &id, &tok(1)).await.err(), Some(RefusalCode::HostOffline));
}

#[tokio::test]
async fn a_newer_host_connection_replaces_the_older_and_state_must_be_resent() {
    let rig = start_open().await;
    let mut old = Host::connect(&rig, 1).await;
    old.allow(&[tok(1)]).await;
    let mut phone = Phone::token(&rig, &old.id, &tok(1)).await.unwrap();
    let _ = old.frame().await;

    let mut new = Host::connect(&rig, 1).await;
    assert!(matches!(old.ws.next().await, Some(Ok(Message::Close(Some(f)))) if refusal(Some(u16::from(f.code))) == Some(RefusalCode::HostReplaced)));
    assert!(matches!(phone.recv().await, Got::Close(c) if refusal(c) == Some(RefusalCode::HostOffline)));
    // The allow list lived on the old connection: the new host has to send it again.
    assert_eq!(Phone::token(&rig, &new.id, &tok(1)).await.err(), Some(RefusalCode::NotAllowed));
    new.allow(&[tok(1)]).await;
    assert!(Phone::token(&rig, &new.id, &tok(1)).await.is_ok());
}

#[tokio::test]
async fn revoking_a_device_drops_its_channel_at_once() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1), tok(2)]).await;
    let mut a = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    let _ = host.frame().await;
    let mut b = Phone::token(&rig, &host.id, &tok(2)).await.unwrap();
    let _ = host.frame().await;

    host.allow(&[tok(2)]).await;
    assert!(matches!(a.recv().await, Got::Close(c) if refusal(c) == Some(RefusalCode::Revoked)));
    assert_eq!(Phone::token(&rig, &host.id, &tok(1)).await.err(), Some(RefusalCode::NotAllowed));
    // The other device is untouched.
    b.send(b"still here").await;
    assert!(matches!(host.frame().await, (FrameKind::Data, _, p) if p == b"still here"));
}

// ---- pairing slots ---------------------------------------------------------------------------

#[tokio::test]
async fn pairing_slot_refusals() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    let pid = [3u8; 16];

    // No slot yet.
    assert_eq!(Phone::pairing(&rig, &host.id, &pid).await.err(), Some(RefusalCode::NotAllowed));
    host.open_slot(pid, 600).await;
    // Wrong id.
    assert_eq!(Phone::pairing(&rig, &host.id, &[4u8; 16]).await.err(), Some(RefusalCode::NotAllowed));
    // A second phone while one is pairing.
    let first = Phone::pairing(&rig, &host.id, &pid).await.unwrap();
    assert_eq!(Phone::pairing(&rig, &host.id, &pid).await.err(), Some(RefusalCode::NotAllowed));
    drop(first);
    // Closed by the host: single use.
    send_text(&mut host.ws, &csm_remote_proto::relay::to_text(&HostToRelay::PairingClose)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(Phone::pairing(&rig, &host.id, &pid).await.err(), Some(RefusalCode::NotAllowed));
}

#[tokio::test]
async fn pairing_slot_allows_five_attempts_then_dies() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    let pid = [3u8; 16];
    host.open_slot(pid, 600).await;
    for _ in 0..5 {
        let p = Phone::pairing(&rig, &host.id, &pid).await.unwrap();
        drop(p);
        // Wait until the relay has seen the channel go before trying again.
        loop {
            if rig.relay.counts().1 == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    assert_eq!(Phone::pairing(&rig, &host.id, &pid).await.err(), Some(RefusalCode::NotAllowed));
}

#[tokio::test]
async fn pairing_slot_expires() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    let pid = [3u8; 16];
    host.open_slot(pid, 1).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(Phone::pairing(&rig, &host.id, &pid).await.err(), Some(RefusalCode::NotAllowed));
}

#[tokio::test]
async fn pairing_slot_lifetime_is_capped_at_ten_minutes() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    // open_slot asserts the relay answered with min(secs, 600).
    host.open_slot([1u8; 16], 86_400).await;
}

// ---- refusal codes ---------------------------------------------------------------------------

#[tokio::test]
async fn client_refusals() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let other = host_id(&secret(77));

    assert_eq!(Phone::token(&rig, &other, &tok(1)).await.err(), Some(RefusalCode::HostOffline));
    assert_eq!(Phone::token(&rig, &host.id, &tok(99)).await.err(), Some(RefusalCode::NotAllowed));

    let hello = |extra: &str| format!(r#"{{"t":"hello","v":1,"host_id":"{}"{extra}}}"#, host.id);
    let cases = [
        ("not json".to_owned(), RefusalCode::BadRequest),
        (r#"{"t":"nope"}"#.to_owned(), RefusalCode::BadRequest),
        (hello(""), RefusalCode::BadRequest),
        (hello(&format!(r#","token":"{}","pairing_id":"{}""#, b64_encode(&tok(1)), b64_encode(&[1u8; 16]))), RefusalCode::BadRequest),
        (hello(r#","token":"short""#), RefusalCode::BadRequest),
        (format!(r#"{{"t":"hello","v":1,"host_id":"BAD","token":"{}"}}"#, b64_encode(&tok(1))), RefusalCode::BadRequest),
        (format!(r#"{{"t":"hello","v":2,"host_id":"{}","token":"{}"}}"#, host.id, b64_encode(&tok(1))), RefusalCode::UnsupportedVersion),
    ];
    for (text, code) in cases {
        assert_eq!(Phone::connect_raw(&rig, &text, &[]).await.err(), Some(code), "{text}");
    }
    // A binary message instead of the hello.
    let mut ws = ws_connect(&rig, "/v1/client", &[]).await.unwrap();
    send_bin(&mut ws, vec![1, 2, 3]).await;
    assert!(matches!(next(&mut ws).await, Got::Text(t) if t.contains("bad_request")));
}

#[tokio::test]
async fn host_refusals() {
    let rig = start_open().await;
    let good = secret(1);
    let id = host_id(&good);
    let hello = |v: u8, host_id: &str, secret: &str| format!(r#"{{"t":"hello","v":{v},"host_id":"{host_id}","host_secret":"{secret}"}}"#);
    let attempt = |text: String| {
        let rig = &rig;
        async move {
            let mut ws = ws_connect(rig, "/v1/host", &[]).await.unwrap();
            send_text(&mut ws, &text).await;
            let Got::Text(t) = next(&mut ws).await else { panic!("no reply") };
            match serde_json::from_str::<RelayToHost>(&t).unwrap() {
                RelayToHost::Error { code } => code,
                other => panic!("{other:?}"),
            }
        }
    };
    // The secret does not hash to the claimed id.
    assert_eq!(attempt(hello(1, &id, &b64_encode(&secret(2)))).await, RefusalCode::NotAllowed);
    assert_eq!(attempt(hello(2, &id, &b64_encode(&good))).await, RefusalCode::UnsupportedVersion);
    assert_eq!(attempt(hello(1, "nope", &b64_encode(&good))).await, RefusalCode::BadRequest);
    assert_eq!(attempt(hello(1, &id, "short")).await, RefusalCode::BadRequest);
    assert_eq!(attempt("{".to_owned()).await, RefusalCode::BadRequest);
    // Control messages before hello are refused too.
    assert_eq!(attempt(r#"{"t":"allow","tokens":[]}"#.to_owned()).await, RefusalCode::BadRequest);
}

#[tokio::test]
async fn bad_control_messages_get_an_error_and_the_host_stays_connected() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    send_text(&mut host.ws, "{").await;
    assert_eq!(host.reply().await, RelayToHost::Error { code: RefusalCode::BadRequest });
    send_text(&mut host.ws, r#"{"t":"allow","tokens":["nope"]}"#).await;
    assert_eq!(host.reply().await, RelayToHost::Error { code: RefusalCode::BadRequest });
    send_text(&mut host.ws, r#"{"t":"pairing_open","pairing_id":"x","expires_in":5}"#).await;
    assert_eq!(host.reply().await, RelayToHost::Error { code: RefusalCode::BadRequest });
    send_text(&mut host.ws, r#"{"t":"hello","v":1,"host_id":"x","host_secret":"y"}"#).await;
    assert_eq!(host.reply().await, RelayToHost::Error { code: RefusalCode::BadRequest });
    // Still working.
    host.allow(&[]).await;
}

#[tokio::test]
async fn a_flood_of_garbage_disconnects_the_host() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    for _ in 0..10 {
        send_bin(&mut host.ws, vec![9, 9]).await;
    }
    assert!(matches!(next(&mut host.ws).await, Got::Close(c) if refusal(c) == Some(RefusalCode::BadRequest)));
}

#[tokio::test]
async fn too_many_allow_entries_is_relay_full() {
    let mut c = open_config();
    c.limits.max_allow_entries = 2;
    let rig = start(c, None).await;
    let mut host = Host::connect(&rig, 1).await;
    let r = host.control(&HostToRelay::Allow { tokens: (1..=3).map(|i| b64_encode(&token_hash(&tok(i)))).collect() }).await;
    assert_eq!(r, RelayToHost::Error { code: RefusalCode::RelayFull });
}

#[tokio::test]
async fn oversize_frames_are_refused_and_the_channel_ends() {
    let mut c = open_config();
    c.limits.max_frame_bytes = 2048;
    let rig = start(c, None).await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let mut phone = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    let (_, ch, _) = host.frame().await;
    phone.send(&vec![0u8; 4096]).await;
    // The relay drops the phone socket (too big) and the host hears the channel is closed.
    let gone = phone.recv().await;
    assert!(matches!(gone, Got::Gone | Got::Close(_)), "{gone:?}");
    let (kind, closed, _) = host.frame().await;
    assert_eq!((kind, closed), (FrameKind::Close, ch));
}

// ---- rate limits and capacity ----------------------------------------------------------------

#[tokio::test]
async fn per_channel_frame_rate_is_limited() {
    let mut c = open_config();
    c.limits.frames_per_second_per_channel = 5; // burst 10
    let rig = start(c, None).await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let mut phone = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    let (_, ch, _) = host.frame().await;
    for _ in 0..40 {
        // The relay may close mid-flood; a failed send is fine.
        let _ = phone.ws.send(Message::Binary(vec![1u8; 8].into())).await;
    }
    let mut closed = None;
    for _ in 0..60 {
        match host.frame().await {
            (FrameKind::Close, c2, payload) => {
                assert_eq!(c2, ch);
                closed = Some(parse_close_payload(&payload));
                break;
            }
            (FrameKind::Data, ..) => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(closed, Some(Some(RefusalCode::RateLimited)));
}

#[tokio::test]
async fn per_channel_rate_applies_to_host_to_phone_too() {
    let mut c = open_config();
    c.limits.frames_per_second_per_channel = 5;
    let rig = start(c, None).await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let mut phone = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    let (_, ch, _) = host.frame().await;
    for _ in 0..40 {
        let _ = host.ws.send(Message::Binary(encode_frame(FrameKind::Data, ch, b"x").into())).await;
    }
    let mut last = None;
    for _ in 0..60 {
        match phone.recv().await {
            Got::Bin(_) => {}
            other => {
                last = Some(other);
                break;
            }
        }
    }
    assert!(matches!(last, Some(Got::Close(c)) if refusal(c) == Some(RefusalCode::RateLimited)), "{last:?}");
}

#[tokio::test]
async fn per_ip_new_connection_rate_is_limited() {
    let mut c = open_config();
    c.limits.new_connections_per_ip_per_minute = 3;
    let rig = start(c, None).await;
    let mut host = Host::connect(&rig, 1).await; // 1
    host.allow(&[tok(1)]).await;
    let _a = Phone::token(&rig, &host.id, &tok(1)).await.unwrap(); // 2
    let _b = Phone::token(&rig, &host.id, &tok(1)).await.unwrap(); // 3
    assert_eq!(Phone::token(&rig, &host.id, &tok(1)).await.err(), Some(RefusalCode::RateLimited)); // 4
}

#[tokio::test]
async fn per_ip_open_socket_cap() {
    let mut c = open_config();
    c.limits.max_sockets_per_ip = 3;
    let rig = start(c, None).await;
    let mut host = Host::connect(&rig, 1).await; // 1
    host.allow(&[tok(1)]).await;
    let _a = Phone::token(&rig, &host.id, &tok(1)).await.unwrap(); // 2
    let b = Phone::token(&rig, &host.id, &tok(1)).await.unwrap(); // 3
    assert_eq!(Phone::token(&rig, &host.id, &tok(1)).await.err(), Some(RefusalCode::RateLimited));
    // Closing one frees a slot.
    drop(b);
    let mut ok = false;
    for _ in 0..50 {
        if Phone::token(&rig, &host.id, &tok(1)).await.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ok);
}

#[tokio::test]
async fn forwarded_for_is_used_only_when_trusted() {
    // Trusted: distinct forwarded addresses are separate budgets.
    let mut c = open_config();
    c.trust_forwarded_for = true;
    c.limits.new_connections_per_ip_per_minute = 2;
    let rig = start(c, None).await;
    let mut host = Host::try_connect(&rig, 1, None, &[("x-forwarded-for", "9.9.9.1")]).await.unwrap();
    host.allow(&[tok(1)]).await;
    for ip in ["9.9.9.2", "9.9.9.3", "9.9.9.4"] {
        let hello = hello_token(&host.id, &tok(1));
        assert!(Phone::connect_raw(&rig, &hello, &[("x-forwarded-for", ip)]).await.is_ok(), "{ip}");
    }
    // The rightmost entry counts (the one our proxy added); a spoofed leftmost one does not help.
    let hello = hello_token(&host.id, &tok(1));
    let h = [("x-forwarded-for", "1.1.1.1, 9.9.9.2")];
    assert!(Phone::connect_raw(&rig, &hello, &h).await.is_ok());
    assert_eq!(Phone::connect_raw(&rig, &hello, &h).await.err(), Some(RefusalCode::RateLimited));

    // Not trusted: the header is ignored, everything is one address.
    let mut c = open_config();
    c.limits.new_connections_per_ip_per_minute = 2;
    let rig = start(c, None).await;
    let mut host = Host::try_connect(&rig, 1, None, &[("x-forwarded-for", "9.9.9.1")]).await.unwrap();
    host.allow(&[tok(1)]).await;
    let hello = hello_token(&host.id, &tok(1));
    assert!(Phone::connect_raw(&rig, &hello, &[("x-forwarded-for", "9.9.9.2")]).await.is_ok());
    assert_eq!(Phone::connect_raw(&rig, &hello, &[("x-forwarded-for", "9.9.9.3")]).await.err(), Some(RefusalCode::RateLimited));
}

#[tokio::test]
async fn global_host_cap_and_channel_cap() {
    let mut c = open_config();
    c.limits.max_hosts = 1;
    c.limits.max_channels_per_host = 1;
    let rig = start(c, None).await;
    let host = Host::connect(&rig, 1).await;
    assert_eq!(Host::try_connect(&rig, 2, None, &[]).await.err(), Some(RefusalCode::RelayFull));
    // The same host reconnecting is not a new host.
    let mut again = Host::connect(&rig, 1).await;
    again.allow(&[tok(1)]).await;
    let _p = Phone::token(&rig, &again.id, &tok(1)).await.unwrap();
    assert_eq!(Phone::token(&rig, &again.id, &tok(1)).await.err(), Some(RefusalCode::RelayFull));
    drop(host);
}

#[tokio::test]
async fn silent_peers_are_closed() {
    let mut c = open_config();
    c.limits.host_silence_secs = 1;
    c.limits.client_idle_secs = 1;
    c.limits.hello_timeout_secs = 1;
    let rig = start(c, None).await;
    // A socket that never says hello.
    let mut ws = ws_connect(&rig, "/v1/client", &[]).await.unwrap();
    assert!(matches!(next(&mut ws).await, Got::Text(t) if t.contains("idle_timeout")));
    // A silent host, and a silent phone.
    let mut host = Host::connect(&rig, 1).await;
    assert!(matches!(next(&mut host.ws).await, Got::Close(c) if refusal(c) == Some(RefusalCode::IdleTimeout)));
    // A silent phone (the host stays well within its own silence limit here).
    let mut c = open_config();
    c.limits.client_idle_secs = 1;
    let rig = start(c, None).await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let mut phone = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    assert!(matches!(phone.recv().await, Got::Close(c) if refusal(c) == Some(RefusalCode::IdleTimeout)));
}

// ---- operator access control ---------------------------------------------------------------------

#[tokio::test]
async fn closed_relay_admits_listed_hosts_and_enrolment_tokens_only() {
    let listed = host_id(&secret(1));
    let mut c = Config::default();
    c.access.allowed_hosts = vec![listed.clone()];
    c.access.enrol_tokens = vec!["umage-team-enrolment-token".into()];
    let rig = start(c, None).await;

    // Listed by id.
    let _listed_host = Host::try_connect(&rig, 1, None, &[]).await.unwrap();
    // Unlisted and no token.
    assert_eq!(Host::try_connect(&rig, 2, None, &[]).await.err(), Some(RefusalCode::NotEnrolled));
    // Wrong token.
    assert_eq!(Host::try_connect(&rig, 2, Some("umage-team-enrolment-WRONG"), &[]).await.err(), Some(RefusalCode::NotEnrolled));
    // Right token, any id.
    let mut host = Host::try_connect(&rig, 2, Some("umage-team-enrolment-token"), &[]).await.unwrap();
    // Admitted hosts work end to end.
    host.allow(&[tok(1)]).await;
    assert!(Phone::token(&rig, &host.id, &tok(1)).await.is_ok());
    // Phones are not subject to host access control (the host's allow list gates them).
    assert_eq!(Phone::token(&rig, &listed, &tok(1)).await.err(), Some(RefusalCode::NotAllowed));
}

#[tokio::test]
async fn enrolment_token_may_be_configured_as_a_hash() {
    let mut c = Config::default();
    c.access.enrol_tokens = vec![csm_relay::config::enrol_token_entry("umage-team-enrolment-token")];
    let rig = start(c, None).await;
    assert!(Host::try_connect(&rig, 5, Some("umage-team-enrolment-token"), &[]).await.is_ok());
    assert_eq!(Host::try_connect(&rig, 5, None, &[]).await.err(), Some(RefusalCode::NotEnrolled));
}

#[tokio::test]
async fn allowed_hosts_file_takes_effect_without_restart() {
    let dir = std::env::temp_dir().join(format!("csm-relay-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("hosts.txt");
    std::fs::write(&file, "# nobody yet\n").unwrap();
    let mut c = Config::default();
    c.access.allowed_hosts_file = Some(file.clone());
    let rig = start(c, None).await;
    assert_eq!(Host::try_connect(&rig, 1, None, &[]).await.err(), Some(RefusalCode::NotEnrolled));
    std::fs::write(&file, format!("{}  # Allan\n", host_id(&secret(1)))).unwrap();
    assert!(Host::try_connect(&rig, 1, None, &[]).await.is_ok());
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn oversized_enrolment_token_is_a_bad_request() {
    let mut c = Config::default();
    c.access.enrol_tokens = vec!["umage-team-enrolment-token".into()];
    let rig = start(c, None).await;
    assert_eq!(Host::try_connect(&rig, 1, Some(&"x".repeat(500)), &[]).await.err(), Some(RefusalCode::BadRequest));
}

// ---- http endpoints and origin ---------------------------------------------------------------

async fn http_get(rig: &Rig, path: &str, headers: &str) -> String {
    let mut s = TcpStream::connect(rig.addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: relay.test\r\nConnection: close\r\n{headers}\r\n").as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn healthz_vapid_and_nothing_else() {
    let rig = start(open_config(), Some(Arc::new(FakePush::default()))).await;
    assert!(http_get(&rig, "/healthz", "").await.starts_with("HTTP/1.1 200"));
    let v = http_get(&rig, "/v1/vapid", "Origin: https://phone.example.test\r\n").await;
    assert!(v.starts_with("HTTP/1.1 200"), "{v}");
    assert!(v.contains(r#"{"public_key":"BPUBLICKEY"}"#));
    assert!(v.to_ascii_lowercase().contains("access-control-allow-origin: https://phone.example.test"));
    for p in ["/", "/v1", "/v1/stats", "/metrics", "/admin"] {
        assert!(http_get(&rig, p, "").await.starts_with("HTTP/1.1 404"), "{p}");
    }
    // Push disabled: no VAPID key to hand out.
    let rig = start_open().await;
    assert!(http_get(&rig, "/v1/vapid", "").await.starts_with("HTTP/1.1 404"));
}

#[tokio::test]
async fn client_origin_check_when_configured() {
    let mut c = open_config();
    c.allowed_origins = vec!["https://phone.example.test".into()];
    let rig = start(c, None).await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let hello = hello_token(&host.id, &tok(1));
    assert!(Phone::connect_raw(&rig, &hello, &[("origin", "https://phone.example.test")]).await.is_ok());
    let denied = ws_connect(&rig, "/v1/client", &[("origin", "https://evil.example")]).await;
    match denied {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status().as_u16(), 403),
        other => panic!("expected a 403, got {:?}", other.map(|_| "connected")),
    }
    // Non-browser clients send no Origin and pass: this check is defence in depth only.
    assert!(Phone::connect_raw(&rig, &hello, &[]).await.is_ok());
}

// ---- push -----------------------------------------------------------------------------------

fn push_req(id: u32, endpoint: &str, body: &[u8]) -> HostToRelay {
    HostToRelay::Push(PushRequest { id, endpoint: endpoint.to_owned(), ttl: 60, urgency: None, topic: None, body: b64_encode(body) })
}

#[tokio::test]
async fn push_is_forwarded_checked_and_throttled() {
    let fake = Arc::new(FakePush::default());
    let mut c = open_config();
    c.push.per_endpoint_per_hour = 2;
    c.push.per_host_per_hour = 3;
    let rig = start(c, Some(fake.clone())).await;
    let mut host = Host::connect(&rig, 1).await;
    let fcm = "https://fcm.googleapis.com/fcm/send/abc";

    // Forwarded: the relay reports the service's answer; the body is passed on untouched.
    send_text(&mut host.ws, &csm_remote_proto::relay::to_text(&push_req(1, fcm, b"ciphertext"))).await;
    assert_eq!(host.reply().await, RelayToHost::PushResult { id: 1, ok: true, status: Some(201), code: None });
    {
        let jobs = fake.jobs.lock().unwrap();
        assert_eq!((jobs.len(), jobs[0].body.as_slice(), jobs[0].ttl), (1, &b"ciphertext"[..], 60));
    }

    let refused = |id, code| RelayToHost::PushResult { id, ok: false, status: None, code: Some(code) };
    // Not a known push service (SSRF guard).
    assert_eq!(host.control(&push_req(2, "https://169.254.169.254/latest", b"x")).await, refused(2, RefusalCode::PushRefused));
    assert_eq!(host.control(&push_req(3, "http://fcm.googleapis.com/x", b"x")).await, refused(3, RefusalCode::PushRefused));
    // Bad body: empty, not base64, too big.
    assert_eq!(host.control(&push_req(4, fcm, b"")).await, refused(4, RefusalCode::BadRequest));
    assert_eq!(host.control(&push_req(5, fcm, &[0u8; 4097])).await, refused(5, RefusalCode::BadRequest));
    // Per-endpoint cap (2 per hour; one already used).
    assert_eq!(host.control(&push_req(6, fcm, b"y")).await, RelayToHost::PushResult { id: 6, ok: true, status: Some(201), code: None });
    assert_eq!(host.control(&push_req(7, fcm, b"z")).await, refused(7, RefusalCode::RateLimited));
    // Per-host cap (3 per hour; two used on fcm; a third on another endpoint, then none).
    let other = "https://updates.push.services.mozilla.com/wpush/v2/q";
    assert_eq!(host.control(&push_req(8, other, b"a")).await, RelayToHost::PushResult { id: 8, ok: true, status: Some(201), code: None });
    assert_eq!(host.control(&push_req(9, other, b"b")).await, refused(9, RefusalCode::RateLimited));
    assert_eq!(fake.jobs.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn push_with_push_disabled_is_refused() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    let r = host.control(&push_req(1, "https://fcm.googleapis.com/x", b"x")).await;
    assert_eq!(r, RelayToHost::PushResult { id: 1, ok: false, status: None, code: Some(RefusalCode::PushRefused) });
}

// ---- logs -----------------------------------------------------------------------------------

#[tokio::test]
async fn relay_state_does_not_outlive_its_connections() {
    let rig = start_open().await;
    let mut host = Host::connect(&rig, 1).await;
    host.allow(&[tok(1)]).await;
    let phone = Phone::token(&rig, &host.id, &tok(1)).await.unwrap();
    assert_eq!(rig.relay.counts(), (1, 1));
    drop(phone);
    drop(host);
    for _ in 0..100 {
        if rig.relay.counts() == (0, 0) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("state leaked: {:?}", rig.relay.counts());
}

// ---- CSM peers: their own namespace ------------------------------------------------------------

async fn peer_client(rig: &Rig, host: &str, token: &[u8; 32]) -> Result<Phone, RefusalCode> {
    Phone::connect_at(rig, "/v1/peer-client", &hello_token(host, token), &[]).await
}

#[tokio::test]
async fn a_peer_channel_routes_bytes_both_ways_in_the_peer_namespace() {
    let rig = start_open().await;
    let mut host = Host::try_connect_at(&rig, "/v1/peer-host", 3, None, &[]).await.unwrap();
    host.allow(&[tok(7)]).await;
    let mut client = peer_client(&rig, &host.id, &tok(7)).await.unwrap();
    let (kind, ch, payload) = host.frame().await;
    assert_eq!(kind, FrameKind::Open);
    assert_eq!(OpenInfo::decode(&payload), Some(OpenInfo::Device(token_hash(&tok(7)))));
    let up: Vec<u8> = (0..=255u8).collect();
    client.send(&up).await;
    assert_eq!(host.frame().await, (FrameKind::Data, ch, up.clone()));
    host.send_data(ch, &up).await;
    assert!(matches!(client.recv().await, Got::Bin(b) if b == up));
}

#[tokio::test]
async fn phones_and_peers_never_reach_each_others_hosts() {
    let rig = start_open().await;
    // The same host id, registered once as a phone host and once as a peer host: two namespaces.
    let mut phone_host = Host::connect(&rig, 4).await;
    let mut peer_host = Host::try_connect_at(&rig, "/v1/peer-host", 4, None, &[]).await.unwrap();
    assert_eq!(phone_host.id, peer_host.id);
    phone_host.allow(&[tok(1)]).await;
    peer_host.allow(&[tok(1)]).await;
    // Each endpoint reaches only its own host, even with the same credential.
    let _phone = Phone::token(&rig, &phone_host.id, &tok(1)).await.unwrap();
    assert!(matches!(phone_host.frame().await, (FrameKind::Open, _, _)));
    let _peer = peer_client(&rig, &peer_host.id, &tok(1)).await.unwrap();
    assert!(matches!(peer_host.frame().await, (FrameKind::Open, _, _)));

    // A host that exists only as a peer host is invisible to phones, and the reverse.
    let mut peer_only = Host::try_connect_at(&rig, "/v1/peer-host", 5, None, &[]).await.unwrap();
    peer_only.allow(&[tok(2)]).await;
    assert_eq!(Phone::token(&rig, &peer_only.id, &tok(2)).await.err(), Some(RefusalCode::HostOffline));
    let mut phone_only = Host::connect(&rig, 6).await;
    phone_only.allow(&[tok(3)]).await;
    assert_eq!(peer_client(&rig, &phone_only.id, &tok(3)).await.err(), Some(RefusalCode::HostOffline));
}

#[tokio::test]
async fn a_peer_host_cannot_send_a_push() {
    let rig = start_open().await;
    let mut host = Host::try_connect_at(&rig, "/v1/peer-host", 3, None, &[]).await.unwrap();
    let r = host.control(&push_req(1, "https://fcm.googleapis.com/x", b"x")).await;
    assert!(matches!(r, RelayToHost::PushResult { ok: false, code: Some(RefusalCode::PushRefused), .. }), "{r:?}");
}

#[tokio::test]
async fn browsers_cannot_open_the_peer_endpoints() {
    let rig = start_open().await;
    let origin = [("origin", "https://phone.example")];
    assert!(ws_connect(&rig, "/v1/peer-host", &origin).await.is_err());
    assert!(ws_connect(&rig, "/v1/peer-client", &origin).await.is_err());
}

#[tokio::test]
async fn peer_channels_take_their_own_higher_limits() {
    // 300 frames at once: far over a phone channel's 50 frames/s (burst 100), well within a peer's.
    let rig = start_open().await;
    let mut host = Host::try_connect_at(&rig, "/v1/peer-host", 3, None, &[]).await.unwrap();
    host.allow(&[tok(7)]).await;
    let mut client = peer_client(&rig, &host.id, &tok(7)).await.unwrap();
    let _ = host.frame().await;
    for i in 0..300u32 {
        client.send(&i.to_be_bytes()).await;
    }
    for i in 0..300u32 {
        match host.frame().await {
            (FrameKind::Data, _, payload) => assert_eq!(payload, i.to_be_bytes()),
            other => panic!("frame {i}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn peer_frame_rate_is_configurable_and_enforced() {
    let mut c = open_config();
    c.limits.peer_frames_per_second_per_channel = 5;
    let rig = start(c, None).await;
    let mut host = Host::try_connect_at(&rig, "/v1/peer-host", 3, None, &[]).await.unwrap();
    host.allow(&[tok(7)]).await;
    let mut client = peer_client(&rig, &host.id, &tok(7)).await.unwrap();
    let _ = host.frame().await;
    for _ in 0..60 {
        let _ = client.ws.send(Message::Binary(b"x".to_vec().into())).await;
    }
    let mut closed = None;
    for _ in 0..80 {
        match host.frame().await {
            (FrameKind::Close, _, payload) => {
                closed = Some(parse_close_payload(&payload));
                break;
            }
            (FrameKind::Data, ..) => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(closed, Some(Some(RefusalCode::RateLimited)));
}

#[tokio::test]
async fn revoking_a_peer_device_drops_its_channel_at_once() {
    let rig = start_open().await;
    let mut host = Host::try_connect_at(&rig, "/v1/peer-host", 3, None, &[]).await.unwrap();
    host.allow(&[tok(7)]).await;
    let mut client = peer_client(&rig, &host.id, &tok(7)).await.unwrap();
    let _ = host.frame().await;
    host.allow(&[]).await;
    assert!(matches!(client.recv().await, Got::Close(Some(c)) if refusal(Some(c)) == Some(RefusalCode::Revoked)));
}

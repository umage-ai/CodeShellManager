//! The two WebSocket endpoints. Text messages are relay control; binary messages are opaque.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use csm_remote_proto::PROTOCOL_VERSION;
use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{HOST_SECRET_LEN, PAIRING_ID_LEN, TOKEN_LEN, b64_decode_fixed, host_id as derive_host_id, ct_eq, is_valid_host_id, sha256};
use csm_remote_proto::limits::{MAX_CONTROL_BYTES, MAX_HOST_ACCESS_TOKEN_BYTES, MAX_PUSH_BODY_BYTES, MAX_PUSH_ENDPOINT_BYTES, MAX_PUSH_TTL_SECS};
use csm_remote_proto::relay::{
    ClientToRelay, FrameError, FrameKind, HostToRelay, LimitsInfo, PeerClientText, PushRequest, RelayToClient, RelayToHost, decode_frame, encode_ack, encode_frame,
    parse_close_payload, to_text,
};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};

use crate::limiter::ChannelLimiter;
use crate::push::{PushJob, endpoint_allowed};
use crate::state::{ClientOut, Credential, HOST_QUEUE, HostOut, HostTx, IpGuard, Kind, Relay};

/// A host that sends this many malformed messages is disconnected.
const MAX_BAD_MESSAGES: u32 = 10;

/// The client address: the socket peer, or the last `X-Forwarded-For` entry behind a trusted proxy.
pub fn client_ip(relay: &Relay, peer: SocketAddr, forwarded_for: Option<&str>) -> IpAddr {
    if relay.settings.trust_forwarded_for
        && let Some(ip) = forwarded_for.and_then(|v| v.rsplit(',').next()).and_then(|s| s.trim().parse::<IpAddr>().ok())
    {
        return ip;
    }
    peer.ip()
}

async fn close_with(ws: &mut WebSocket, code: Option<RefusalCode>) {
    let frame = match code {
        Some(c) => CloseFrame { code: c.ws_close_code(), reason: c.as_str().into() },
        None => CloseFrame { code: 1000, reason: "".into() },
    };
    let _ = ws.send(Message::Close(Some(frame))).await;
}

async fn refuse_host(ws: &mut WebSocket, code: RefusalCode) {
    tracing::info!(side = "host", refused = %code, "refused");
    let _ = ws.send(Message::Text(to_text(&RelayToHost::Error { code }).into())).await;
    close_with(ws, Some(code)).await;
}

async fn refuse_client(ws: &mut WebSocket, code: RefusalCode) {
    tracing::info!(side = "client", refused = %code, "refused");
    let _ = ws.send(Message::Text(to_text(&RelayToClient::Error { code }).into())).await;
    close_with(ws, Some(code)).await;
}

/// The first text message of a socket, within the hello timeout. `Err(None)` = the peer just left.
async fn first_text(ws: &mut WebSocket, secs: u64) -> Result<String, Option<RefusalCode>> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        match timeout(deadline.saturating_duration_since(Instant::now()), ws.recv()).await {
            Err(_) => return Err(Some(RefusalCode::IdleTimeout)),
            Ok(None | Some(Err(_))) => return Err(None),
            Ok(Some(Ok(Message::Text(t)))) => {
                return if t.len() > MAX_CONTROL_BYTES { Err(Some(RefusalCode::FrameTooLarge)) } else { Ok(t.as_str().to_owned()) };
            }
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            Ok(Some(Ok(Message::Close(_)))) => return Err(None),
            Ok(Some(Ok(Message::Binary(_)))) => return Err(Some(RefusalCode::BadRequest)),
        }
    }
}

// ---- host socket ---------------------------------------------------------------------------

pub async fn host_socket(relay: Arc<Relay>, kind: Kind, mut ws: WebSocket, ip: Result<IpGuard, RefusalCode>) {
    let _guard = match ip {
        Ok(g) => g,
        Err(code) => return refuse_host(&mut ws, code).await,
    };
    let lim = relay.settings.limits.clone();
    let text = match first_text(&mut ws, lim.hello_timeout_secs).await {
        Ok(t) => t,
        Err(Some(code)) => return refuse_host(&mut ws, code).await,
        Err(None) => return,
    };
    let host_id = match authenticate_host(&relay, &text).await {
        Ok(v) => v,
        Err(code) => return refuse_host(&mut ws, code).await,
    };
    let (tx, mut rx) = mpsc::channel::<HostOut>(HOST_QUEUE);
    let conn = match relay.register_host(kind, &host_id, tx) {
        Ok(c) => c,
        Err(code) => return refuse_host(&mut ws, code).await,
    };
    tracing::info!(side = "host", "connected");
    let welcome = RelayToHost::Welcome {
        v: PROTOCOL_VERSION,
        limits: LimitsInfo {
            max_frame_bytes: u32::try_from(lim.max_frame_bytes).unwrap_or(u32::MAX),
            // This kind's own caps: a peer host is told the peer limits, not the phone ones.
            max_channels: u32::try_from(relay.caps(kind).max_channels).unwrap_or(u32::MAX),
            max_allow_entries: u32::try_from(lim.max_allow_entries).unwrap_or(u32::MAX),
            max_pairing_secs: lim.max_pairing_secs,
            push: relay.push.is_some() && kind == Kind::Phone,
            frames_per_second: relay.caps(kind).frames_per_second,
            bytes_per_second: relay.caps(kind).bytes_per_second,
        },
    };
    if ws.send(Message::Text(to_text(&welcome).into())).await.is_err() {
        relay.unregister_host(kind, &host_id, conn);
        return;
    }

    let silence = Duration::from_secs(lim.host_silence_secs);
    let mut last = Instant::now();
    let mut bad = 0u32;
    loop {
        tokio::select! {
            incoming = ws.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                last = Instant::now();
                match msg {
                    Message::Text(t) => {
                        if t.len() > MAX_CONTROL_BYTES {
                            close_with(&mut ws, Some(RefusalCode::FrameTooLarge)).await;
                            break;
                        }
                        let reply = host_control(&relay, kind, &host_id, conn, t.as_str()).await;
                        if let Some(reply) = reply {
                            if matches!(reply, RelayToHost::Error { .. }) {
                                bad += 1;
                            }
                            if ws.send(Message::Text(to_text(&reply).into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Message::Binary(b) => {
                        if b.len() > lim.max_frame_bytes {
                            close_with(&mut ws, Some(RefusalCode::FrameTooLarge)).await;
                            break;
                        }
                        match decode_frame(&b) {
                            Ok(f) => match f.kind {
                                FrameKind::Data => relay.host_data(kind, &host_id, conn, f.channel, f.payload),
                                FrameKind::Close => relay.host_closes_channel(kind, &host_id, conn, f.channel, parse_close_payload(f.payload)),
                                FrameKind::Open | FrameKind::Ack => bad += 1,
                            },
                            Err(FrameError::UnknownKind(_)) => {}
                            Err(_) => bad += 1,
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Close(_) => break,
                }
                if bad >= MAX_BAD_MESSAGES {
                    close_with(&mut ws, Some(RefusalCode::BadRequest)).await;
                    break;
                }
            }
            out = rx.recv() => match out {
                Some(HostOut::Text(t)) => {
                    if ws.send(Message::Text(t.into())).await.is_err() { break; }
                }
                Some(HostOut::Binary(b)) => {
                    if ws.send(Message::Binary(b.into())).await.is_err() { break; }
                }
                Some(HostOut::Shutdown(code)) => {
                    close_with(&mut ws, Some(code)).await;
                    break;
                }
                None => break,
            },
            () = sleep_until(last + silence) => {
                tracing::info!(side = "host", refused = %RefusalCode::IdleTimeout, "closed");
                close_with(&mut ws, Some(RefusalCode::IdleTimeout)).await;
                break;
            }
        }
    }
    relay.unregister_host(kind, &host_id, conn);
    tracing::info!(side = "host", "disconnected");
}

/// Checks the hello: version, id shape, secret hashes to the id, and the operator's access rules.
async fn authenticate_host(relay: &Relay, text: &str) -> Result<String, RefusalCode> {
    let Ok(HostToRelay::Hello { v, host_id, host_secret, access }) = serde_json::from_str::<HostToRelay>(text) else {
        return Err(RefusalCode::BadRequest);
    };
    if v != PROTOCOL_VERSION {
        return Err(RefusalCode::UnsupportedVersion);
    }
    if !is_valid_host_id(&host_id) || access.as_ref().is_some_and(|a| a.len() > MAX_HOST_ACCESS_TOKEN_BYTES) {
        return Err(RefusalCode::BadRequest);
    }
    let secret = b64_decode_fixed::<HOST_SECRET_LEN>(&host_secret).ok_or(RefusalCode::BadRequest)?;
    if !ct_eq(derive_host_id(&secret).as_bytes(), host_id.as_bytes()) {
        return Err(RefusalCode::NotAllowed);
    }
    if !relay.settings.access.admits(&host_id, access.as_deref()).await {
        return Err(RefusalCode::NotEnrolled);
    }
    Ok(host_id)
}

/// One control message from a live host. `Some` = a reply to send.
async fn host_control(relay: &Arc<Relay>, kind: Kind, host_id: &str, conn: u64, text: &str) -> Option<RelayToHost> {
    let err = |code| Some(RelayToHost::Error { code });
    let Ok(msg) = serde_json::from_str::<HostToRelay>(text) else { return err(RefusalCode::BadRequest) };
    match msg {
        HostToRelay::Hello { .. } => err(RefusalCode::BadRequest),
        HostToRelay::Allow { tokens } => {
            let mut hashes = Vec::with_capacity(tokens.len());
            for t in &tokens {
                match b64_decode_fixed::<32>(t) {
                    Some(h) => hashes.push(h),
                    None => return err(RefusalCode::BadRequest),
                }
            }
            match relay.set_allow(kind, host_id, conn, hashes) {
                Ok(n) => Some(RelayToHost::AllowOk { count: u32::try_from(n).unwrap_or(u32::MAX) }),
                Err(code) => err(code),
            }
        }
        HostToRelay::PairingOpen { pairing_id, expires_in } => {
            let Some(id) = b64_decode_fixed::<PAIRING_ID_LEN>(&pairing_id) else { return err(RefusalCode::BadRequest) };
            match relay.open_slot(kind, host_id, conn, id, expires_in) {
                Ok(secs) => Some(RelayToHost::PairingOk { expires_in: secs }),
                Err(code) => err(code),
            }
        }
        HostToRelay::PairingClose => match relay.close_slot(kind, host_id, conn) {
            Ok(()) => None,
            Err(code) => err(code),
        },
        HostToRelay::Push(req) if kind == Kind::Phone => host_push(relay, host_id, conn, req),
        HostToRelay::Push(req) => Some(RelayToHost::PushResult { id: req.id, ok: false, status: None, code: Some(RefusalCode::PushRefused) }),
    }
}

fn host_push(relay: &Arc<Relay>, host_id: &str, conn: u64, req: PushRequest) -> Option<RelayToHost> {
    let refuse = |code| Some(RelayToHost::PushResult { id: req.id, ok: false, status: None, code: Some(code) });
    let Some(sender) = relay.push.clone() else { return refuse(RefusalCode::PushRefused) };
    if req.endpoint.len() > MAX_PUSH_ENDPOINT_BYTES || !endpoint_allowed(&req.endpoint, &relay.settings.push.allowed_hosts) {
        return refuse(RefusalCode::PushRefused);
    }
    let Some(body) = csm_remote_proto::ids::b64_decode(&req.body).filter(|b| !b.is_empty() && b.len() <= MAX_PUSH_BODY_BYTES) else {
        return refuse(RefusalCode::BadRequest);
    };
    if req.ttl > MAX_PUSH_TTL_SECS || req.topic.as_ref().is_some_and(|t| t.len() > 32 || !t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) {
        return refuse(RefusalCode::BadRequest);
    }
    if !relay.push_allowed(host_id, sha256(req.endpoint.as_bytes())) {
        return refuse(RefusalCode::RateLimited);
    }
    let tx: HostTx = relay.host_tx(Kind::Phone, host_id, conn)?;
    let id = req.id;
    let job = PushJob { endpoint: req.endpoint, ttl: req.ttl, urgency: req.urgency, topic: req.topic, body };
    tokio::spawn(async move {
        let out = sender.send(job).await;
        let code = (!out.ok).then_some(RefusalCode::PushFailed);
        tracing::info!(push_ok = out.ok, status = ?out.status, "push forwarded");
        let _ = tx.try_send(HostOut::Text(to_text(&RelayToHost::PushResult { id, ok: out.ok, status: out.status, code })));
    });
    None
}

// ---- phone socket ----------------------------------------------------------------------------

pub async fn client_socket(relay: Arc<Relay>, kind: Kind, mut ws: WebSocket, ip: Result<IpGuard, RefusalCode>) {
    let _guard = match ip {
        Ok(g) => g,
        Err(code) => return refuse_client(&mut ws, code).await,
    };
    let lim = relay.settings.limits.clone();
    let text = match first_text(&mut ws, lim.hello_timeout_secs).await {
        Ok(t) => t,
        Err(Some(code)) => return refuse_client(&mut ws, code).await,
        Err(None) => return,
    };
    let (host_id, cred) = match parse_client_hello(&text) {
        Ok(v) => v,
        Err(code) => return refuse_client(&mut ws, code).await,
    };
    let opened = match relay.open_channel(kind, &host_id, &cred) {
        Ok(o) => o,
        Err(code) => return refuse_client(&mut ws, code).await,
    };
    let (channel, host_tx, mut rx) = (opened.channel, opened.host_tx, opened.rx);
    if ws.send(Message::Text(to_text(&RelayToClient::ready()).into())).await.is_err() {
        relay.channel_gone(kind, &host_id, channel, None);
        return;
    }
    tracing::info!(side = "client", "channel open");

    let idle = Duration::from_secs(lim.client_idle_secs);
    let mut last = Instant::now();
    let caps = relay.caps(kind);
    let mut limiter = ChannelLimiter::new(caps.frames_per_second, caps.bytes_per_second, csm_remote_proto::limits::BURST_FACTOR, std::time::Instant::now());
    let mut why: Option<RefusalCode> = None;
    loop {
        tokio::select! {
            incoming = ws.recv() => {
                let Some(Ok(msg)) = incoming else { break };
                match msg {
                    Message::Binary(b) => {
                        last = Instant::now();
                        if b.len() > lim.max_frame_bytes {
                            why = Some(RefusalCode::FrameTooLarge);
                            close_with(&mut ws, why).await;
                            break;
                        }
                        if !limiter.allow(b.len(), std::time::Instant::now()) {
                            why = Some(RefusalCode::RateLimited);
                            close_with(&mut ws, why).await;
                            break;
                        }
                        let frame = encode_frame(FrameKind::Data, channel, &b);
                        if host_tx.try_send(HostOut::Binary(frame)).is_err() {
                            why = Some(RefusalCode::RateLimited);
                            close_with(&mut ws, why).await;
                            break;
                        }
                    }
                    // A peer client may report how much of its channel it has taken (an ack). The
                    // relay forwards only the counts, to the host, as an Ack frame; it never reads
                    // Noise bytes. Phones have no such message.
                    Message::Text(t) if kind == Kind::Peer && t.len() <= 256 => {
                        last = Instant::now();
                        let Ok(PeerClientText::Ack { frames, bytes }) = serde_json::from_str::<PeerClientText>(t.as_str()) else {
                            why = Some(RefusalCode::BadRequest);
                            close_with(&mut ws, why).await;
                            break;
                        };
                        if !limiter.allow(0, std::time::Instant::now()) {
                            why = Some(RefusalCode::RateLimited);
                            close_with(&mut ws, why).await;
                            break;
                        }
                        let frame = encode_frame(FrameKind::Ack, channel, &encode_ack(frames, bytes));
                        if host_tx.try_send(HostOut::Binary(frame)).is_err() {
                            why = Some(RefusalCode::RateLimited);
                            close_with(&mut ws, why).await;
                            break;
                        }
                    }
                    Message::Text(_) => {
                        why = Some(RefusalCode::BadRequest);
                        close_with(&mut ws, why).await;
                        break;
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Close(_) => break,
                }
            }
            out = rx.recv() => match out {
                Some(ClientOut::Binary(b)) => {
                    last = Instant::now();
                    if ws.send(Message::Binary(b.into())).await.is_err() { break; }
                }
                Some(ClientOut::Close(code)) => {
                    tracing::info!(side = "client", closed = ?code.map(RefusalCode::as_str), "channel closed by relay or host");
                    close_with(&mut ws, code).await;
                    // The channel was already removed by whoever sent this.
                    return;
                }
                None => break,
            },
            () = sleep_until(last + idle) => {
                why = Some(RefusalCode::IdleTimeout);
                close_with(&mut ws, why).await;
                break;
            }
        }
    }
    relay.channel_gone(kind, &host_id, channel, why);
    tracing::info!(side = "client", "channel closed");
}

fn parse_client_hello(text: &str) -> Result<(String, Credential), RefusalCode> {
    let Ok(ClientToRelay::Hello { v, host_id, token, pairing_id }) = serde_json::from_str::<ClientToRelay>(text) else {
        return Err(RefusalCode::BadRequest);
    };
    if v != PROTOCOL_VERSION {
        return Err(RefusalCode::UnsupportedVersion);
    }
    if !is_valid_host_id(&host_id) {
        return Err(RefusalCode::BadRequest);
    }
    let cred = match (token, pairing_id) {
        (Some(t), None) => {
            let raw = b64_decode_fixed::<TOKEN_LEN>(&t).ok_or(RefusalCode::BadRequest)?;
            Credential::Device(csm_remote_proto::ids::token_hash(&raw))
        }
        (None, Some(p)) => Credential::Pairing(b64_decode_fixed::<PAIRING_ID_LEN>(&p).ok_or(RefusalCode::BadRequest)?),
        _ => return Err(RefusalCode::BadRequest),
    };
    Ok((host_id, cred))
}

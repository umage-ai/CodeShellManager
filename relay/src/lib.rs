//! `csm-relay`: the relay of the CSM phone remote.
//!
//! Untrusted by design. It joins one CSM add-on (the *host*, connected outbound) to the phones
//! that pair with it, copying opaque end-to-end encrypted frames between them. It parses only the
//! [`csm_remote_proto::relay`] layer, keeps everything in memory, holds no secret that matters
//! (a host's secret proves a host id; push payloads arrive already encrypted), and never logs ids
//! at info level or payloads at all.
//!
//! Endpoints: `GET /v1/host` (WebSocket), `GET /v1/client` (WebSocket), `GET /v1/vapid`,
//! `GET /healthz`. Nothing else.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod config;
pub mod limiter;
pub mod push;
pub mod state;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use csm_remote_proto::codes::RefusalCode;
pub use state::Relay;

/// The HTTP/WebSocket router. Serve it with `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn router(relay: Arc<Relay>) -> Router {
    Router::new()
        .route("/v1/host", get(host_upgrade))
        .route("/v1/client", get(client_upgrade))
        .route("/v1/vapid", get(vapid))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(relay)
}

fn forwarded(headers: &HeaderMap) -> Option<&str> {
    headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
}

async fn host_upgrade(State(relay): State<Arc<Relay>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let ip = ws::client_ip(&relay, peer, forwarded(&headers));
    let guard = relay.admit_ip(ip);
    let max = relay.settings.limits.max_frame_bytes;
    upgrade.max_message_size(max).max_frame_size(max).on_upgrade(move |socket| ws::host_socket(relay, socket, guard))
}

async fn client_upgrade(State(relay): State<Arc<Relay>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if !origin_ok(&relay, &headers) {
        return (StatusCode::FORBIDDEN, RefusalCode::NotAllowed.as_str()).into_response();
    }
    let ip = ws::client_ip(&relay, peer, forwarded(&headers));
    let guard = relay.admit_ip(ip);
    let max = relay.settings.limits.max_frame_bytes;
    upgrade.max_message_size(max).max_frame_size(max).on_upgrade(move |socket| ws::client_socket(relay, socket, guard))
}

/// Browser clients send `Origin`; when the operator listed origins, only those pass. A client
/// without an Origin header is not a browser and is let through (this is not an auth check).
fn origin_ok(relay: &Relay, headers: &HeaderMap) -> bool {
    let list = &relay.settings.allowed_origins;
    if list.is_empty() {
        return true;
    }
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(o) => list.iter().any(|a| a.eq_ignore_ascii_case(o.trim_end_matches('/'))),
    }
}

async fn vapid(State(relay): State<Arc<Relay>>, headers: HeaderMap) -> Response {
    let Some(key) = relay.push.as_ref().and_then(|p| p.public_key()) else {
        return (StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "application/json")], r#"{"code":"push_refused"}"#).into_response();
    };
    let mut resp = ([(header::CONTENT_TYPE, "application/json")], serde_json::json!({"public_key": key}).to_string()).into_response();
    // The phone app lives on another origin and must read this: reflect an allowed origin, or any
    // when the operator did not restrict them (the key is public).
    let allow = match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(o) if origin_ok(&relay, &headers) => HeaderValue::from_str(o).ok(),
        _ if relay.settings.allowed_origins.is_empty() => Some(HeaderValue::from_static("*")),
        _ => None,
    };
    if let Some(v) = allow {
        resp.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        resp.headers_mut().insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"));
    resp
}

/// Serves until `shutdown` resolves, sweeping idle state in the background.
pub async fn serve(listener: tokio::net::TcpListener, relay: Arc<Relay>, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    let sweeper = {
        let relay = relay.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            let mut n = 0u32;
            loop {
                tick.tick().await;
                relay.sweep();
                n += 1;
                if n.is_multiple_of(10) {
                    let (hosts, channels) = relay.counts();
                    tracing::info!(hosts, channels, "status");
                }
            }
        })
    };
    let app = router(relay.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            relay.shutdown_all(RefusalCode::HostOffline);
        })
        .await;
    sweeper.abort();
    result
}

//! `csm-relay [--config relay.toml]`. See `relay/README.md`.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::PathBuf;
use std::sync::Arc;

use csm_relay::config::{Config, enrol_token_entry};
use csm_relay::push::{HttpPushSender, PushSender, Vapid};
use csm_relay::{Relay, serve};

const HELP: &str = "csm-relay: relay for the CSM phone remote

usage:
  csm-relay [--config <relay.toml>]     run the relay (config also from CSM_RELAY_CONFIG; CSM_RELAY_* override it)
  csm-relay enrol-token <secret>        print the sha256:<hex> config entry for an enrolment token
  csm-relay healthcheck                 exit 0 if a relay on this machine answers /healthz (for container health checks)
  csm-relay --help

Everything is configured in the TOML file or the environment; see README.md.";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("csm-relay: {msg}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path = std::env::var_os("CSM_RELAY_CONFIG").map(PathBuf::from);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(());
            }
            "--config" => config_path = Some(PathBuf::from(it.next().ok_or("--config needs a path")?)),
            "healthcheck" => {
                let mut config = match &config_path {
                    Some(p) => Config::load(p).map_err(|e| e.to_string())?,
                    None => Config::default(),
                };
                config.apply_env(&|k| std::env::var(k).ok());
                return healthcheck(&config.bind);
            }
            "enrol-token" => {
                let secret = it.next().ok_or("enrol-token needs the secret")?;
                println!("{}", enrol_token_entry(secret));
                return Ok(());
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }
    let mut config = match &config_path {
        Some(p) => Config::load(p).map_err(|e| e.to_string())?,
        None => Config::default(),
    };
    config.apply_env(&|k| std::env::var(k).ok());
    let settings = config.into_settings().map_err(|e| e.to_string())?;
    init_logging(settings.json_logs);

    let push: Option<Arc<dyn PushSender>> = if settings.push.enabled {
        let vapid = Vapid::load_or_create(&settings.push.vapid_key_file, &settings.push.vapid_subject).map_err(|e| e.to_string())?;
        Some(Arc::new(HttpPushSender::new(vapid)))
    } else {
        None
    };
    let bind = settings.bind;
    tracing::info!(%bind, access = %settings.access.describe(), push = push.is_some(), "starting");
    let relay = Relay::new(settings, push);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind).await.map_err(|e| format!("cannot listen on {bind}: {e}"))?;
        serve(listener, relay, async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await
        .map_err(|e| format!("server: {e}"))
    })
}

fn init_logging(json: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("CSM_RELAY_LOG").unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    if json { b.json().init() } else { b.init() }
}

/// A plain HTTP GET of `/healthz` on this machine's relay port (a distroless image has no curl).
fn healthcheck(bind: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    let port = bind.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()).ok_or("bind has no port")?;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = std::time::Duration::from_secs(3);
    let mut s = std::net::TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("cannot connect to {addr}: {e}"))?;
    s.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| e.to_string())?;
    if out.starts_with("HTTP/1.1 200") { Ok(()) } else { Err("unhealthy".to_owned()) }
}

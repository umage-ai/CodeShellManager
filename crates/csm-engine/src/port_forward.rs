//! SSH port forwards: a link to a loopback address on an SSH session's remote host
//! (`open_url` with the session) opens here only through a forward, and `session_port_forward`
//! starts one on demand.
//!
//! The rules (which arguments, which `LocalForward` lines, which URLs) are pure in
//! `csm-services::ssh_ports`. This module does the I/O: `ssh -G` (read the configured forwards)
//! and `ssh -N -L` (the forward itself), both as argv, both on a worker thread, never on the UI
//! thread. A forward lives in the runtime only: never persisted or restored. It is stopped when
//! its session is torn down (close, sleep, restart) and on quit.
//!
//! The process goes through [`SshRunner`], so tests run a fake ssh and production runs
//! [`system_ssh`].

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::Child;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use csm_core::models::ShellSession;
use csm_services::ssh_ports::{self, Connection};
use serde::Serialize;
use serde_json::json;

use crate::engine::{Ctx, ParkedReply, Reply};
use crate::tool::{ToolError, ToolOutput};

/// How long `ssh -G` may take before the link is treated as not forwarded.
const CONFIG_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a new forward may take to accept connections before it is given up.
pub(crate) const FORWARD_TIMEOUT: Duration = Duration::from_secs(20);
/// How often the watcher checks whether an ssh process ended on its own.
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// A running forward (or any process the engine holds on behalf of a session).
pub trait ForwardProcess: Send {
    /// False once the process has ended, for any reason.
    fn alive(&self) -> bool;
    fn pid(&self) -> u32;
    /// Ends the process. Idempotent.
    fn stop(&self);
}

/// Runs ssh. `args` are the connection arguments of [`Connection`] (no program name).
pub trait SshRunner: Send + Sync {
    /// The output of `ssh -G ...` (`args` start with `-G`, see [`Connection::config_args`]), or
    /// `None` when it fails or cannot run. Blocking.
    fn config(&self, args: &[String]) -> Option<String>;
    /// Starts `ssh <args>`, which the caller then owns. Returns once the process has started.
    fn start(&self, args: &[String]) -> io::Result<Box<dyn ForwardProcess>>;
}

/// The ssh the engine uses. Shared by every session; each call is independent.
pub type SshHandle = Arc<dyn SshRunner>;

/// No ssh at all: the default for tests and headless engines.
pub struct NoSsh;

impl SshRunner for NoSsh {
    fn config(&self, _args: &[String]) -> Option<String> {
        None
    }

    fn start(&self, _args: &[String]) -> io::Result<Box<dyn ForwardProcess>> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "ssh is not available in this engine"))
    }
}

/// The `ssh` on the PATH, run as argv.
pub struct SystemSsh;

impl SshRunner for SystemSsh {
    fn config(&self, args: &[String]) -> Option<String> {
        let out = crate::proc::capture_program("ssh", args, CONFIG_TIMEOUT)?;
        out.success.then(|| out.text())
    }

    fn start(&self, args: &[String]) -> io::Result<Box<dyn ForwardProcess>> {
        let child = crate::proc::spawn_child("ssh", args)?;
        Ok(Box::new(ChildForward::new(child)))
    }
}

/// The production ssh.
pub fn system_ssh() -> SshHandle {
    Arc::new(SystemSsh)
}

/// A child process held by the engine. A watcher thread notices when it ends on its own, and
/// dropping it kills it, so nothing outlives the engine.
struct ChildForward {
    pid: u32,
    child: Arc<Mutex<Child>>,
    alive: Arc<AtomicBool>,
}

impl ChildForward {
    fn new(child: Child) -> Self {
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));
        let alive = Arc::new(AtomicBool::new(true));
        let (watched, flag) = (child.clone(), alive.clone());
        let _ = std::thread::Builder::new().name("csm-port-forward".into()).spawn(move || {
            while flag.load(Ordering::SeqCst) {
                let ended = match watched.lock() {
                    Ok(mut c) => !matches!(c.try_wait(), Ok(None)),
                    Err(_) => true,
                };
                if ended {
                    flag.store(false, Ordering::SeqCst);
                    break;
                }
                std::thread::sleep(WATCH_EVERY);
            }
        });
        ChildForward { pid, child, alive }
    }
}

impl ForwardProcess for ChildForward {
    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn stop(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for ChildForward {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A forward the engine holds: session, remote port, local port, the process.
pub(crate) struct Forward {
    pub(crate) session_id: String,
    pub(crate) destination: String,
    pub(crate) remote_port: u16,
    pub(crate) local_port: u16,
    pub(crate) process: Box<dyn ForwardProcess>,
}

/// A live forward as the read model shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortForwardView {
    pub session_id: String,
    /// `user@host` (or the alias) the forward goes to.
    pub destination: String,
    pub remote_port: u16,
    pub local_port: u16,
    pub pid: u32,
    /// The address to open: `http://127.0.0.1:<local port>/`.
    pub url: String,
}

fn view_of(f: &Forward) -> PortForwardView {
    PortForwardView {
        session_id: f.session_id.clone(),
        destination: f.destination.clone(),
        remote_port: f.remote_port,
        local_port: f.local_port,
        pid: f.process.pid(),
        url: format!("http://127.0.0.1:{}/", f.local_port),
    }
}

/// The live forwards, for the snapshot.
pub(crate) fn live_views(forwards: &[Forward]) -> Vec<PortForwardView> {
    forwards.iter().filter(|f| f.process.alive()).map(view_of).collect()
}

/// Forgets the forwards whose process has ended (stopping what is left of them).
pub(crate) fn prune(forwards: &mut Vec<Forward>) {
    forwards.retain(|f| {
        let alive = f.process.alive();
        if !alive {
            f.process.stop();
        }
        alive
    });
}

/// Stops every forward of `session_id` (close, sleep, restart).
pub(crate) fn stop_session(forwards: &mut Vec<Forward>, session_id: &str) {
    forwards.retain(|f| {
        if f.session_id == session_id {
            f.process.stop();
            false
        } else {
            true
        }
    });
}

/// Stops the forwards of `session_id` on `local` (or all of them, for `None`). The number stopped.
pub(crate) fn stop_matching(forwards: &mut Vec<Forward>, session_id: &str, local: Option<u16>) -> usize {
    let before = forwards.len();
    forwards.retain(|f| {
        let hit = f.session_id == session_id && local.is_none_or(|port| f.local_port == port);
        if hit {
            f.process.stop();
        }
        !hit
    });
    before - forwards.len()
}

/// Stops every forward (quit).
pub(crate) fn stop_all(forwards: &mut Vec<Forward>) {
    for f in forwards.drain(..) {
        f.process.stop();
    }
}

/// The ssh connection a session's SSH command makes: the same argv the session launches, minus
/// the terminal flag and the remote command.
pub(crate) fn connection_for(s: &ShellSession) -> Result<Connection, String> {
    let plan = crate::launch::plan_with(s, None, "", "/bin/sh", None)?;
    ssh_ports::connection_of(&plan.argv)
}

/// Whether the port is free on this machine's loopback interface.
fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// A free loopback port the OS picks.
fn ephemeral_port() -> Option<u16> {
    TcpListener::bind(("127.0.0.1", 0)).ok()?.local_addr().ok().map(|a| a.port())
}

/// The local port for a forward: the requested one (must be free), else the remote port when it
/// is free here, else an ephemeral free port.
pub(crate) fn choose_local_port(remote: u16, requested: Option<u16>) -> Result<u16, ToolError> {
    match requested {
        Some(p) if port_is_free(p) => Ok(p),
        Some(p) => Err(ToolError::Refused(format!("local port {p} is in use on this computer; pick another or leave local_port out"))),
        None if port_is_free(remote) => Ok(remote),
        None => ephemeral_port().ok_or_else(|| ToolError::Failed("no free local port is available".into())),
    }
}

/// Waits until `port` accepts connections, the process ends, or `timeout` passes.
fn wait_ready(process: &dyn ForwardProcess, port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    loop {
        if !process.alive() {
            return Err("ssh stopped before the forward was ready (the host, the key or the config may be wrong)".into());
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("no answer within {} seconds", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Starts a forward and waits for it. Runs on a worker. On failure nothing is left running.
pub(crate) fn start_forward(
    ssh: &dyn SshRunner,
    conn: &Connection,
    remote: u16,
    requested: Option<u16>,
    timeout: Duration,
) -> Result<(u16, Box<dyn ForwardProcess>), ToolError> {
    let local = choose_local_port(remote, requested)?;
    let process = ssh.start(&conn.forward_args(local, remote)).map_err(|e| ToolError::Failed(format!("could not start ssh: {e}")))?;
    match wait_ready(process.as_ref(), local, timeout) {
        Ok(()) => Ok((local, process)),
        Err(why) => {
            process.stop();
            Err(ToolError::Failed(format!("port {remote} on {} was not forwarded: {why}", conn.destination)))
        }
    }
}

/// The reply for a link to a loopback port of an SSH session's remote host.
///
/// A forward that exists (or one `ssh -G` lists for this port) opens the link at its local
/// port. Otherwise nothing opens: the reply says the port is on the remote host, so the UI can
/// offer "Forward and open".
pub(crate) fn open_loopback(ctx: &mut Ctx<'_>, session: &ShellSession, url: String, port: u16) -> Reply {
    let conn = connection_for(session).map_err(ToolError::Refused)?;
    let destination = conn.destination.clone();
    let session_id = session.id.clone();
    if let Some(local) = forward_for(ctx.state.rt.forwards.as_slice(), &session.id, port) {
        return open_at(ctx, &url, port, local, &destination);
    }
    let ssh = ctx.state.options.ssh.clone();
    let opener = ctx.state.options.url_opener.clone();
    crate::tools::open::on_worker(ctx, "csm-open-url-ssh", move || {
        let configured = ssh.config(&conn.config_args()).map(|text| ssh_ports::parse_local_forwards(&text)).unwrap_or_default();
        match ssh_ports::forwarded_local_port(&configured, port) {
            Some(local) => {
                let link = ssh_ports::rewrite_to_local_port(&url, local).ok_or_else(|| ToolError::Refused("not an http or https link".into()))?;
                opener(link.clone());
                Ok(ToolOutput::new(
                    format!("Opening {link}: port {port} on {destination} is forwarded here."),
                    json!({"url": link, "forwarded": true, "local_port": local, "remote_port": port}),
                ))
            }
            None => Ok(ToolOutput::new(
                format!("Port {port} is on {destination}, not on this computer."),
                json!({"url": url, "needs_forward": true, "remote_port": port, "session_id": session_id, "destination": destination}),
            )),
        }
    })
}

/// Opens `url` through the forward at `local`, synchronously (no process is started).
fn open_at(ctx: &mut Ctx<'_>, url: &str, port: u16, local: u16, destination: &str) -> Reply {
    let link = ssh_ports::rewrite_to_local_port(url, local).ok_or_else(|| ToolError::Refused("not an http or https link".into()))?;
    (ctx.state.options.url_opener)(link.clone());
    Ok(ToolOutput::new(
        format!("Opening {link}: port {port} on {destination} is forwarded here."),
        json!({"url": link, "forwarded": true, "local_port": local, "remote_port": port}),
    ))
}

/// The local port of a live forward of `session_id` to `remote`.
pub(crate) fn forward_for(forwards: &[Forward], session_id: &str, remote: u16) -> Option<u16> {
    forwards.iter().find(|f| f.session_id == session_id && f.remote_port == remote && f.process.alive()).map(|f| f.local_port)
}

/// Starts a forward for a session, on a worker, and answers the parked call when it is ready
/// (`open`: opens its address then). An existing forward for the same port is reused at once.
pub(crate) fn start_for_session(ctx: &mut Ctx<'_>, session: &ShellSession, remote: u16, requested: Option<u16>, open: bool) -> Reply {
    let conn = connection_for(session).map_err(ToolError::Refused)?;
    if let Some(local) = forward_for(ctx.state.rt.forwards.as_slice(), &session.id, remote) {
        let url = format!("http://127.0.0.1:{local}/");
        if open {
            (ctx.state.options.url_opener)(url.clone());
        }
        return Ok(ToolOutput::new(
            format!("Already forwarded: {url} is port {remote} on {}.", conn.destination),
            json!({"url": url, "local_port": local, "remote_port": remote, "reused": true}),
        ));
    }
    let ssh = ctx.state.options.ssh.clone();
    let engine = ctx.engine.clone();
    let session_id = session.id.clone();
    let parked = ctx.park();
    let spawned = std::thread::Builder::new().name("csm-port-forward-start".into()).spawn(move || {
        match start_forward(ssh.as_ref(), &conn, remote, requested, FORWARD_TIMEOUT) {
            Ok((local, process)) => {
                let destination = conn.destination;
                engine.post(Box::new(move |ctx: &mut Ctx<'_>| register(ctx, session_id, destination, remote, local, process, open, parked)));
            }
            Err(e) => {
                if let Some(parked) = parked {
                    parked.send(Err(e));
                }
            }
        }
    });
    match spawned {
        Ok(_) => Ok(ToolOutput::message("Starting the port forward.")),
        Err(e) => Err(ToolError::Failed(format!("could not start a worker: {e}"))),
    }
}

/// On the engine thread: keeps a ready forward (or drops it when its session has gone or a
/// concurrent start forwarded the same port already), then answers the call.
#[allow(clippy::too_many_arguments)]
fn register(
    ctx: &mut Ctx<'_>,
    session_id: String,
    destination: String,
    remote: u16,
    local: u16,
    process: Box<dyn ForwardProcess>,
    open: bool,
    parked: Option<ParkedReply>,
) {
    let reply: Reply = if crate::lifecycle::session_index(ctx.state, &session_id).is_none() {
        process.stop();
        Err(ToolError::NotFound(format!("no session `{session_id}`; the forward was not kept")))
    } else if let Some(existing) = forward_for(ctx.state.rt.forwards.as_slice(), &session_id, remote) {
        process.stop();
        let url = format!("http://127.0.0.1:{existing}/");
        if open {
            (ctx.state.options.url_opener)(url.clone());
        }
        Ok(ToolOutput::new(
            format!("Already forwarded: {url} is port {remote} on {destination}."),
            json!({"url": url, "local_port": existing, "remote_port": remote, "reused": true}),
        ))
    } else {
        let url = format!("http://127.0.0.1:{local}/");
        ctx.state.rt.forwards.push(Forward { session_id, destination: destination.clone(), remote_port: remote, local_port: local, process });
        ctx.state.touch();
        if open {
            (ctx.state.options.url_opener)(url.clone());
        }
        Ok(ToolOutput::new(
            format!("Forwarding {url} to port {remote} on {destination}."),
            json!({"url": url, "local_port": local, "remote_port": remote, "reused": false}),
        ))
    };
    if let Some(parked) = parked {
        parked.send(reply);
    }
}

/// Stops the forwards of a session that is being torn down (close, sleep, restart).
pub(crate) fn teardown_session(ctx: &mut Ctx<'_>, session_id: &str) {
    if ctx.state.rt.forwards.iter().any(|f| f.session_id == session_id) {
        stop_session(&mut ctx.state.rt.forwards, session_id);
        ctx.state.touch();
    }
}

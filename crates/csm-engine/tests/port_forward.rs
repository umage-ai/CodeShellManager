//! SSH port forwards: `session_port_forward` / `session_port_forwards` / `session_port_unforward`,
//! and `open_url` for a loopback link of an SSH session. ssh is a fake: its `start` binds the
//! forwarded local port (as `ssh -L` would) and `stop` releases it, so readiness and cleanup are
//! real TCP behaviour.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{H, test_app};
use csm_core::models::{SessionKind, ShellSession};
use csm_engine::{ForwardProcess, SshHandle, SshRunner, ToolError};
use serde_json::json;

/// One fake ssh process: alive until stopped, holding its forwarded port while it lives.
struct ProcState {
    alive: AtomicBool,
    listener: Mutex<Option<TcpListener>>,
    pid: u32,
}

struct FakeProc(Arc<ProcState>);

impl ForwardProcess for FakeProc {
    fn alive(&self) -> bool {
        self.0.alive.load(Ordering::SeqCst)
    }

    fn pid(&self) -> u32 {
        self.0.pid
    }

    fn stop(&self) {
        self.0.alive.store(false, Ordering::SeqCst);
        self.0.listener.lock().unwrap().take();
    }
}

#[derive(Default)]
struct FakeSsh {
    /// What `ssh -G` prints; `None` makes it fail.
    config: Mutex<Option<String>>,
    config_calls: AtomicUsize,
    starts: Mutex<Vec<Vec<String>>>,
    procs: Mutex<Vec<Arc<ProcState>>>,
    /// Start fails outright.
    refuse: AtomicBool,
    /// The process ends at once (ssh gave up: no route, refused forward).
    die: AtomicBool,
}

impl FakeSsh {
    fn with_config(text: Option<&str>) -> Arc<FakeSsh> {
        Arc::new(FakeSsh { config: Mutex::new(text.map(str::to_owned)), ..Default::default() })
    }

    fn alive_count(&self) -> usize {
        self.procs.lock().unwrap().iter().filter(|p| p.alive.load(Ordering::SeqCst)).count()
    }
}

/// The local port of `ssh ... -L 127.0.0.1:<local>:127.0.0.1:<remote>`.
fn local_of(args: &[String]) -> Option<u16> {
    let spec = args.iter().position(|a| a == "-L").and_then(|i| args.get(i + 1))?;
    spec.split(':').nth(1)?.parse().ok()
}

impl SshRunner for FakeSsh {
    fn config(&self, _args: &[String]) -> Option<String> {
        self.config_calls.fetch_add(1, Ordering::SeqCst);
        self.config.lock().unwrap().clone()
    }

    fn start(&self, args: &[String]) -> io::Result<Box<dyn ForwardProcess>> {
        self.starts.lock().unwrap().push(args.to_vec());
        if self.refuse.load(Ordering::SeqCst) {
            return Err(io::Error::other("no ssh here"));
        }
        let local = local_of(args).ok_or_else(|| io::Error::other("no -L"))?;
        let listener = TcpListener::bind(("127.0.0.1", local))?;
        let die = self.die.load(Ordering::SeqCst);
        let state = Arc::new(ProcState {
            alive: AtomicBool::new(!die),
            listener: Mutex::new(if die { None } else { Some(listener) }),
            pid: 4000 + u32::try_from(self.procs.lock().unwrap().len()).unwrap_or(0),
        });
        self.procs.lock().unwrap().push(state.clone());
        Ok(Box::new(FakeProc(state)))
    }
}

fn ssh_session(id: &str) -> ShellSession {
    ShellSession {
        id: id.into(),
        name: "agent".into(),
        kind: SessionKind::Ssh,
        ssh_host: "agent-1".into(),
        ssh_config_file: "cfg dir/nexus_ssh_config".into(),
        command: "bash".into(),
        is_dormant: true,
        ..Default::default()
    }
}

fn rig(ssh: &Arc<FakeSsh>, opened: &Arc<Mutex<Vec<String>>>) -> H {
    let mut app = test_app();
    app.sessions.push(ssh_session("r"));
    app.sessions.push(ShellSession { id: "l".into(), name: "local".into(), command: "pwsh".into(), is_dormant: true, ..Default::default() });
    let handle: SshHandle = ssh.clone();
    let seen = opened.clone();
    H::start(app, move |o| {
        o.ssh = handle;
        o.url_opener = Arc::new(move |u| seen.lock().unwrap().push(u));
    })
}

fn refused(r: Result<csm_engine::ToolOutput, ToolError>) -> String {
    match r {
        Err(ToolError::Refused(m) | ToolError::InvalidParams(m) | ToolError::NotFound(m) | ToolError::Failed(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn no_open() -> Arc<Mutex<Vec<String>>> {
    Arc::new(Mutex::new(Vec::new()))
}

/// A local port nothing listens on (bind, read, release).
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

#[test]
fn a_forward_starts_lists_reuses_and_stops() {
    let ssh = FakeSsh::with_config(None);
    let h = rig(&ssh, &no_open());
    let out = h.ok("session_port_forward", json!({"session_id": "r", "remote_port": 3000}));
    let local = out.data["local_port"].as_u64().unwrap() as u16;
    assert_eq!(out.data["url"], json!(format!("http://127.0.0.1:{local}/")));
    assert_eq!(out.data["reused"], json!(false));
    // The argv: the connection (no -t, no remote command), a loopback-only forward, no prompts.
    let argv = ssh.starts.lock().unwrap()[0].clone();
    assert_eq!(&argv[..3], ["-F", "cfg dir/nexus_ssh_config", "agent-1"], "{argv:?}");
    assert!(!argv.iter().any(|a| a == "-t" || a == "bash"), "{argv:?}");
    assert!(argv.windows(2).any(|w| w == ["-L", &format!("127.0.0.1:{local}:127.0.0.1:3000")]), "{argv:?}");
    assert!(argv.windows(2).any(|w| w == ["-o", "BatchMode=yes"]) && argv.contains(&"-N".to_owned()), "{argv:?}");
    // It accepts connections while it runs.
    assert!(TcpStream::connect(("127.0.0.1", local)).is_ok());

    let list = h.ok("session_port_forwards", json!({"session_id": "r"}));
    assert_eq!(list.data["forwards"].as_array().unwrap().len(), 1);
    assert_eq!(list.data["forwards"][0]["remote_port"], json!(3000));
    assert_eq!(list.data["forwards"][0]["local_port"], json!(local));
    assert_eq!(h.engine.view().port_forwards.len(), 1);
    assert!(h.ok("session_port_forwards", json!({"session_id": "l"})).data["forwards"].as_array().unwrap().is_empty());

    // The same remote port again is the same forward, not a second ssh.
    let again = h.ok("session_port_forward", json!({"session_id": "r", "remote_port": 3000}));
    assert_eq!(again.data["reused"], json!(true));
    assert_eq!(ssh.starts.lock().unwrap().len(), 1);

    let gone = h.ok("session_port_unforward", json!({"session_id": "r", "local_port": local}));
    assert_eq!(gone.data["removed"], json!(1));
    assert_eq!(ssh.alive_count(), 0);
    assert!(TcpStream::connect(("127.0.0.1", local)).is_err(), "the local port is released");
    assert!(h.engine.view().port_forwards.is_empty());
    h.engine.shutdown();
}

#[test]
fn a_port_in_use_locally_falls_back_to_a_free_one_and_an_explicit_one_is_refused() {
    let ssh = FakeSsh::with_config(None);
    let h = rig(&ssh, &no_open());
    // Something here already holds 3000's twin: the default local port must be another one.
    let busy = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let busy_port = busy.local_addr().unwrap().port();
    let out = h.ok("session_port_forward", json!({"session_id": "r", "remote_port": busy_port}));
    assert_ne!(out.data["local_port"], json!(busy_port));
    let explicit = refused(h.ui("session_port_forward", json!({"session_id": "r", "remote_port": 3001, "local_port": busy_port})));
    assert!(explicit.contains("in use"), "{explicit}");
    drop(busy);
    h.engine.shutdown();
}

#[test]
fn refusals_for_bad_ports_sessions_and_unforward_arguments() {
    let ssh = FakeSsh::with_config(None);
    let h = rig(&ssh, &no_open());
    for bad in [json!(0), json!(65536), json!(-1)] {
        let r = h.ui("session_port_forward", json!({"session_id": "r", "remote_port": bad}));
        assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{bad}: {r:?}");
    }
    let r = h.ui("session_port_forward", json!({"session_id": "r", "remote_port": 3000, "local_port": 0}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{r:?}");
    let local = refused(h.ui("session_port_forward", json!({"session_id": "l", "remote_port": 3000})));
    assert!(local.contains("SSH sessions"), "{local}");
    assert!(matches!(h.ui("session_port_forward", json!({"session_id": "nope", "remote_port": 3000})), Err(ToolError::NotFound(_))));
    let both = h.ui("session_port_unforward", json!({"session_id": "r", "local_port": 3000, "all": true}));
    assert!(matches!(both, Err(ToolError::InvalidParams(_))), "{both:?}");
    let neither = h.ui("session_port_unforward", json!({"session_id": "r"}));
    assert!(matches!(neither, Err(ToolError::InvalidParams(_))), "{neither:?}");
    let unknown = h.ui("session_port_unforward", json!({"session_id": "r", "local_port": free_port()}));
    assert!(matches!(unknown, Err(ToolError::NotFound(_))), "{unknown:?}");
    assert!(ssh.starts.lock().unwrap().is_empty(), "no ssh was started for a refusal");
    // Declared honestly: a process and the network, so MCP callers get a consent card.
    let spec = h.engine.registry().get("session_port_forward").copied().unwrap();
    assert!(spec.effects.contains(csm_engine::Effects::LAUNCHES_PROCESS));
    assert!(spec.effects.contains(csm_engine::Effects::ACCESSES_NETWORK));
    h.engine.shutdown();
}

#[test]
fn a_forward_that_never_comes_up_fails_and_leaves_nothing_running() {
    let ssh = FakeSsh::with_config(None);
    ssh.die.store(true, Ordering::SeqCst);
    let h = rig(&ssh, &no_open());
    let r = refused(h.ui("session_port_forward", json!({"session_id": "r", "remote_port": 3050})));
    assert!(r.contains("not forwarded"), "{r}");
    assert_eq!(ssh.alive_count(), 0);
    ssh.die.store(false, Ordering::SeqCst);
    ssh.refuse.store(true, Ordering::SeqCst);
    let r = refused(h.ui("session_port_forward", json!({"session_id": "r", "remote_port": 3050})));
    assert!(r.contains("could not start ssh"), "{r}");
    h.engine.shutdown();
}

#[test]
fn closing_the_session_stops_its_forwards() {
    let ssh = FakeSsh::with_config(None);
    let h = rig(&ssh, &no_open());
    let out = h.ok("session_port_forward", json!({"session_id": "r", "remote_port": 3020}));
    let local = out.data["local_port"].as_u64().unwrap() as u16;
    h.ok("session_close", json!({"session_id": "r", "force": true}));
    assert_eq!(ssh.alive_count(), 0, "the ssh process is gone");
    assert!(TcpStream::connect(("127.0.0.1", local)).is_err());
    assert!(h.engine.view().port_forwards.is_empty());
    h.engine.shutdown();
}

#[test]
fn quitting_the_app_stops_every_forward() {
    let ssh = FakeSsh::with_config(None);
    let h = rig(&ssh, &no_open());
    h.ok("session_port_forward", json!({"session_id": "r", "remote_port": 3030}));
    h.ok("session_port_forward", json!({"session_id": "r", "remote_port": 8030}));
    assert_eq!(ssh.alive_count(), 2);
    h.engine.shutdown();
    assert_eq!(ssh.alive_count(), 0, "quit kills what is left");
}

#[test]
fn unforward_all_stops_only_that_sessions_forwards() {
    let ssh = FakeSsh::with_config(None);
    let h = rig(&ssh, &no_open());
    h.ok("session_port_forward", json!({"session_id": "r", "remote_port": 3040}));
    h.ok("session_port_forward", json!({"session_id": "r", "remote_port": 8040}));
    let out = h.ok("session_port_unforward", json!({"session_id": "r", "all": true}));
    assert_eq!(out.data["removed"], json!(2));
    assert_eq!(ssh.alive_count(), 0);
    h.engine.shutdown();
}

#[test]
fn a_loopback_link_of_an_ssh_session_opens_when_the_port_is_forwarded_in_the_config() {
    let ssh = FakeSsh::with_config(Some("host agent-1\nlocalforward 3000 localhost:3000\nlocalforward 8080 127.0.0.1:80\n"));
    let opened = no_open();
    let h = rig(&ssh, &opened);
    let out = h.ok("open_url", json!({"url": "http://localhost:3000/app?x=1", "session_id": "r"}));
    assert_eq!(out.data["forwarded"], json!(true));
    assert_eq!(*opened.lock().unwrap(), vec!["http://localhost:3000/app?x=1".to_owned()]);
    // ssh -G got the connection only: no -t, no remote command.
    assert!(ssh.config_calls.load(Ordering::SeqCst) >= 1);
    // A forward to the remote's port 80 is found through 127.0.0.1 and opens at its local port.
    let out = h.ok("open_url", json!({"url": "http://127.0.0.1/", "session_id": "r"}));
    assert_eq!(out.data["local_port"], json!(8080));
    assert_eq!(opened.lock().unwrap().last().map(String::as_str), Some("http://127.0.0.1:8080/"));
    assert!(ssh.starts.lock().unwrap().is_empty(), "nothing is started for a configured forward");
    h.engine.shutdown();
}

#[test]
fn a_loopback_link_to_an_unforwarded_port_is_not_opened_and_says_where_the_port_is() {
    let ssh = FakeSsh::with_config(Some("localforward 9000 localhost:9000\n"));
    let opened = no_open();
    let h = rig(&ssh, &opened);
    let out = h.ok("open_url", json!({"url": "http://127.0.0.1:3000", "session_id": "r"}));
    assert_eq!(out.data["needs_forward"], json!(true));
    assert_eq!(out.data["remote_port"], json!(3000));
    assert_eq!(out.data["destination"], json!("agent-1"));
    assert!(out.message.contains("Port 3000 is on agent-1, not on this computer"), "{}", out.message);
    assert!(opened.lock().unwrap().is_empty(), "nothing opened");
    // A failing `ssh -G` is the same answer: not forwarded.
    let ssh = FakeSsh::with_config(None);
    let h2 = rig(&ssh, &no_open());
    let out = h2.ok("open_url", json!({"url": "http://localhost:3000", "session_id": "r"}));
    assert_eq!(out.data["needs_forward"], json!(true));
    h.engine.shutdown();
    h2.engine.shutdown();
}

#[test]
fn a_started_forward_serves_the_loopback_link_without_asking_ssh_again() {
    let ssh = FakeSsh::with_config(None);
    let opened = no_open();
    let h = rig(&ssh, &opened);
    let fwd = h.ok("session_port_forward", json!({"session_id": "r", "remote_port": 5173}));
    let local = fwd.data["local_port"].as_u64().unwrap();
    let out = h.ok("open_url", json!({"url": "http://localhost:5173/", "session_id": "r"}));
    assert_eq!(out.data["local_port"], json!(local));
    assert_eq!(opened.lock().unwrap().last().cloned(), Some(format!("http://localhost:{local}/")));
    assert_eq!(ssh.config_calls.load(Ordering::SeqCst), 0);
    h.engine.shutdown();
}

#[test]
fn links_that_do_not_point_at_the_loopback_open_as_before() {
    let ssh = FakeSsh::with_config(Some(""));
    let opened = no_open();
    let h = rig(&ssh, &opened);
    // Not loopback: the link is about the machine it is on, so it opens here.
    h.ok("open_url", json!({"url": "https://example.com/a", "session_id": "r"}));
    // A local session's loopback link opens here too.
    h.ok("open_url", json!({"url": "http://localhost:3000", "session_id": "l"}));
    // No session: as before.
    h.ok("open_url", json!({"url": "http://localhost:3001"}));
    assert_eq!(opened.lock().unwrap().len(), 3);
    assert_eq!(ssh.config_calls.load(Ordering::SeqCst), 0, "ssh is asked only for SSH loopback links");
    assert!(matches!(h.ui("open_url", json!({"url": "http://localhost:3000", "session_id": "zz"})), Err(ToolError::NotFound(_))));
    h.engine.shutdown();
}

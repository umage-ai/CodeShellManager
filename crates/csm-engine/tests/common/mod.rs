//! Test harness: an engine on a fake PTY, driven only through `Engine::call`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_core::models::AppState;
use csm_engine::{Caller, Engine, Snapshot, State, StateOptions, ToolError, ToolOutput};
use csm_pty::{Backend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use serde_json::{Value, json};

pub struct FakePty {
    pub argv: Vec<String>,
    /// The environment changes the spawn asked for (`None` removes).
    pub env: Vec<(String, Option<String>)>,
    pub size: Mutex<PtySize>,
    pub first_size: PtySize,
    sink: Mutex<Option<OutputSink>>,
    backlog: Mutex<Vec<u8>>,
    pub written: Mutex<Vec<u8>>,
    exited: AtomicBool,
    code: Mutex<Option<i32>>,
    callbacks: Mutex<Vec<ExitCallback>>,
    pub closed: AtomicBool,
    pub killed: AtomicBool,
    /// Exit by itself when closed (a Claude that honours the console close).
    pub exit_on_close: AtomicBool,
    pub created: Instant,
    pub closed_at: Mutex<Option<Instant>>,
}

impl FakePty {
    fn new(opts: &SpawnOptions) -> Self {
        Self {
            argv: opts.argv.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
            env: opts.env.iter().map(|(k, v)| (k.to_string_lossy().into_owned(), v.as_ref().map(|v| v.to_string_lossy().into_owned()))).collect(),
            size: Mutex::new(opts.size),
            first_size: opts.size,
            sink: Mutex::new(None),
            backlog: Mutex::new(Vec::new()),
            written: Mutex::new(Vec::new()),
            exited: AtomicBool::new(false),
            code: Mutex::new(None),
            callbacks: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            killed: AtomicBool::new(false),
            exit_on_close: AtomicBool::new(true),
            created: Instant::now(),
            closed_at: Mutex::new(None),
        }
    }

    /// Output from the "program".
    pub fn emit(&self, bytes: &[u8]) {
        let mut sink = self.sink.lock().unwrap();
        match sink.as_mut() {
            Some(s) => s(bytes),
            None => self.backlog.lock().unwrap().extend_from_slice(bytes),
        }
    }

    pub fn exit(&self, code: Option<i32>) {
        if self.exited.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.code.lock().unwrap() = code;
        let cbs = std::mem::take(&mut *self.callbacks.lock().unwrap());
        for cb in cbs {
            cb(code);
        }
    }

    pub fn written(&self) -> Vec<u8> {
        self.written.lock().unwrap().clone()
    }

    pub fn written_text(&self) -> String {
        String::from_utf8_lossy(&self.written()).into_owned()
    }
}

impl Pty for FakePty {
    fn subscribe(&self, mut sink: OutputSink) {
        let backlog = std::mem::take(&mut *self.backlog.lock().unwrap());
        if !backlog.is_empty() {
            sink(&backlog);
        }
        *self.sink.lock().unwrap() = Some(sink);
    }
    fn on_exit(&self, cb: ExitCallback) {
        if self.exited.load(Ordering::SeqCst) {
            cb(*self.code.lock().unwrap());
        } else {
            self.callbacks.lock().unwrap().push(cb);
        }
    }
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.written.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn resize(&self, size: PtySize) -> io::Result<()> {
        *self.size.lock().unwrap() = size;
        Ok(())
    }
    fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
    fn exit_code(&self) -> Option<i32> {
        *self.code.lock().unwrap()
    }
    fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        self.exit(Some(1));
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        *self.closed_at.lock().unwrap() = Some(Instant::now());
        if self.exit_on_close.load(Ordering::SeqCst) {
            self.exit(Some(0));
        }
    }
    fn backend(&self) -> Backend {
        Backend::BundledConPty
    }
    fn pid(&self) -> Option<u32> {
        Some(1)
    }
}

pub struct H {
    pub engine: Engine,
    pub ptys: Arc<Mutex<Vec<Arc<FakePty>>>>,
    pub dir: tempfile::TempDir,
}

pub fn fast_options(ptys: &Arc<Mutex<Vec<Arc<FakePty>>>>) -> StateOptions {
    let list = ptys.clone();
    StateOptions {
        pty: Arc::new(move |opts: SpawnOptions| {
            let p = Arc::new(FakePty::new(&opts));
            list.lock().unwrap().push(p.clone());
            Ok(p as Arc<dyn Pty>)
        }),
        first_size_timeout: Duration::from_millis(30),
        exit_wait: Duration::from_millis(1500),
        shutdown_budget: Duration::from_millis(1500),
        save_debounce: Duration::from_millis(10),
        ..StateOptions::default()
    }
}

/// Settings that keep tests fast (no Claude stagger) and hermetic (no resume lookup).
pub fn test_app() -> AppState {
    let mut app = AppState::default();
    app.settings.claude_launch_stagger_ms = 0;
    app.settings.auto_resume_claude = false;
    app
}

impl H {
    pub fn start(app: AppState, tweak: impl FnOnce(&mut StateOptions)) -> H {
        let ptys = Arc::new(Mutex::new(Vec::new()));
        let mut opts = fast_options(&ptys);
        tweak(&mut opts);
        let engine = Engine::start(State::new(app, opts), vec![]).unwrap();
        H { engine, ptys, dir: tempfile::tempdir().unwrap() }
    }

    pub fn new() -> H {
        Self::start(test_app(), |_| {})
    }

    pub fn ui(&self, id: &str, params: Value) -> Result<ToolOutput, ToolError> {
        self.engine.call(Caller::Ui, id, params)
    }

    pub fn ok(&self, id: &str, params: Value) -> ToolOutput {
        match self.ui(id, params.clone()) {
            Ok(o) => o,
            Err(e) => panic!("{id} {params} failed: {e:?}"),
        }
    }

    pub fn wait_until(&self, what: &str, f: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let v = self.engine.view();
            if f(&v) {
                return v;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}; last snapshot: {}", serde_json::to_string_pretty(&*v).unwrap());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn status(&self, id: &str) -> csm_engine::Status {
        self.engine.view().session(id).map(|s| s.status).unwrap()
    }

    pub fn wait_status(&self, id: &str, st: csm_engine::Status) -> Arc<Snapshot> {
        self.wait_until(&format!("{id} to be {st:?}"), |v| v.session(id).is_some_and(|s| s.status == st))
    }

    pub fn pty(&self, n: usize) -> Arc<FakePty> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(p) = self.ptys.lock().unwrap().get(n) {
                return p.clone();
            }
            if Instant::now() > deadline {
                panic!("no PTY #{n} was created");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn pty_count(&self) -> usize {
        self.ptys.lock().unwrap().len()
    }

    /// A session running `command`, live (its fake PTY started).
    pub fn session(&self, command: &str) -> String {
        let folder = self.dir.path().to_string_lossy().into_owned();
        let out = self.ok("session_new", json!({"folder": folder, "command": command}));
        let id = out.data["id"].as_str().unwrap().to_owned();
        self.wait_status(&id, csm_engine::Status::Idle);
        id
    }

    /// The fake PTY currently backing a session (the newest one created for it).
    pub fn pty_of(&self, id: &str) -> Arc<FakePty> {
        let n = self.pty_count();
        let _ = id;
        self.pty(n - 1)
    }
}

impl Drop for H {
    fn drop(&mut self) {
        self.engine.shutdown();
    }
}

/// OSC 7501 report, BEL-terminated.
pub fn osc7501(payload: &str) -> Vec<u8> {
    format!("\x1b]7501;{payload}\x07").into_bytes()
}

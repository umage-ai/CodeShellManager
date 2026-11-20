//! Headless gate check: runs a program on csm-pty + csm-term exactly as a pane does (same
//! ConPTY selection, same in-order reply path, 7501 probe answered on the bundled ConPTY), waits,
//! sends Ctrl+C twice, and prints what the terminal saw: probe/answer, OSC 7501 reports, and the
//! final screen. Used to verify `claude --debug-file X` logs `programStatus=yes`.
//!
//!   probe-check [--secs N] [--cols C] [--rows R] [--cwd DIR] [--inbox] -- PROGRAM ARGS...

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use csm_pty::{Backend, Pty, PtySize, SpawnOptions};
use csm_term::alacritty_terminal::grid::Dimensions;
use csm_term::alacritty_terminal::index::{Column, Line};
use csm_term::{GridSize, ProbeReply, ReplySink, TapEvent, TermConfig, TermEvent, Terminal};

struct Replies(Arc<OnceLock<Arc<dyn Pty>>>);
impl ReplySink for Replies {
    fn send(&self, bytes: &[u8]) {
        if let Some(p) = self.0.get() {
            let _ = p.write(bytes);
        }
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (mut secs, mut cols, mut rows) = (20u64, 120u16, 40u16);
    let mut cwd = None;
    let mut argv: Vec<OsString> = Vec::new();
    while let Some(a) = args.next() {
        let s = a.to_string_lossy().into_owned();
        let mut val = || args.next().map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
        match s.as_str() {
            "--secs" => secs = val().parse().unwrap_or(secs),
            "--cols" => cols = val().parse().unwrap_or(cols),
            "--rows" => rows = val().parse().unwrap_or(rows),
            "--cwd" => cwd = Some(PathBuf::from(val())),
            "--inbox" => csm_pty::set_prefer_bundled(false),
            "--" => {
                argv.extend(args.by_ref());
                break;
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    if argv.is_empty() {
        eprintln!("usage: probe-check [--secs N] [--cwd DIR] [--inbox] -- PROGRAM ARGS...");
        std::process::exit(2);
    }
    if let Some((b, why)) = csm_pty::conpty_selection() {
        println!("ConPTY: {} — {why}", b.label());
    }
    let cell: Arc<OnceLock<Arc<dyn Pty>>> = Arc::new(OnceLock::new());
    let term = Arc::new(Mutex::new(Terminal::new(GridSize::new(cols, rows), TermConfig::default(), Arc::new(Replies(cell.clone())))));
    let mut opts = SpawnOptions::new(argv, PtySize::new(cols, rows));
    opts.cwd = cwd;
    opts.env.push(("TERM".into(), Some("xterm-256color".into())));
    opts.env.push(("COLORTERM".into(), Some("truecolor".into())));
    let pty = match csm_pty::spawn(opts) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("spawn failed: {e}");
            std::process::exit(1);
        }
    };
    let backend = pty.backend();
    println!("PTY backend: {}", backend.label());
    let probe = if backend == Backend::InboxConPty { ProbeReply::Off } else { ProbeReply::Raw };
    term.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_probe_reply(probe);
    let _ = cell.set(pty.clone());
    let seen: Arc<Mutex<Vec<(Duration, TermEvent)>>> = Arc::new(Mutex::new(Vec::new()));
    let t0 = Instant::now();
    {
        let term = term.clone();
        let seen = seen.clone();
        let trace = std::env::var_os("CSM_TRACE_BYTES").is_some();
        pty.subscribe(Box::new(move |bytes| {
            if trace {
                println!("{:>7.3}s  {} bytes: {}", t0.elapsed().as_secs_f32(), bytes.len(), bytes.escape_ascii());
            }
            let mut t = term.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            t.advance(bytes);
            let evs = t.take_events();
            if !evs.is_empty() {
                let mut s = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                s.extend(evs.into_iter().map(|e| (t0.elapsed(), e)));
            }
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline && !pty.has_exited() {
        std::thread::sleep(Duration::from_millis(100));
        let mut t = term.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        t.flush_sync_if_due(Instant::now());
    }
    let screen = dump(&term);
    // Leave politely: Ctrl+C twice (Claude: "press Ctrl+C again to exit").
    for _ in 0..2 {
        let _ = pty.write(&[0x03]);
        std::thread::sleep(Duration::from_millis(700));
    }
    let exit_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < exit_deadline && !pty.has_exited() {
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("exited by itself after Ctrl+C: {} (code {:?})", pty.has_exited(), pty.exit_code());
    pty.kill();

    println!("--- terminal events ---");
    for (at, e) in seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter() {
        let text = match e {
            TermEvent::Tap(TapEvent::ProgramStatus(p)) => format!("OSC 7501 report: {p}"),
            TermEvent::Tap(TapEvent::ProgramStatusProbe) => "OSC 7501 probe (?)".into(),
            TermEvent::ProbeAnswered { answered } => format!("probe answered: {answered}"),
            other => format!("{other:?}"),
        };
        println!("{:>7.3}s  {text}", at.as_secs_f32());
    }
    println!("--- screen at {secs}s ---\n{screen}");
}

fn dump(term: &Mutex<Terminal>) -> String {
    let t = term.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let grid = t.term().grid();
    let mut out = String::new();
    for row in 0..grid.screen_lines() {
        let line = &grid[Line(row as i32)];
        let mut s: String = (0..grid.columns()).map(|c| line[Column(c)].c).collect();
        s.truncate(s.trim_end().len());
        out.push_str(&s);
        out.push('\n');
    }
    out
}

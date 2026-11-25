//! The Git tab and the branch chip through the real UI (egui_kittest + AccessKit): on the demo
//! backend's fixture, and on a real engine whose session sits in a real git repository.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use csm_engine::{Caller, Engine, State, StateOptions};
use csm_pty::{Backend as PtyBackend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_ui::model::InspectorTab;
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::theme::ThemeKind;
use csm_ui::{CsmApp, EngineBackend, EngineOptions};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

#[test]
fn the_git_tab_shows_the_fixture_and_fetch_submits_git_fetch() {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    // The pane header's chip opens the Git tab for its session.
    h.get_all_by_label("Git: feat/brand-facet").next().unwrap().click();
    settle(&mut h);
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Git));
    assert!(h.query_by_label_contains("Git status: feat/brand-facet → origin/feat/brand-facet").is_some(), "the header line");
    assert!(h.query_by_label("Changed: M src/search/FacetPanel.tsx").is_some());
    assert!(h.query_by_label("Changed: A src/search/api/brand.ts").is_some());
    assert!(h.query_by_label("Worktree: search-sort").is_some());
    // A fixture is already there: the tab doesn't read git again.
    assert!(!h.state().submitted().iter().any(|c| c.id == "git_status"));

    h.get_by_label("Fetch").click();
    settle(&mut h);
    let c = h.state().submitted().last().cloned().unwrap();
    assert_eq!((c.id.as_str(), c.params), ("git_fetch", json!({"session_id": "web"})));

    // Switch branch ▸ loads the list when it opens.
    h.get_by_label("Switch branch ▸").click();
    settle(&mut h);
    assert!(h.state().submitted().iter().any(|c| c.id == "git_branches"));

    // A worktree row focuses its session.
    h.get_by_label("Worktree: search-sort").click();
    settle(&mut h);
    assert!(h.state().submitted().iter().any(|c| c.id == "session_focus" && c.params == json!({"session_id": "wt"})));
}

// ── On the real engine ─────────────────────────────────────────────────────────────────────

struct FakePty {
    exited: AtomicBool,
    callbacks: Mutex<Vec<ExitCallback>>,
}

impl Pty for FakePty {
    fn subscribe(&self, _: OutputSink) {}
    fn on_exit(&self, cb: ExitCallback) {
        self.callbacks.lock().unwrap().push(cb);
    }
    fn write(&self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn resize(&self, _: PtySize) -> io::Result<()> {
        Ok(())
    }
    fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
    fn exit_code(&self) -> Option<i32> {
        None
    }
    fn kill(&self) {
        if !self.exited.swap(true, Ordering::SeqCst) {
            for cb in std::mem::take(&mut *self.callbacks.lock().unwrap()) {
                cb(Some(1));
            }
        }
    }
    fn close(&self) {
        self.kill();
    }
    fn backend(&self) -> PtyBackend {
        PtyBackend::BundledConPty
    }
    fn pid(&self) -> Option<u32> {
        Some(1)
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=csm", "-c", "user.email=csm@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn until(h: &mut Harness<'static, CsmApp>, what: &str, f: impl Fn(&Harness<'static, CsmApp>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !f(h) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.step();
}

#[test]
fn on_the_real_engine_the_chip_and_tab_follow_the_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("proj");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "one"]);
    std::fs::write(repo.join("b.txt"), "new\n").unwrap();

    let options = StateOptions {
        pty: Arc::new(|_: SpawnOptions| Ok(Arc::new(FakePty { exited: AtomicBool::new(false), callbacks: Mutex::new(Vec::new()) }) as Arc<dyn Pty>)),
        first_size_timeout: Duration::from_millis(50),
        restore_on_start: false,
        exit_wait: Duration::from_millis(300),
        ..StateOptions::default()
    };
    let engine: Engine = Engine::start(State::new(csm_core::models::AppState::default(), options), csm_ui::app_tools()).unwrap();
    let id = engine.call(Caller::Ui, "session_new", json!({"folder": repo.display().to_string(), "command": "pwsh"})).unwrap().data["id"]
        .as_str()
        .unwrap()
        .to_owned();
    engine.call(Caller::Ui, "inspector_show", json!({"tab": "git"})).unwrap();
    let backend = EngineBackend::new(engine.clone(), EngineOptions::default());
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let mut app = CsmApp::new(Box::new(csm_ui::EnginePort::new(engine.clone())), Box::new(backend), TerminalRenderer::new(fonts));
    app.ui.no_dialog_delay = true;
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app);

    // The poll fills the snapshot: the chip in the pane header, the summary in the tab.
    until(&mut h, "the branch chip", |h| h.query_by_label("Git: main").is_some());
    until(&mut h, "the summary", |h| h.query_by_label_contains("Git status: main · no upstream · 1 changed").is_some());
    // The tab read git_status and worktree_list itself: the changed file and the one worktree.
    until(&mut h, "the changed file", |h| h.query_by_label("Changed: ?? b.txt").is_some());
    until(&mut h, "the worktree list", |h| h.query_by_label("Worktree: proj").is_some());

    // A failed operation opens the output panel with git's words (no remote to fetch from is
    // not a failure, so pull without an upstream it is).
    h.get_by_label("Pull").click();
    until(&mut h, "the failure panel", |h| h.query_by_label_contains("Git failure: Pull (fast-forward only) failed.").is_some());
    let _ = id;
    engine.shutdown();
}

// ── Open on GitHub ─────────────────────────────────────────────────────────────────────────

#[test]
fn open_on_github_opens_the_branch_and_the_pull_requests_through_open_url() {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h.get_all_by_label("Git: feat/brand-facet").next().unwrap().click();
    settle(&mut h);
    h.get_by_label("Open on GitHub").click();
    settle(&mut h);
    let c = h.state().submitted().last().cloned().unwrap();
    assert_eq!((c.id.as_str(), c.params), ("open_url", json!({"url": "https://github.com/odulodo/odulodo-web/tree/feat/brand-facet"})));

    // ▾: the same page, and the pull requests.
    h.get_by_label("More GitHub links").click();
    settle(&mut h);
    assert!(h.query_by_label("Open branch on GitHub").is_some());
    h.get_by_label("Open pull requests").click();
    settle(&mut h);
    let c = h.state().submitted().last().cloned().unwrap();
    assert_eq!((c.id.as_str(), c.params), ("open_url", json!({"url": "https://github.com/odulodo/odulodo-web/pulls"})));
}

#[test]
fn open_on_github_is_hidden_without_a_github_remote() {
    let mut b = backend(ThemeKind::Mocha);
    if let Some(d) = b.model_mut().git.get_mut("web") {
        d.github = None;
    }
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    h.get_all_by_label("Git: feat/brand-facet").next().unwrap().click();
    settle(&mut h);
    assert!(h.query_by_label("Fetch").is_some(), "the tab is showing");
    assert!(h.query_by_label("Open on GitHub").is_none());
    assert!(h.query_by_label("More GitHub links").is_none());
}

/// A real engine on `repo` with its Git tab showing; the URLs it opens land in the vector.
fn real_git_tab(repo: &Path) -> (Harness<'static, CsmApp>, Engine, Arc<Mutex<Vec<String>>>) {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let sink = opened.clone();
    let options = StateOptions {
        pty: Arc::new(|_: SpawnOptions| Ok(Arc::new(FakePty { exited: AtomicBool::new(false), callbacks: Mutex::new(Vec::new()) }) as Arc<dyn Pty>)),
        first_size_timeout: Duration::from_millis(50),
        restore_on_start: false,
        exit_wait: Duration::from_millis(300),
        url_opener: Arc::new(move |u| sink.lock().unwrap().push(u)),
        ..StateOptions::default()
    };
    let engine: Engine = Engine::start(State::new(csm_core::models::AppState::default(), options), csm_ui::app_tools()).unwrap();
    engine.call(Caller::Ui, "session_new", json!({"folder": repo.display().to_string(), "command": "pwsh"})).unwrap();
    engine.call(Caller::Ui, "inspector_show", json!({"tab": "git"})).unwrap();
    let backend = EngineBackend::new(engine.clone(), EngineOptions::default());
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let mut app = CsmApp::new(Box::new(csm_ui::EnginePort::new(engine.clone())), Box::new(backend), TerminalRenderer::new(fonts));
    app.ui.no_dialog_delay = true;
    let h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app);
    (h, engine, opened)
}

fn repo_with_remote(tmp: &Path, branch: &str, remote: &str) -> std::path::PathBuf {
    let repo = tmp.join("proj");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-b", branch]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "one"]);
    git(&repo, &["remote", "add", "origin", remote]);
    repo
}

#[test]
fn on_the_real_engine_a_github_origin_opens_its_branch_page() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_remote(tmp.path(), "feat/brand#1", "git@github.com:umage-ai/csm-test.git");
    let (mut h, engine, opened) = real_git_tab(&repo);
    until(&mut h, "the GitHub link", |h| h.query_by_label("Open on GitHub").is_some());
    h.get_by_label("Open on GitHub").click();
    until(&mut h, "the page opened", |_| !opened.lock().unwrap().is_empty());
    assert_eq!(opened.lock().unwrap().clone(), vec!["https://github.com/umage-ai/csm-test/tree/feat/brand%231".to_string()]);
    engine.shutdown();
}

#[test]
fn on_the_real_engine_a_non_github_origin_shows_no_link() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_remote(tmp.path(), "main", "https://gitlab.com/umage-ai/csm-test.git");
    let (mut h, engine, _) = real_git_tab(&repo);
    // The git_status reply has landed (its summary is in the tab), and it found no GitHub remote.
    until(&mut h, "the git_status reply", |h| h.state().model.git.values().any(|d| d.summary.is_some()));
    assert!(h.query_by_label("Fetch").is_some());
    assert!(h.query_by_label("Open on GitHub").is_none());
    engine.shutdown();
}

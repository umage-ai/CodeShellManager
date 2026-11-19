//! The git tools end to end through `Engine::call`, against real repositories: a throwaway
//! bare "remote" and clones of it. Sessions run on the fake PTY; git is real (on PATH).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::H;
use csm_engine::{Caller, Snapshot, ToolError};
use serde_json::{Value, json};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=csm", "-c", "user.email=csm@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

struct Repos {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    remote: PathBuf,
    a: PathBuf,
}

/// The real form of a path (the temp dir may have an 8.3 spelling; git prints long paths).
fn real(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap().display().to_string();
    PathBuf::from(c.strip_prefix(r"\\?\").unwrap_or(&c))
}

fn repos() -> Repos {
    let tmp = tempfile::tempdir().unwrap();
    let root = real(tmp.path());
    let remote = root.join("remote.git");
    std::fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "--bare", "-b", "main"]);
    let seed = root.join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    std::fs::write(seed.join("a.txt"), "one\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "one"]);
    git(&seed, &["remote", "add", "origin", &s(&remote)]);
    git(&seed, &["push", "-u", "origin", "main"]);
    git(&root, &["clone", &s(&remote), "a"]);
    let a = root.join("a");
    Repos { _tmp: tmp, root, remote, a }
}

fn push_from_other(r: &Repos, file: &str) {
    let b = r.root.join("b");
    if !b.exists() {
        git(&r.root, &["clone", &s(&r.remote), "b"]);
    }
    git(&b, &["pull", "--ff-only"]);
    std::fs::write(b.join(file), "theirs\n").unwrap();
    git(&b, &["add", "."]);
    git(&b, &["commit", "-m", file]);
    git(&b, &["push"]);
}

/// A live session (fake PTY) in `folder`.
fn session_in(h: &H, folder: &Path, command: &str) -> String {
    let out = h.ok("session_new", json!({"folder": s(folder), "command": command}));
    out.data["id"].as_str().unwrap().to_owned()
}

fn view_of(h: &H, id: &str, what: &str, f: impl Fn(&csm_engine::SessionView) -> bool) -> std::sync::Arc<Snapshot> {
    h.wait_until(what, |v| v.session(id).is_some_and(&f))
}

fn err_text(r: Result<csm_engine::ToolOutput, ToolError>) -> String {
    match r {
        Ok(o) => panic!("expected an error, got {:?}", o.message),
        Err(e) => e.to_string(),
    }
}

#[test]
fn the_poll_fills_the_snapshot_and_fetch_and_pull_move_it() {
    let r = repos();
    let h = H::new();
    let id = session_in(&h, &r.a, "pwsh");
    let v = view_of(&h, &id, "the first git status", |s| s.branch.as_deref() == Some("main") && s.git_upstream.is_some());
    let sv = v.session(&id).unwrap();
    assert_eq!(sv.git_upstream.as_deref(), Some("origin/main"));
    assert_eq!((sv.git_ahead, sv.git_behind, sv.git_changes), (Some(0), Some(0), 0));
    assert_eq!(sv.git_summary.as_deref(), Some("main → origin/main · up to date · clean"));
    assert!(sv.repo_root.is_some() && sv.worktree_of.is_none());

    // git_status: the same, read now, with the changed files.
    std::fs::write(r.a.join("new.txt"), "x\n").unwrap();
    let st = h.ok("git_status", json!({"session_id": id}));
    assert_eq!(st.data["changes"][0]["path"], "new.txt");
    assert_eq!(st.data["change_count"], 1);
    assert!(st.message.contains("1 changed"), "{}", st.message);
    view_of(&h, &id, "the stored status", |s| s.git_dirty && s.git_changes == 1);
    std::fs::remove_file(r.a.join("new.txt")).unwrap();

    // Fetch: the toast sentence comes from the re-read status, and the snapshot follows.
    push_from_other(&r, "b1.txt");
    let f = h.ok("git_fetch", json!({"session_id": id}));
    assert_eq!(f.message, "1 new commit on origin/main — Pull to fast-forward.");
    view_of(&h, &id, "behind by one", |s| s.git_behind == Some(1));

    let p = h.ok("git_pull", json!({"session_id": id}));
    assert!(p.message.starts_with("Fast-forwarded"), "{}", p.message);
    view_of(&h, &id, "level again", |s| s.git_behind == Some(0) && !s.git_busy);
    assert!(r.a.join("b1.txt").exists());

    // Diverged: refused with git's reason, nothing merged.
    std::fs::write(r.a.join("mine.txt"), "mine\n").unwrap();
    git(&r.a, &["add", "."]);
    git(&r.a, &["commit", "-m", "mine"]);
    push_from_other(&r, "b2.txt");
    h.ok("git_fetch", json!({"session_id": id}));
    let e = err_text(h.ui("git_pull", json!({"session_id": id})));
    assert!(e.starts_with("Pull (fast-forward only) failed."), "{e}");
    assert!(!r.a.join("b2.txt").exists());
    view_of(&h, &id, "diverged", |s| s.git_ahead == Some(1) && s.git_behind == Some(1));
}

#[test]
fn a_checkout_is_seen_by_the_watcher_before_the_next_poll() {
    let r = repos();
    git(&r.a, &["branch", "other"]);
    let h = H::new();
    let id = session_in(&h, &r.a, "pwsh");
    view_of(&h, &id, "main", |s| s.branch.as_deref() == Some("main"));
    // Let the probe that located the repo (and set the watch) land.
    std::thread::sleep(Duration::from_millis(500));
    let t0 = Instant::now();
    git(&r.a, &["switch", "other"]);
    view_of(&h, &id, "the switch", |s| s.branch.as_deref() == Some("other"));
    // The focused poll is every 10 s: seeing it well before that is the watcher.
    assert!(t0.elapsed() < Duration::from_secs(6), "took {:?}", t0.elapsed());
}

#[test]
fn an_osc_9001_branch_wins_over_the_poll() {
    let r = repos();
    let h = H::new();
    let id = session_in(&h, &r.a, "pwsh");
    view_of(&h, &id, "main", |s| s.branch.as_deref() == Some("main"));
    h.pty_of(&id).emit(b"\x1b]9001;git-branch=remote-feature;git-dirty=1\x07");
    let v = view_of(&h, &id, "the pushed branch", |s| s.branch.as_deref() == Some("remote-feature"));
    let sv = v.session(&id).unwrap();
    assert!(sv.git_dirty);
    assert!(sv.git_summary.is_none() && sv.git_upstream.is_none(), "the local folder's state is not mixed in");
}

#[test]
fn switch_handles_dash_names_remote_only_names_and_branches_held_elsewhere() {
    let r = repos();
    git(&r.a, &["update-ref", "refs/heads/-x", "HEAD"]);
    let h = H::new();
    let id = session_in(&h, &r.a, "pwsh");
    view_of(&h, &id, "main", |s| s.branch.as_deref() == Some("main"));

    let o = h.ok("git_switch_branch", json!({"session_id": id, "name": "-x"}));
    assert_eq!(o.message, "Switched to -x.");
    view_of(&h, &id, "-x", |s| s.branch.as_deref() == Some("-x"));
    h.ok("git_switch_branch", json!({"session_id": id, "name": "main"}));

    // Only on the remote: --no-guess refuses to invent a tracking branch.
    let b = r.root.join("b");
    git(&r.root, &["clone", &s(&r.remote), "b"]);
    git(&b, &["switch", "-c", "remote-only"]);
    git(&b, &["push", "-u", "origin", "remote-only"]);
    h.ok("git_fetch", json!({"session_id": id}));
    let e = err_text(h.ui("git_switch_branch", json!({"session_id": id, "name": "remote-only"})));
    assert!(e.contains("failed"), "{e}");
    assert!(!git(&r.a, &["branch", "--list", "remote-only"]).contains("remote-only"));

    // A branch checked out in another worktree: refused, naming it; git_branches maps it.
    let wt = r.root.join("a-held");
    git(&r.a, &["worktree", "add", "-b", "held", "--", &s(&wt)]);
    let wid = session_in(&h, &wt, "pwsh");
    let e = err_text(h.ui("git_switch_branch", json!({"session_id": id, "name": "held"})));
    assert!(e.contains("a-held") && e.contains("one worktree at a time"), "{e}");
    let br = h.ok("git_branches", json!({"session_id": id}));
    let held = br.data["branches"].as_array().unwrap().iter().find(|b| b["name"] == "held").unwrap().clone();
    assert_eq!(held["worktree"]["name"], "a-held");
    assert_eq!(held["worktree"]["session_id"], wid.as_str());
    assert_eq!(br.data["current"], "main");
    let main = br.data["branches"].as_array().unwrap().iter().find(|b| b["name"] == "main").unwrap().clone();
    assert_eq!(main["current"], true);
    assert!(main["worktree"].is_null());

    // SSH sessions have no folder for git.
    let ssh = h.ok("session_new", json!({"kind": "ssh", "ssh": {"host": "example.invalid"}, "command": "bash", "focus": false})).data["id"].as_str().unwrap().to_owned();
    assert!(matches!(h.ui("git_fetch", json!({"session_id": ssh})), Err(ToolError::Refused(_))));
}

#[test]
fn worktree_new_opens_a_sibling_session_and_the_list_maps_sessions() {
    let r = repos();
    let h = H::new();
    let id = session_in(&h, &r.a, "pwsh -NoLogo");
    view_of(&h, &id, "main", |s| s.branch.as_deref() == Some("main"));
    let o = h.ok("worktree_new", json!({"session_id": id, "branch": "feat/x"}));
    let path = PathBuf::from(o.data["path"].as_str().unwrap());
    assert!(path.join("a.txt").exists(), "{}", path.display());
    assert_eq!(path.file_name().unwrap().to_string_lossy(), "a-feat-x", "beside the main checkout");
    assert_eq!(o.data["created_branch"], true);
    let nid = o.data["new_session_id"].as_str().unwrap().to_owned();
    let v = view_of(&h, &nid, "the new session's git", |s| s.branch.as_deref() == Some("feat/x"));
    let ns = v.session(&nid).unwrap();
    assert_eq!(ns.command, "pwsh -NoLogo", "the same command as its source");
    assert_eq!(ns.worktree_of.as_deref(), Some("a"));
    assert!(ns.git_summary.as_deref().unwrap().ends_with("worktree of a"), "{:?}", ns.git_summary);
    let order: Vec<&str> = v.sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(order, vec![id.as_str(), nid.as_str()], "placed right after its source");
    assert_eq!(v.focused.as_deref(), Some(nid.as_str()));
    // Both sessions share one repo root (one busy slot).
    assert_eq!(v.session(&id).unwrap().repo_root.as_deref().map(str::to_lowercase), ns.repo_root.as_deref().map(str::to_lowercase));

    // The same branch again: git refuses (the folder exists / branch checked out).
    assert!(h.ui("worktree_new", json!({"session_id": id, "branch": "feat/x"})).is_err());
    // An existing branch with a base is a params error.
    git(&r.a, &["branch", "existing"]);
    assert!(matches!(h.ui("worktree_new", json!({"session_id": id, "branch": "existing", "base": "main"})), Err(ToolError::InvalidParams(_))));
    // Without a session.
    let o = h.ok("worktree_new", json!({"session_id": id, "branch": "existing", "open_session": false}));
    assert!(o.data.get("new_session_id").is_none());

    let l = h.ok("worktree_list", json!({"session_id": id}));
    let rows = l.data["worktrees"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["main"], true);
    assert_eq!(rows[0]["current"], true);
    let fx = rows.iter().find(|w| w["branch"] == "feat/x").unwrap();
    assert_eq!(fx["sessions"][0]["id"], nid.as_str());
    let ex = rows.iter().find(|w| w["branch"] == "existing").unwrap();
    assert!(ex["sessions"].as_array().unwrap().is_empty());
    // The params to open a session there are a valid session_new call, placed after the source.
    let mut open = ex["open"].clone();
    open["focus"] = json!(false);
    assert_eq!(open["after"], id.as_str());
    let opened = h.ok("session_new", open);
    assert_eq!(opened.data["command"], "pwsh -NoLogo");
}

#[test]
fn worktree_remove_refusals_then_close_wait_remove_and_keep_an_unmerged_branch() {
    let r = repos();
    let h = H::new();
    let id = session_in(&h, &r.a, "pwsh");
    view_of(&h, &id, "main", |s| s.branch.as_deref() == Some("main"));

    // The main checkout.
    let e = err_text(h.ui("worktree_remove", json!({"session_id": id})));
    assert!(e.contains("main checkout"), "{e}");

    let o = h.ok("worktree_new", json!({"session_id": id, "branch": "wt"}));
    let wt = PathBuf::from(o.data["path"].as_str().unwrap());
    let wid = o.data["new_session_id"].as_str().unwrap().to_owned();
    // A second, sleeping session in the same worktree.
    let sleeper = session_in(&h, &wt, "pwsh");
    h.ok("session_sleep", json!({"session_id": sleeper}));

    // Dirty: refused before anything is closed.
    std::fs::write(wt.join("u.txt"), "u\n").unwrap();
    let e = err_text(h.ui("worktree_remove", json!({"session_id": wid, "force": true})));
    assert!(e.contains("uncommitted"), "{e}");
    std::fs::remove_file(wt.join("u.txt")).unwrap();

    // A repository nested inside it (in an ignored folder, where git's clean check can't see).
    std::fs::write(wt.join(".gitignore"), ".worktrees/\n").unwrap();
    git(&wt, &["add", ".gitignore"]);
    git(&wt, &["commit", "-m", "ignore"]);
    let inner = wt.join(".worktrees").join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-b", "main"]);
    let e = err_text(h.ui("worktree_remove", json!({"session_id": wid, "force": true})));
    assert!(e.contains("repository lives inside"), "{e}");
    std::fs::remove_dir_all(wt.join(".worktrees")).unwrap();

    // Open sessions without force: refused, naming them.
    let e = err_text(h.ui("worktree_remove", json!({"session_id": wid})));
    assert!(e.contains("force: true") && e.contains("a ⎇ wt"), "{e}");
    assert!(h.engine.view().session(&wid).is_some());

    // With force: the live session is closed (and waited for), the folder removed, the sleeping
    // session deleted afterwards, and the unmerged branch kept by `branch -d`.
    let out = h.ok("worktree_remove", json!({"session_id": id, "path": s(&wt), "force": true, "delete_branch": true}));
    assert!(!wt.exists());
    assert!(out.message.contains("Kept the branch wt"), "{}", out.message);
    let v = h.engine.view();
    assert!(v.session(&wid).is_none() && v.session(&sleeper).is_none());
    assert!(v.session(&id).is_some());
    assert_eq!(out.data["closed_sessions"].as_array().unwrap().len(), 1);
    assert!(git(&r.a, &["branch", "--list", "wt"]).contains("wt"), "an unmerged branch is kept");
    // The closed one can come back from Recently closed; the sleeping one never went there.
    let closed = h.ok("recently_closed_list", json!({}));
    assert_eq!(closed.data.as_array().map(Vec::len), Some(1), "{}", closed.data);

    // A merged branch is deleted.
    let o = h.ok("worktree_new", json!({"session_id": id, "branch": "merged", "open_session": false}));
    let out = h.ok("worktree_remove", json!({"session_id": id, "path": o.data["path"], "delete_branch": true}));
    assert!(out.message.contains("Deleted the branch merged"), "{}", out.message);

    // Prune: a worktree whose folder is gone.
    let o = h.ok("worktree_new", json!({"session_id": id, "branch": "gone", "open_session": false}));
    std::fs::remove_dir_all(o.data["path"].as_str().unwrap()).unwrap();
    let l = h.ok("worktree_list", json!({"session_id": id}));
    assert_eq!(l.data["prunable"], 1);
    let p = h.ok("worktree_prune", json!({"session_id": id}));
    assert_eq!(p.message, "Pruned 1 missing worktree.");
}

#[test]
fn not_a_repo_and_effects_are_honest() {
    let h = H::new();
    let plain = tempfile::tempdir().unwrap();
    let id = session_in(&h, plain.path(), "pwsh");
    assert!(matches!(h.ui("git_status", json!({"session_id": id})), Err(ToolError::Refused(_))));
    assert!(matches!(h.ui("git_fetch", json!({"session_id": id})), Err(ToolError::Refused(_))));
    let reg = h.engine.registry();
    use csm_engine::Effects as E;
    let fx = |id: &str| reg.get(id).unwrap().effects;
    assert!(fx("git_fetch").contains(E::ACCESSES_NETWORK) && !fx("git_fetch").contains(E::WRITES_FILESYSTEM));
    assert!(fx("git_pull").contains(E::ACCESSES_NETWORK | E::WRITES_FILESYSTEM));
    assert!(fx("git_switch_branch").contains(E::WRITES_FILESYSTEM));
    assert!(fx("worktree_new").contains(E::WRITES_FILESYSTEM));
    assert!(fx("worktree_remove").contains(E::MAY_DISCARD_DATA | E::WRITES_FILESYSTEM));
    assert!(fx("git_status").contains(E::IDEMPOTENT));
    // External callers are asked before a git process runs.
    assert!(fx("git_status").needs_consent());
    let _ = Caller::Cli;
    let _: Value = json!(null);
}

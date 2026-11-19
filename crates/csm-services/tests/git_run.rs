//! The git process layer against real repositories: a throwaway bare "remote" and clones of it
//! in a temp directory. Needs `git` on PATH (as the app does).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use csm_services::git::run;
use csm_services::git::watch::RepoWatchers;
use csm_services::git::worktrees;

/// Runs git for test setup; panics with git's output on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=csm",
            "-c",
            "user.email=csm@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(p: &Path, text: &str) {
    std::fs::write(p, text).unwrap();
}

struct Repos {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    remote: PathBuf,
    a: PathBuf,
}

/// `remote.git` (bare) with one commit on `main`, cloned to `a`.
fn repos() -> Repos {
    let tmp = tempfile::tempdir().unwrap();
    // The real form, so comparisons with git's own (long) paths hold on machines whose temp dir
    // has an 8.3 spelling.
    let root = PathBuf::from(run::canonical(&tmp.path().display().to_string()));
    let remote = root.join("remote.git");
    std::fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "--bare", "-b", "main"]);
    let seed = root.join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    write(&seed.join("a.txt"), "one\n");
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-m", "one"]);
    git(
        &seed,
        &["remote", "add", "origin", &remote.display().to_string()],
    );
    git(&seed, &["push", "-u", "origin", "main"]);
    let a = root.join("a");
    git(&root, &["clone", &remote.display().to_string(), "a"]);
    Repos {
        _tmp: tmp,
        root,
        remote,
        a,
    }
}

fn s(p: &Path) -> String {
    p.display().to_string()
}

/// Pushes a new commit to the remote from a second clone.
fn push_from_other(r: &Repos, file: &str) {
    let b = r.root.join("b");
    if !b.exists() {
        git(&r.root, &["clone", &s(&r.remote), "b"]);
    }
    git(&b, &["pull", "--ff-only"]);
    write(&b.join(file), "theirs\n");
    git(&b, &["add", "."]);
    git(&b, &["commit", "-m", file]);
    git(&b, &["push"]);
}

#[test]
fn status_reports_branch_upstream_ahead_behind_and_changes() {
    let r = repos();
    let st = run::status(&s(&r.a)).expect("a repo");
    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!((st.ahead, st.behind), (Some(0), Some(0)));
    assert!(!st.is_dirty());

    write(&r.a.join("a.txt"), "changed\n");
    write(&r.a.join("new.txt"), "x\n");
    git(&r.a, &["commit", "-am", "local"]);
    write(&r.a.join("a.txt"), "dirty again\n");
    push_from_other(&r, "theirs.txt");
    git(&r.a, &["fetch"]);
    let st = run::status(&s(&r.a)).unwrap();
    assert_eq!((st.ahead, st.behind), (Some(1), Some(1)));
    let codes: Vec<(String, String)> = st
        .changes
        .iter()
        .map(|c| (c.code.clone(), c.path.clone()))
        .collect();
    assert!(
        codes.contains(&(".M".to_string(), "a.txt".to_string()))
            || codes.iter().any(|(c, p)| c.contains('M') && p == "a.txt"),
        "{codes:?}"
    );
    assert!(
        codes.iter().any(|(c, p)| c == "??" && p == "new.txt"),
        "{codes:?}"
    );

    // Not a repo: no status, no root.
    let plain = r.root.join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert!(run::status(&s(&plain)).is_none() || run::repo_root(&s(&plain)).is_none());
    assert!(run::status(&s(&r.root.join("missing"))).is_none());
}

#[test]
fn fetch_then_pull_fast_forwards_and_a_diverged_branch_is_refused() {
    let r = repos();
    push_from_other(&r, "b1.txt");
    let f = run::fetch(&s(&r.a));
    assert!(f.ok, "{}", f.output);
    let st = run::status(&s(&r.a)).unwrap();
    assert_eq!(st.behind, Some(1));
    let p = run::pull_ff_only(&s(&r.a));
    assert!(p.ok, "{}", p.output);
    assert!(r.a.join("b1.txt").exists());

    // Diverge: a local commit and a remote one. Pull must refuse, even with pull.rebase=true.
    write(&r.a.join("mine.txt"), "mine\n");
    git(&r.a, &["add", "."]);
    git(&r.a, &["commit", "-m", "mine"]);
    git(&r.a, &["config", "pull.rebase", "true"]);
    push_from_other(&r, "b2.txt");
    let p = run::pull_ff_only(&s(&r.a));
    assert!(!p.ok, "a diverged branch must not be pulled: {}", p.output);
    assert!(
        !r.a.join("b2.txt").exists(),
        "nothing was merged or rebased"
    );
    let msg = run::failure_message("Pull", &p.output);
    assert!(msg.starts_with("Pull failed."), "{msg}");
    // The local commit is still the tip.
    assert!(git(&r.a, &["log", "-1", "--format=%s"]).trim() == "mine");
}

#[test]
fn switch_handles_a_dash_branch_and_never_guesses_a_remote_one() {
    let r = repos();
    // A branch literally named `-x`: `--` keeps it a branch name.
    // `git branch -x` refuses the name, but a ref can be made with it (a cloned repo can have one).
    git(&r.a, &["update-ref", "refs/heads/-x", "HEAD"]);
    let sw = run::switch_branch(&s(&r.a), "-x");
    assert!(sw.ok, "{}", sw.output);
    assert_eq!(run::status(&s(&r.a)).unwrap().branch.as_deref(), Some("-x"));
    assert!(run::branches(&s(&r.a)).unwrap().contains(&"-x".to_string()));
    assert!(run::switch_branch(&s(&r.a), "main").ok);

    // A branch that exists only on the remote: --no-guess must not create a tracking branch.
    let b = r.root.join("b");
    git(&r.root, &["clone", &s(&r.remote), "b"]);
    git(&b, &["switch", "-c", "remote-only"]);
    git(&b, &["push", "-u", "origin", "remote-only"]);
    assert!(run::fetch(&s(&r.a)).ok);
    let sw = run::switch_branch(&s(&r.a), "remote-only");
    assert!(
        !sw.ok,
        "a remote-only name must not be invented: {}",
        sw.output
    );
    assert!(
        !run::branches(&s(&r.a))
            .unwrap()
            .contains(&"remote-only".to_string())
    );
}

#[test]
fn worktrees_add_list_and_remove_with_the_refusals() {
    let r = repos();
    let a = s(&r.a);
    let wt = r.root.join("a-feat");
    let add = run::add_worktree(&a, &s(&wt), "feat", true, None);
    assert!(add.ok, "{}", add.output);
    let list = run::worktree_list(&a);
    assert_eq!(list.len(), 2, "{list:?}");
    assert!(worktrees::is_main(&list, &list[0]));
    assert!(worktrees::same_path(&list[0].path, &a));
    let w = worktrees::containing(&list, &s(&wt)).expect("the new worktree is listed");
    assert_eq!(w.branch.as_deref(), Some("feat"));
    assert!(!worktrees::is_main(&list, w));

    // Repo root and "worktree of" from inside the linked worktree.
    let root = run::repo_root(&s(&wt)).unwrap();
    assert!(worktrees::same_path(&root, &a), "{root} vs {a}");
    assert_eq!(
        run::linked_worktree_of(&s(&wt), Some(&root)).as_deref(),
        Some("a")
    );
    assert_eq!(
        run::linked_worktree_of(&a, Some(&root)),
        None,
        "the main checkout is not a worktree of anything"
    );
    // Its own git dir, for the watcher.
    let gd = run::git_dir(&s(&wt)).unwrap();
    assert!(
        gd.join("HEAD").is_file()
            && gd
                .to_string_lossy()
                .replace('\\', "/")
                .contains("/worktrees/"),
        "{}",
        gd.display()
    );

    // A dirty worktree is refused by git itself (no --force).
    write(&wt.join("untracked.txt"), "x\n");
    let rm = run::remove_worktree(&a, &s(&wt));
    assert!(!rm.ok, "{}", rm.output);
    assert!(wt.exists());
    std::fs::remove_file(wt.join("untracked.txt")).unwrap();

    // The main checkout can't be removed.
    assert!(!run::remove_worktree(&a, &a).ok);

    // A repository nested inside the worktree: the scan finds it, and refusing is the caller's
    // rule (git would delete it).
    let nested = wt.join(".worktrees").join("inner");
    std::fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-b", "main"]);
    let found = run::nested_repositories(&wt, run::NESTED_SCAN_MAX_DIRS).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    // Fails closed: a scan that hits its cap returns None.
    assert!(run::nested_repositories(&wt, 1).is_none());
    std::fs::remove_dir_all(wt.join(".worktrees")).unwrap();
    assert_eq!(
        run::nested_repositories(&wt, run::NESTED_SCAN_MAX_DIRS)
            .unwrap()
            .len(),
        0
    );

    // Unmerged work on the branch: removing the worktree works, deleting the branch with -d
    // keeps it.
    write(&wt.join("f.txt"), "f\n");
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-m", "feat work"]);
    let rm = run::remove_worktree(&a, &s(&wt));
    assert!(rm.ok, "{}", rm.output);
    assert!(!wt.exists());
    let del = run::delete_merged_branch(&a, "feat");
    assert!(!del.ok, "an unmerged branch is kept: {}", del.output);
    assert!(run::branches(&a).unwrap().contains(&"feat".to_string()));

    // A missing folder becomes prunable; prune removes only the bookkeeping.
    let gone = r.root.join("a-gone");
    assert!(run::add_worktree(&a, &s(&gone), "gone", true, None).ok);
    std::fs::remove_dir_all(&gone).unwrap();
    assert!(run::worktree_list(&a).iter().any(|w| w.is_prunable));
    assert!(run::prune_worktrees(&a).ok);
    assert_eq!(run::worktree_list(&a).len(), 1);
}

#[cfg(windows)]
#[test]
fn canonical_follows_junctions() {
    let r = repos();
    let link = r.root.join("via-junction");
    let st = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&r.a)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
    let c = run::canonical(&s(&link));
    assert!(worktrees::same_path(&c, &s(&r.a)), "{c}");
    // The repo root from the junction spelling is the real one too: one busy key per repo.
    let root = run::repo_root(&s(&link)).unwrap();
    assert_eq!(
        worktrees::busy_key(Some(&root), None),
        worktrees::busy_key(Some(&s(&r.a)), None)
    );
    // The nested scan does not follow it.
    assert_eq!(
        run::nested_repositories(&r.root.join("seed"), 1000)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn the_watcher_fires_on_checkout_and_is_shared() {
    let r = repos();
    git(&r.a, &["branch", "other"]);
    let watchers = RepoWatchers::new(Duration::from_millis(150));
    let hits = Arc::new(AtomicUsize::new(0));
    let gd = run::git_dir(&s(&r.a)).unwrap();
    let h = hits.clone();
    let g1 = watchers.acquire(
        &gd,
        Arc::new(move || {
            h.fetch_add(1, Ordering::SeqCst);
        }),
    );
    assert!(g1.is_some());
    let h2 = hits.clone();
    let g2 = watchers.acquire(
        &gd,
        Arc::new(move || {
            h2.fetch_add(100, Ordering::SeqCst);
        }),
    );
    assert_eq!(watchers.watched(), 1, "one watcher per git directory");
    git(&r.a, &["switch", "other"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while hits.load(Ordering::SeqCst) < 101 {
        assert!(
            Instant::now() < deadline,
            "no change seen (hits {})",
            hits.load(Ordering::SeqCst)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(g1);
    assert_eq!(watchers.watched(), 1);
    drop(g2);
    assert_eq!(watchers.watched(), 0, "the last guard ends the watch");
}

#[test]
fn a_hung_git_is_killed_at_its_deadline() {
    // `git hash-object --stdin` would wait for input forever; stdin is closed, so use a real
    // blocker instead: a credential prompt is disabled, so wait on a pager-less long command.
    let r = repos();
    let t0 = Instant::now();
    // `-c core.fsmonitor=...` would run a hook; keep it portable: an alias that sleeps.
    let sleeper = if cfg!(windows) {
        "!powershell -NoProfile -Command Start-Sleep 30"
    } else {
        "!sleep 30"
    };
    let args: Vec<String> = ["-c", &format!("alias.zz={sleeper}"), "zz"]
        .iter()
        .map(|x| x.to_string())
        .collect();
    let out = run::run_git(&s(&r.a), &args, Duration::from_millis(1500), true);
    assert!(out.timed_out, "{out:?}");
    assert!(
        t0.elapsed() < Duration::from_secs(15),
        "the tree was killed, not waited for: {:?}",
        t0.elapsed()
    );
}

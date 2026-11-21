#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
//! The Review add-on through the real plugin host, on a throwaway git repository: the shipped
//! manifest, the screen (changed files, a diff, a comment from a range selection), an agent that
//! lists, reads, replies and marks the thread addressed without a consent card, the refusals (an
//! agent cannot resolve or delete, nor work in another project), the person resolving, the threads
//! moving and going outdated as the file changes, persistence across a restart, and "Send to
//! agent" reaching the session as a bracketed paste.

#[path = "../../../crates/csm-engine/tests/common/mod.rs"]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_core::addons::screen::{Bind, EventKind, Kind, Node, ScreenEvent, ScreenTree};
use csm_engine::{Caller, ToolError, ToolOutput};
use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_csm-review");

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=csm", "-c", "user.email=csm@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

fn real(p: &Path) -> PathBuf {
    let c = fs::canonicalize(p).unwrap().display().to_string();
    PathBuf::from(c.strip_prefix(r"\\?\").unwrap_or(&c))
}

const LIB: &str = "fn main() {\n    let x = read()?;\n    println!(\"{x}\");\n}\n";

struct World {
    h: H,
    tmp: tempfile::TempDir,
    repo: PathBuf,
    session: String,
    project: String,
}

fn wait<T>(what: &str, f: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// An engine with the shipped Review manifest enabled and running, and a live session in a repo
/// with one uncommitted change and one untracked file.
fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let base = real(tmp.path());
    let repo = base.join("shop");
    fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "# shop\n").unwrap();
    fs::write(repo.join("src/lib.rs"), "fn main() {\n    let x = 1;\n}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "first"]);
    fs::write(repo.join("src/lib.rs"), LIB).unwrap();
    fs::write(repo.join("notes.txt"), "todo\n").unwrap();

    let shipped = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/addons/review/addon.toml")).unwrap();
    let wanted = "argv = [\"csm-review\"]";
    assert!(shipped.contains(wanted), "the shipped manifest changed shape");
    let text = shipped.replace(wanted, &format!("argv = [{}]", serde_json::to_string(EXE).unwrap()));
    let state_dir = base.join("state");
    let folder = state_dir.join("addons").join("review");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("addon.toml"), text).unwrap();
    let state = state_dir.join("state.json");
    let h = H::start(test_app(), move |o| {
        o.save_path = Some(state);
        o.restore_on_start = false;
    });
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    let a = h.ok("addon_list", json!({})).data["addons"][0].clone();
    assert_eq!(a["state"], "disabled", "shipped disabled until enabled");
    assert_eq!(a["problems"].as_array().unwrap().len(), 0, "{a}");
    h.ok("addon_enable", json!({"id": "review", "version": a["version"], "digest": a["digest"]}));
    wait("review running with its screen", || {
        let rows = h.ok("plugin_status", json!({})).data["processes"].clone();
        let up = rows.as_array()?.iter().any(|p| p["id"] == "review" && p["state"] == "running");
        (up && !h.engine.screens().declared().is_empty()).then_some(())
    });
    let session = h.ok("session_new", json!({"folder": repo.display().to_string(), "command": "pwsh"})).data["id"].as_str().unwrap().to_owned();
    let v = h.wait_until("the repo root", |s| s.session(&session).is_some_and(|x| x.repo_root.is_some()));
    let project = format!("repo:{}", v.session(&session).unwrap().repo_root.clone().unwrap());
    World { h, tmp, repo, session, project }
}

fn agent(w: &World) -> Caller {
    Caller::External { client: "claude-code".into(), session_id: Some(w.session.clone()) }
}

fn data_file(w: &World) -> PathBuf {
    let dir = w.tmp.path();
    let root = real(dir).join("state").join("addon-data").join("review").join("projects");
    let key = fs::read_dir(&root).unwrap().flatten().next().expect("a project folder").path();
    key.join("threads.json")
}

fn call(h: &H, who: Caller, tool: &str, args: Value) -> Result<ToolOutput, ToolError> {
    h.engine.call(who, tool, args)
}

fn walk<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    out.push(n);
    for c in n.children() {
        walk(c, out);
    }
    match &n.kind {
        Kind::Diff(d) => d.overlay.threads.iter().for_each(|t| walk(&t.child, out)),
        Kind::Code(c) => c.overlay.threads.iter().for_each(|t| walk(&t.child, out)),
        _ => {}
    }
}

fn all(t: &ScreenTree) -> Vec<&Node> {
    let mut v = Vec::new();
    walk(&t.root, &mut v);
    v
}

/// The threads of the `diff` or `code` node: (key, start, end, open).
fn threads_of(t: &ScreenTree) -> Vec<(String, u32, u32, bool)> {
    let overlay = |n: &Node| match &n.kind {
        Kind::Diff(d) => d.overlay.threads.iter().map(|x| (x.key.clone(), x.anchor.start, x.anchor.end, x.open)).collect::<Vec<_>>(),
        Kind::Code(c) => c.overlay.threads.iter().map(|x| (x.key.clone(), x.anchor.start, x.anchor.end, x.open)).collect(),
        _ => Vec::new(),
    };
    t.find("diff").or_else(|| t.find("code")).map(overlay).unwrap_or_default()
}

fn tree_where(h: &H, inst: &str, what: &str, f: impl Fn(&ScreenTree) -> bool) -> std::sync::Arc<ScreenTree> {
    wait(what, || h.engine.screens().tree(inst).filter(|t| f(t)))
}

fn send(h: &H, inst: &str, key: &str, kind: EventKind, value: Value) {
    h.engine.screen_event(inst, ScreenEvent::new(key, kind, value)).unwrap();
}

fn open_screen(w: &World) -> String {
    let out = w.h.ok("screen_open", json!({"screen": "review.changes", "context": {"session_id": w.session}}));
    let inst = out.data["instance"].as_str().unwrap().to_owned();
    tree_where(&w.h, &inst, "the changed files", |t| t.find("files").is_some());
    inst
}

fn file_paths(t: &ScreenTree) -> Vec<String> {
    match t.find("files").map(|n| &n.kind) {
        Some(Kind::FileList { files, .. }) => files.iter().map(|f| f.path.clone()).collect(),
        _ => Vec::new(),
    }
}

#[test]
fn a_person_comments_an_agent_answers_and_only_the_person_resolves() {
    let w = world();
    let h = &w.h;
    let inst = open_screen(&w);
    let t = h.engine.screens().tree(&inst).unwrap();
    assert_eq!(file_paths(&t), vec!["src/lib.rs", "notes.txt"], "the changed file, then the untracked one");

    // Pick the file: its diff shows, with the comment range action.
    send(h, &inst, "files", EventKind::Select, json!({"path": "src/lib.rs"}));
    let t = tree_where(h, &inst, "the diff", |t| t.find("diff").is_some());
    assert!(matches!(&t.find("diff").unwrap().kind, Kind::Diff(d) if d.overlay.range_actions.len() == 1 && d.path == "src/lib.rs"));

    // Select line 2 and comment on it, as the person.
    send(h, &inst, "diff", EventKind::RangeAction, json!({"id": "comment", "side": "new", "start": 2, "end": 2}));
    tree_where(h, &inst, "the draft", |t| threads_of(t).iter().any(|x| x.0 == "draft"));
    send(h, &inst, "draft-input", EventKind::Submit, json!({"value": "This swallows the error; return it."}));
    let t = tree_where(h, &inst, "the new thread", |t| threads_of(t).iter().any(|x| x.0.starts_with("t_")));
    let th = threads_of(&t);
    assert_eq!((th.len(), th[0].1, th[0].2, th[0].3), (1, 2, 2, true), "an open thread on line 2");

    // The agent in the session lists and reads it: no consent card (these declare no consent effect).
    let list = call(h, agent(&w), "review.list", json!({})).unwrap();
    assert_eq!(list.data["total"], 1);
    let item = &list.data["threads"][0];
    assert_eq!((item["status"].clone(), item["path"].clone(), item["location"]["state"].clone()), (json!("open"), json!("src/lib.rs"), json!("exact")));
    assert_eq!(item["location"]["start"], 2);
    assert_eq!(item["first_comment"], "This swallows the error; return it.");
    let tid = item["id"].as_str().unwrap().to_owned();
    let got = call(h, agent(&w), "review.get", json!({"thread_id": tid})).unwrap();
    assert!(got.data["current_lines"].as_str().unwrap().contains("read()"), "{}", got.data);
    assert_eq!(got.data["comments"][0]["author"]["kind"], "person");

    // It replies and marks the thread addressed in one call; the open screen follows.
    let r = call(h, agent(&w), "review.reply", json!({"thread_id": tid, "body": "Returned it in 1a2b3c.", "status": "addressed"})).unwrap();
    assert_eq!(r.data["status"], "addressed");
    let t = tree_where(h, &inst, "the thread listed as addressed", |t| match t.find("threads").map(|n| &n.kind) {
        Some(Kind::List { items, .. }) => items.iter().any(|i| i.badges.iter().any(|b| b.text == "addressed")) && items.iter().any(|i| i.badges.iter().any(|b| b.text == "agent")),
        _ => false,
    });
    assert!(all(&t).iter().any(|n| matches!(&n.kind, Kind::Comment(c) if c.agent)), "the agent's comment carries the agent mark");

    // What an agent may not do.
    for (tool, args) in [
        ("review.set_status", json!({"thread_id": tid, "status": "resolved"})),
        ("review.reply", json!({"thread_id": tid, "body": "done", "status": "resolved"})),
        ("review.delete", json!({"thread_id": tid})),
        ("review.list", json!({"project": "repo:C:/somewhere/else"})),
    ] {
        let e = call(h, agent(&w), tool, args.clone()).unwrap_err().to_string();
        assert!(e.contains("only the person") || e.contains("own project"), "{tool}: {e}");
    }
    let still = call(h, agent(&w), "review.get", json!({"thread_id": tid})).unwrap();
    assert_eq!(still.data["status"], "addressed", "the refusals changed nothing");
    // An MCP client with no session must name a project; another add-on cannot change threads.
    let outsider = Caller::External { client: "some-client".into(), session_id: None };
    assert!(call(h, outsider.clone(), "review.list", json!({})).is_err());
    assert_eq!(call(h, outsider.clone(), "review.list", json!({"project": w.project})).unwrap().data["total"], 1);
    assert!(call(h, outsider, "review.set_status", json!({"thread_id": tid, "status": "resolved", "project": w.project})).is_err());
    let other_addon = Caller::External { client: "add-on demo".into(), session_id: None };
    assert!(call(h, other_addon, "review.reply", json!({"thread_id": tid, "body": "x", "project": w.project})).is_err());

    // An agent may start a note, which is marked as its own.
    let c = call(h, agent(&w), "review.create", json!({"path": "README.md", "start": 1, "end": 1, "body": "Say what shop is."})).unwrap();
    let note_id = c.data["thread_id"].as_str().unwrap().to_owned();
    let note = call(h, agent(&w), "review.get", json!({"thread_id": note_id})).unwrap();
    assert_eq!((note.data["comments"][0]["author"]["kind"].clone(), note.data["status"].clone()), (json!("agent"), json!("open")));
    assert_eq!(note.data["comments"][0]["author"]["session_id"], json!(w.session));
    assert!(call(h, agent(&w), "review.create", json!({"path": "../etc/passwd", "start": 1, "end": 1, "body": "x"})).is_err());
    assert!(call(h, agent(&w), "review.create", json!({"path": "README.md", "start": 5, "end": 9, "body": "x"})).is_err(), "lines that are not in the file");
    assert!(call(h, agent(&w), "review.create", json!({"path": "README.md", "start": 1, "end": 1, "body": "   "})).is_err());

    // The person resolves (UI, and the CLI), and the person deletes.
    let r = h.ok("review.set_status", json!({"thread_id": tid, "status": "resolved", "project": w.project}));
    assert_eq!(r.data["status"], "resolved");
    assert_eq!(call(&w.h, Caller::Cli, "review.set_status", json!({"thread_id": tid, "status": "open", "project": w.project})).unwrap().data["status"], "open");
    assert!(call(h, Caller::Cli, "review.list", json!({})).is_err(), "the CLI names a project too");
    h.ok("review.delete", json!({"thread_id": note_id, "project": w.project}));
    assert!(call(h, agent(&w), "review.get", json!({"thread_id": note_id})).is_err());

    // Persistence: the threads are in the add-on's data folder, and survive a restart of the add-on.
    let file: Value = serde_json::from_str(&fs::read_to_string(data_file(&w)).unwrap()).unwrap();
    assert_eq!(file["threads"].as_array().unwrap().len(), 1);
    assert_eq!(file["threads"][0]["anchor"]["path"], "src/lib.rs");
    assert!(file["threads"][0]["anchor"]["snippet"].as_str().unwrap().contains("read()"));
    assert!(!w.repo.join(".csm").exists() && !w.repo.join("threads.json").exists(), "nothing is written in the repository");
    h.ok("plugin_restart", json!({"id": "review"}));
    wait("review running again", || {
        let rows = h.ok("plugin_status", json!({})).data["processes"].clone();
        rows.as_array()?.iter().any(|p| p["id"] == "review" && p["state"] == "running").then_some(())
    });
    let again = wait("the list after a restart", || call(h, agent(&w), "review.list", json!({"status": "all"})).ok());
    assert_eq!(again.data["total"], 1);
    assert_eq!(again.data["threads"][0]["status"], "open");
    assert_eq!(again.data["threads"][0]["comment_count"], 2, "the person's comment and the agent's reply: {}", again.data);
    h.engine.shutdown();
}

#[test]
fn a_thread_moves_with_its_lines_and_goes_outdated_when_they_are_gone() {
    let w = world();
    let h = &w.h;
    let made = call(h, agent(&w), "review.create", json!({"path": "src/lib.rs", "start": 3, "end": 3, "body": "Do not print this."})).unwrap();
    let tid = made.data["thread_id"].as_str().unwrap().to_owned();
    let state = |h: &H| {
        let g = call(h, agent(&w), "review.get", json!({"thread_id": tid})).unwrap();
        (g.data["location"]["state"].as_str().unwrap().to_owned(), g.data["location"]["start"].as_u64(), g.data["current_lines"].as_str().map(str::to_owned))
    };
    assert_eq!(state(h), ("exact".into(), Some(3), Some("    println!(\"{x}\");".into())));
    // Two lines are added above: the thread follows its line.
    fs::write(w.repo.join("src/lib.rs"), format!("// header\n// more header\n{LIB}")).unwrap();
    assert_eq!(state(h), ("moved".into(), Some(5), Some("    println!(\"{x}\");".into())));
    let listed = call(h, agent(&w), "review.list", json!({})).unwrap();
    assert_eq!(listed.data["threads"][0]["location"]["start"], 5);
    // The line is edited a little: still found, by its neighbours.
    fs::write(w.repo.join("src/lib.rs"), format!("// header\n// more header\n{}", LIB.replace("println!(\"{x}\")", "log(\"{x}\")"))).unwrap();
    assert_eq!(state(h).0, "moved");
    // The lines are removed: outdated, and the thread still says what it was about.
    fs::write(w.repo.join("src/lib.rs"), "fn other() {}\n").unwrap();
    let (st, start, now) = state(h);
    assert_eq!((st.as_str(), start, now), ("outdated", None, None));
    let g = call(h, agent(&w), "review.get", json!({"thread_id": tid})).unwrap();
    assert!(g.data["snippet"].as_str().unwrap().contains("println"), "the quoted code stays");
    // The screen lists it, marked outdated.
    let inst = open_screen(&w);
    send(h, &inst, "files", EventKind::Select, json!({"path": "src/lib.rs"}));
    tree_where(h, &inst, "the outdated badge on the open file", |t| matches!(t.find("threads").map(|n| &n.kind), Some(Kind::List { items, .. }) if items.iter().any(|i| i.badges.iter().any(|b| b.text == "outdated"))));
    h.engine.shutdown();
}

#[test]
fn send_to_agent_pastes_a_short_prompt_after_the_person_confirms() {
    let w = world();
    let h = &w.h;
    let pty = h.pty(0);
    pty.emit(b"\x1b[?2004h");
    let inst = open_screen(&w);
    // No threads yet: the button is there but has nothing to send.
    let t = h.engine.screens().tree(&inst).unwrap();
    let send_buttons = |t: &ScreenTree| -> Vec<(bool, Option<(String, Value)>)> {
        all(t)
            .iter()
            .filter_map(|n| match &n.kind {
                Kind::Button(b) if b.label == "Send to agent" => Some((b.disabled, if let Bind::Action(a) = &b.bind { Some((a.tool.clone(), a.params.clone())) } else { None })),
                _ => None,
            })
            .collect()
    };
    assert!(send_buttons(&t).iter().all(|(disabled, a)| *disabled && a.is_none()));

    let made = call(h, agent(&w), "review.create", json!({"path": "src/lib.rs", "start": 2, "end": 3, "body": "Handle the error\nproperly."})).unwrap();
    let tid = made.data["thread_id"].as_str().unwrap().to_owned();
    let t = tree_where(h, &inst, "the send button", |t| send_buttons(t).iter().any(|(d, a)| !*d && a.is_some()));
    let (_, action) = send_buttons(&t).into_iter().find(|(d, _)| !*d).unwrap();
    let (tool, params) = action.unwrap();
    assert_eq!(tool, "session_send_text");
    assert_eq!(params["session_id"], json!(w.session));
    assert_eq!(params["submit"], false, "the person presses Enter");
    let text = params["text"].as_str().unwrap().to_owned();
    assert!(text.contains(&tid) && text.contains("src/lib.rs:2-3") && text.contains("Handle the error properly."), "{text}");
    assert!(text.contains("review.get") && text.contains("review.reply") && text.contains("toolset_enable"), "{text}");
    assert!(text.len() < 2000);
    // The tool the button is bound to (CSM draws the confirm; here we run what it runs).
    let prompt = call(h, agent(&w), "review.prompt", json!({})).unwrap();
    assert_eq!(prompt.data["text"].as_str().unwrap(), text, "the button and the tool share one formatter");
    h.ok("session_send_text", params);
    let written = wait("the paste", || Some(pty.written_text()).filter(|t| t.contains("Handle the error properly.")));
    assert!(written.contains("\x1b[200~") && written.contains("\x1b[201~"), "bracketed paste: {written:?}");
    h.engine.shutdown();
}

#[test]
fn hostile_file_contents_and_missing_files_still_show() {
    let w = world();
    let h = &w.h;
    fs::write(w.repo.join("evil.txt"), "plain\n\x1b[31mred\x1b[0m \u{202e}reversed\nbell\x07\n").unwrap();
    fs::write(w.repo.join("blob.bin"), b"\x00\x01\x02binary").unwrap();
    let inst = open_screen(&w);
    send(h, &inst, "refresh", EventKind::Click, Value::Null);
    tree_where(h, &inst, "the untracked files", |t| file_paths(t).iter().any(|p| p == "evil.txt") && file_paths(t).iter().any(|p| p == "blob.bin"));
    send(h, &inst, "files", EventKind::Select, json!({"path": "evil.txt"}));
    let t = tree_where(h, &inst, "the code view", |t| t.find("code").is_some());
    assert!(matches!(&t.find("code").unwrap().kind, Kind::Code(_)));
    send(h, &inst, "files", EventKind::Select, json!({"path": "blob.bin"}));
    tree_where(h, &inst, "the binary note", |t| t.find("code").is_none() && all(t).iter().any(|n| matches!(&n.kind, Kind::Empty { title, .. } if title.contains("binary"))));
    // A file that is not in the list is ignored, and an event for another screen is dropped.
    send(h, &inst, "files", EventKind::Select, json!({"path": "../../etc/passwd"}));
    send(h, &inst, "files", EventKind::Select, json!({"path": 7}));
    send(h, &inst, "diff", EventKind::RangeAction, json!({"id": "comment", "start": "x"}));
    send(h, &inst, "reply-t_nope-1", EventKind::Submit, json!({"value": "hello"}));
    send(h, &inst, "refresh", EventKind::Click, Value::Null);
    tree_where(h, &inst, "still alive", |t| t.find("files").is_some());
    // The Files source lists tracked files and shows one as code.
    send(h, &inst, "source", EventKind::Select, json!({"value": "files"}));
    let t = tree_where(h, &inst, "tracked files", |t| file_paths(t).iter().any(|p| p == "README.md"));
    assert!(!file_paths(&t).iter().any(|p| p == "notes.txt"), "untracked files are not tracked files");
    send(h, &inst, "files", EventKind::Select, json!({"path": "README.md"}));
    tree_where(h, &inst, "README as code", |t| t.find("code").is_some());
    // The add-on keeps answering tools after all that.
    assert_eq!(call(h, agent(&w), "review.list", json!({})).unwrap().data["total"], 0);
    h.engine.shutdown();
}

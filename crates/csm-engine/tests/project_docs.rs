//! `project_docs_list` and `project_doc_get` through `Engine::call`: discovery order and case,
//! the repository root beside the session's folder, refusal of anything not listed (traversal,
//! other files), the 1 MiB cap, non-UTF-8 text, and that an MCP caller needs no consent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::Path;

use common::H;
use csm_engine::{Caller, ToolError};
use serde_json::{Value, json};

fn write(p: &Path, s: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

fn session_in(h: &H, folder: &Path) -> String {
    let out = h.ok("session_new", json!({"folder": folder.to_string_lossy(), "command": "pwsh"}));
    let id = out.data["id"].as_str().unwrap().to_owned();
    h.wait_status(&id, csm_engine::Status::Idle);
    id
}

fn rels(v: &Value) -> Vec<String> {
    v["docs"].as_array().unwrap().iter().map(|d| d["rel"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn lists_the_docs_in_order_ignoring_case_with_the_repo_root() {
    let h = H::new();
    let root = h.dir.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(&root.join("CLAUDE.md"), b"# root rules");
    write(&root.join("readme.MD"), b"# root readme");
    let sub = root.join("crates").join("app");
    write(&sub.join("Agents.md"), b"# agents");
    write(&sub.join("GEMINI.md"), b"# gemini");
    write(&sub.join("CLAUDE.local.md"), b"# local");
    write(&sub.join(".claude").join("CLAUDE.md"), b"# dot");
    write(&sub.join("notes.md"), b"not a doc");
    let id = session_in(&h, &sub);

    let out = h.ok("project_docs_list", json!({"session_id": id}));
    assert_eq!(
        rels(&out.data),
        ["Agents.md", "CLAUDE.local.md", "GEMINI.md", ".claude/CLAUDE.md", "../../readme.MD", "../../CLAUDE.md"],
        "the folder's docs in name order, then the repository root's"
    );
    let docs = out.data["docs"].as_array().unwrap();
    assert_eq!(docs[0]["name"], "AGENTS.md", "the canonical name, whatever the case on disk");
    assert_eq!(docs[0]["scope"], "folder");
    assert_eq!(docs[5]["scope"], "repo_root");
    assert_eq!(docs[0]["size"], 8);
    assert!(docs[0]["modified_ms"].as_u64().unwrap() > 0);
    assert!(Path::new(docs[0]["path"].as_str().unwrap()).is_absolute());
    assert_eq!(out.data["default"], "../../CLAUDE.md", "CLAUDE.md first, even at the root");
    assert_eq!(out.data["looked_for"].as_array().unwrap().len(), 6);

    // The same folder by path; at the root itself nothing is listed twice.
    let by_path = h.ok("project_docs_list", json!({"path": sub.to_string_lossy()}));
    assert_eq!(rels(&by_path.data), rels(&out.data));
    let at_root = h.ok("project_docs_list", json!({"path": root.to_string_lossy()}));
    assert_eq!(rels(&at_root.data), ["readme.MD", "CLAUDE.md"]);
    assert_eq!(at_root.data["repo_root"], Value::Null);

    // A folder with none says what was looked for.
    let empty = h.dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let none = h.ok("project_docs_list", json!({"path": empty.to_string_lossy()}));
    assert!(rels(&none.data).is_empty());
    assert_eq!(none.data["default"], Value::Null);
    assert!(none.message.contains("README.md, CLAUDE.md, AGENTS.md"), "{}", none.message);
}

#[test]
fn get_reads_listed_docs_and_refuses_everything_else() {
    let h = H::new();
    let root = h.dir.path().join("repo");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write(&root.join("CLAUDE.md"), b"# Root\n\nrules");
    write(&root.join("secret.env"), b"TOKEN=x");
    let sub = root.join("app");
    write(&sub.join("README.md"), b"# App");
    write(&sub.join("other.md"), b"no");
    let id = session_in(&h, &sub);

    let got = h.ok("project_doc_get", json!({"session_id": id, "file": "README.md"}));
    assert_eq!(got.data["text"], "# App");
    assert_eq!((got.data["truncated"].as_bool(), got.data["lossy"].as_bool()), (Some(false), Some(false)));
    assert_eq!(h.ok("project_doc_get", json!({"session_id": id, "file": "../CLAUDE.md"})).data["text"], "# Root\n\nrules");
    // By absolute path, too.
    let abs = sub.join("README.md").to_string_lossy().into_owned();
    assert_eq!(h.ok("project_doc_get", json!({"session_id": id, "file": abs})).data["text"], "# App");

    let secret_abs = root.join("secret.env").to_string_lossy().into_owned();
    for bad in ["../secret.env", "other.md", "./../app/../secret.env", "..\\secret.env", &secret_abs, "missing.md", ""] {
        let e = h.ui("project_doc_get", json!({"session_id": id, "file": bad})).unwrap_err();
        assert!(matches!(e, ToolError::Refused(_)), "{bad}: {e:?}");
    }
    // Params are checked.
    assert!(matches!(h.ui("project_doc_get", json!({"file": "README.md"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("project_doc_get", json!({"session_id": id, "path": "C:\\", "file": "x"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("project_docs_list", json!({"path": "relative/dir"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("project_docs_list", json!({"session_id": "nope"})), Err(ToolError::NotFound(_))));
}

#[test]
fn big_and_non_utf8_docs() {
    let h = H::new();
    let f = h.dir.path().join("p");
    write(&f.join("README.md"), &vec![b'a'; 1024 * 1024 + 10]);
    write(&f.join("CLAUDE.md"), b"caf\xe9 \xff ok");
    let path = f.to_string_lossy().into_owned();
    let big = h.ok("project_doc_get", json!({"path": path, "file": "README.md"}));
    assert_eq!(big.data["truncated"], true);
    assert_eq!(big.data["text"].as_str().unwrap().len(), 1024 * 1024);
    assert_eq!(big.data["size"], 1024 * 1024 + 10);
    let lossy = h.ok("project_doc_get", json!({"path": path, "file": "CLAUDE.md"}));
    assert_eq!(lossy.data["lossy"], true);
    assert_eq!(lossy.data["text"], "caf\u{FFFD} \u{FFFD} ok");
}

#[test]
fn ssh_sessions_have_none_and_mcp_callers_need_no_consent() {
    let h = H::new();
    let ssh = h.ok("session_new", json!({"command": "bash", "kind": "ssh", "ssh": {"host": "box"}, "focus": false})).data["id"].as_str().unwrap().to_owned();
    assert!(matches!(h.ui("project_docs_list", json!({"session_id": ssh})), Err(ToolError::Refused(_))));

    write(&h.dir.path().join("AGENTS.md"), b"# a");
    let mcp = Caller::External { client: "test-mcp".into(), session_id: None };
    let sid = session_in(&h, h.dir.path());
    // Read-only effects: no consent card, so these answer at once. An MCP client names a session.
    let l = h.engine.call(mcp.clone(), "project_docs_list", json!({"session_id": sid})).unwrap();
    assert_eq!(rels(&l.data), ["AGENTS.md"]);
    let t = h.engine.call(mcp, "project_doc_get", json!({"session_id": sid, "file": "AGENTS.md"})).unwrap();
    assert_eq!(t.data["text"], "# a");
    let spec = h.engine.registry().get("project_doc_get").copied().unwrap();
    assert!(spec.effects.read_only_hint() && !spec.effects.needs_consent());
}

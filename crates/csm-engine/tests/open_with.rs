//! "Open with…" and "Open folder" through the tools. Nothing here launches a program: every
//! call is one that must be refused before a process starts.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{H, test_app};
use csm_core::models::{OpenWithEntry, SessionKind, ShellSession};
use csm_engine::{Caller, ToolError};
use serde_json::json;

fn rig(folder: &str, list: Vec<OpenWithEntry>) -> H {
    let mut app = test_app();
    app.settings.open_with = list;
    app.sessions.push(ShellSession {
        id: "s".into(),
        name: "s".into(),
        command: "pwsh".into(),
        working_folder: folder.into(),
        is_dormant: true,
        ..Default::default()
    });
    app.sessions.push(ShellSession {
        id: "r".into(),
        name: "r".into(),
        kind: SessionKind::Ssh,
        ssh_host: "box".into(),
        ssh_user: "al".into(),
        ssh_remote_folder: "/srv/app".into(),
        is_dormant: true,
        ..Default::default()
    });
    H::start(app, |_| {})
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!(r"C:\Program Files\Csm Test Editors\{name}.exe") } else { format!("/opt/csm-test/{name}") }
}

fn vscode() -> OpenWithEntry {
    OpenWithEntry {
        id: "vs".into(),
        label: "VS Code".into(),
        executable: exe("Code"),
        local_args: Some("{folder}".into()),
        ssh_args: Some("--folder-uri vscode-remote://ssh-remote+{userAtHost:uri}{remoteFolder:uri}".into()),
        is_default: true,
        ..Default::default()
    }
}

fn err(h: &H, id: &str, p: serde_json::Value) -> ToolError {
    h.ui(id, p.clone()).err().unwrap_or_else(|| panic!("{id} {p} should fail"))
}

#[test]
fn the_snapshot_and_list_show_each_entrys_program_and_resolved_command() {
    let dir = tempfile::tempdir().unwrap();
    let h = rig(&dir.path().display().to_string(), vec![vscode()]);
    let v = h.engine.view();
    assert_eq!(v.open_with.len(), 1);
    let s = v.session("s").unwrap();
    assert_eq!(s.open_with.len(), 1);
    assert_eq!(s.open_with[0].program, if cfg!(windows) { "Code.exe" } else { "Code" });
    assert!(s.open_with[0].is_default);
    assert!(s.open_with[0].command.contains(&dir.path().display().to_string()), "{}", s.open_with[0].command);
    let r = v.session("r").unwrap();
    assert!(r.open_with[0].command.contains("vscode-remote://ssh-remote+al%40box/srv/app"), "{}", r.open_with[0].command);
    let out = h.ok("open_with_list", json!({"session_id": "r"}));
    assert_eq!(out.data["entries"][0]["label"], "VS Code");
    assert_eq!(out.data["for_session"][0]["entry_id"], "vs");
    h.engine.shutdown();
}

#[test]
fn open_with_refuses_before_starting_anything() {
    let dir = tempfile::tempdir().unwrap();
    let h = rig(&dir.path().display().to_string(), vec![vscode()]);
    // The program doesn't exist: refused at prepare, nothing launched.
    let e = err(&h, "open_with", json!({"session_id": "s"}));
    assert!(matches!(&e, ToolError::Failed(m) if m.contains("not found")), "{e:?}");
    let e = err(&h, "open_with", json!({"session_id": "s", "entry": "Nope"}));
    assert!(matches!(e, ToolError::NotFound(_)));
    let e = err(&h, "open_with", json!({"session_id": "missing"}));
    assert!(matches!(e, ToolError::NotFound(_)));
    h.engine.shutdown();
}

#[test]
fn open_with_set_checks_every_entry_and_says_what_is_wrong() {
    let h = rig(r"C:\src", vec![]);
    let set = |entries: serde_json::Value| h.ui("open_with_set", json!({"entries": entries}));
    let mut bad = vec![
        (json!({"label": "Rel", "executable": "code.exe", "local_args": "{folder}"}), "full path"),
        (json!({"label": "NoArgs", "executable": exe("Ed"), "local_args": ""}), "at least one"),
        (json!({"label": "Literal", "executable": exe("Ed"), "local_args": "--new-window"}), "placeholder"),
        (json!({"label": "Unknown", "executable": exe("Ed"), "local_args": "{host}"}), "not available"),
        (json!({"label": "Quote", "executable": exe("Ed"), "local_args": "\"{folder}"}), "Unbalanced"),
        (json!({"label": "Ctl", "executable": exe("Ed"), "local_args": "{folder}\u{7}"}), "control"),
    ];
    if cfg!(windows) {
        bad.extend([
            (json!({"label": "Sys", "executable": r"C:\Windows\System32\notepad.exe", "local_args": "{folder}"}), "not accepted"),
            (json!({"label": "Unc", "executable": r"\\server\share\ed.exe", "local_args": "{folder}"}), "Network"),
            (json!({"label": "Shell", "executable": r"C:\Tools\pwsh.exe", "local_args": "{folder}"}), "shell"),
            (json!({"label": "Script", "executable": r"C:\Tools\ed.ps1", "local_args": "{folder}"}), ".exe"),
        ]);
    } else {
        bad.extend([(json!({"label": "Shell", "executable": "/bin/bash", "local_args": "{folder}"}), "shell")]);
    }
    for (entry, why) in bad {
        let e = set(json!([entry.clone()])).unwrap_err();
        assert!(matches!(&e, ToolError::InvalidParams(m) if m.contains(why)), "{entry}: {e:?}");
    }
    // A good list is cleaned: one default, ids filled in.
    let ok = set(json!([
        {"label": "A", "executable": exe("A"), "local_args": "{folder}"},
        {"label": "B", "executable": exe("B"), "local_args": "{folder}", "is_default": true},
    ]))
    .unwrap();
    let entries = ok.data["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries.iter().filter(|e| e["is_default"] == true).count(), 1);
    assert!(entries.iter().all(|e| e["id"].as_str().is_some_and(|i| !i.is_empty())));
    // MCP clients may not decide what runs on a click; the CLI may.
    let ext = Caller::External { client: "c".into(), session_id: None };
    let e = h.engine.call(ext, "open_with_set", json!({"entries": []})).unwrap_err();
    assert!(matches!(e, ToolError::Refused(_)));
    assert!(h.engine.call(Caller::Cli, "open_with_set", json!({"entries": []})).is_ok());
    h.engine.shutdown();
}

#[test]
#[cfg(windows)]
fn batch_file_entries_refuse_cmd_metacharacters_in_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let hostile = dir.path().join("a&calc");
    std::fs::create_dir(&hostile).unwrap();
    let bat = OpenWithEntry {
        id: "b".into(),
        label: "Bat".into(),
        executable: dir.path().join("ed.cmd").display().to_string(),
        local_args: Some("{folder}".into()),
        ..Default::default()
    };
    std::fs::write(&bat.executable, "@echo off").unwrap();
    let h = rig(&hostile.display().to_string(), vec![bat]);
    let e = err(&h, "open_with", json!({"session_id": "s"}));
    assert!(matches!(&e, ToolError::Failed(m) if m.contains("batch file")), "{e:?}");
    h.engine.shutdown();
}

#[test]
fn open_folder_opens_only_existing_local_folders() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("run-me.exe");
    std::fs::write(&file, "").unwrap();
    let h = rig(&file.display().to_string(), vec![]);
    let e = err(&h, "open_folder", json!({"session_id": "s"}));
    assert!(matches!(&e, ToolError::Failed(m) if m.contains("not an existing folder")), "a file is never opened: {e:?}");
    let e = err(&h, "open_folder", json!({"session_id": "r"}));
    assert!(matches!(e, ToolError::Refused(_)), "SSH has no local folder");
    h.engine.shutdown();
}

#[test]
fn a_hostile_list_in_state_json_is_cleaned_on_load() {
    let mut list = vec![vscode()];
    list.push(OpenWithEntry {
        id: "x".into(),
        label: "Evil".into(),
        executable: if cfg!(windows) { r"C:\Windows\System32\cmd.exe".into() } else { "/bin/sh".into() },
        local_args: Some("/c calc {folder}".into()),
        ..Default::default()
    });
    list.push(OpenWithEntry {
        id: "y".into(),
        label: "Literal".into(),
        executable: exe("Lit"),
        local_args: Some("--always-this".into()),
        ..Default::default()
    });
    let h = rig(r"C:\src", list);
    let v = h.wait_until("cleaned", |v| v.open_with.len() == 1);
    assert_eq!(v.open_with[0].label, "VS Code");
    h.engine.shutdown();
}

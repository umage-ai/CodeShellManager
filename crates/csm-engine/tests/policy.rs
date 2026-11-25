//! Who may change what, through `Engine::call`: the cosmetic-only settings allow-list for MCP
//! clients and paired devices (SEC-3), "decides what runs" tools against every non-person
//! caller (SEC-19), the backup export's guards (SEC-16) and import's launch settings (SEC-5),
//! project docs for MCP callers (SEC-13) and OSC 52 clipboard writes (SEC-17).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::H;
use csm_engine::{Caller, ToolError};
use csm_peer_proto::msg::Level;
use serde_json::{Value, json};

fn mcp() -> Caller {
    Caller::External { client: "an-agent".into(), session_id: None }
}

fn peer() -> Caller {
    Caller::Peer { device_id: "dev1".into(), name: "Laptop".into(), level: Level::Full }
}

fn refused(r: &Result<csm_engine::ToolOutput, ToolError>) -> bool {
    matches!(r, Err(ToolError::Refused(_)))
}

#[test]
fn settings_set_is_an_allow_list_for_mcp_clients_and_paired_devices() {
    let h = H::new();
    for caller in [mcp(), peer()] {
        // Cosmetic: allowed.
        let r = h.engine.call(caller.clone(), "settings_set", json!({"key": "UiZoomPercent", "value": 110}));
        assert!(r.is_ok(), "{caller:?}: {r:?}");
        let r = h.engine.call(caller.clone(), "settings_set", json!({"key": "terminal_font_size", "value": 15}));
        assert!(r.is_ok(), "{caller:?}: {r:?}");
        // Everything else: refused, with a pointer to Settings.
        for key in [
            "OfferMcpToClaude",
            "AllowOsc52Clipboard",
            "WarnOnCloseWhenBusy",
            "CloseAction",
            "ClaudeLaunchStaggerMs",
            "AutoResumeClaude",
            "DefaultCommand",
            "LaunchCommands",
            "AnthropicApiKey",
            "StartMinimized",
            "CheckForUpdates",
            "ReopenRemoteWindows",
        ] {
            let r = h.engine.call(caller.clone(), "settings_set", json!({"key": key, "value": false}));
            match &r {
                Err(ToolError::Refused(m)) => assert!(m.contains("Settings in the app"), "{key}: {m}"),
                other => panic!("{key} for {caller:?}: {other:?}"),
            }
        }
    }
    // The person is unchanged.
    h.ok("settings_set", json!({"key": "OfferMcpToClaude", "value": true}));
    let cli = h.engine.call(Caller::Cli, "settings_set", json!({"key": "WarnOnCloseWhenBusy", "value": false}));
    assert!(cli.is_ok(), "{cli:?}");
}

/// Every tool that decides what runs, with a call that would do it. A paired device is refused
/// just like an MCP client (`Caller::is_external`).
#[test]
fn tools_that_decide_what_runs_refuse_a_paired_device_like_an_mcp_client() {
    let h = H::new();
    let abs = h.dir.path().join("x.csm").display().to_string();
    let calls: Vec<(&str, Value)> = vec![
        ("settings_set", json!({"key": "DefaultCommand", "value": "evil"})),
        ("open_with_set", json!({"entries": []})),
        ("addon_run_apply", json!({"template": "a.b", "session_id": "s1"})),
        ("state_import", json!({"path": abs})),
        ("state_import_wpf", json!({"path": abs})),
    ];
    for caller in [mcp(), peer()] {
        for (tool, params) in &calls {
            let r = h.engine.call(caller.clone(), tool, params.clone());
            assert!(refused(&r), "{tool} for {caller:?}: {r:?}");
        }
    }
}

#[test]
fn export_needs_a_csm_name_never_replaces_unasked_and_never_carries_secrets() {
    let h = H::new();
    h.ok("settings_set", json!({"key": "AnthropicApiKey", "value": "sk-ant-secret"}));
    let dir = h.dir.path().join("out");
    std::fs::create_dir_all(&dir).unwrap();

    let bad = h.ui("state_export", json!({"path": dir.join("notes.txt").display().to_string()}));
    assert!(matches!(bad, Err(ToolError::InvalidParams(_))), "{bad:?}");
    assert!(!dir.join("notes.txt").exists());

    let target = dir.join("a.csm");
    h.ok("state_export", json!({"path": target.display().to_string()}));
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(!text.contains("sk-ant-secret"), "the person's own export carries no API key either");

    std::fs::write(&target, "precious").unwrap();
    let again = h.ui("state_export", json!({"path": target.display().to_string()}));
    assert!(refused(&again), "{again:?}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "precious");
    h.ok("state_export", json!({"path": target.display().to_string(), "overwrite": true}));
    assert_ne!(std::fs::read_to_string(&target).unwrap(), "precious");
    assert_eq!(std::fs::read_to_string(target.with_extension("csm.bak")).unwrap(), "precious", "the old file is kept");

    let spec = h.engine.registry().get("state_export").copied().unwrap();
    assert!(spec.effects.contains(csm_engine::Effects::MAY_DISCARD_DATA));
}

#[test]
fn import_leaves_launch_settings_alone_until_asked() {
    let h = H::new();
    h.ok("settings_set", json!({"key": "DefaultCommand", "value": "pwsh"}));
    let file = h.dir.path().join("b.csm");
    let backup = json!({
        "Sessions": [{"Id": "s1", "Name": "box", "Command": "ssh", "SshConfigFile": "C:\\Users\\a\\cfg", "SshRemoteArgv": ["rm", "-rf", "x"],
                      "RunCommands": [{"Id": "r", "Label": "go", "CommandLine": "npm start"}]}],
        "Groups": [],
        "Settings": {"DefaultCommand": "evil", "LaunchCommands": ["evil"]},
    });
    std::fs::write(&file, backup.to_string()).unwrap();
    let path = file.display().to_string();

    let dry = h.ok("state_import", json!({"path": path, "dry_run": true}));
    assert_eq!(dry.data["launch_settings"]["differs"], true);
    assert_eq!(dry.data["launch_settings"]["default_command"], "evil");
    let details = dry.data["session_details"].to_string();
    assert!(details.contains("ssh config file") && details.contains("rm -rf x") && details.contains("npm start"), "{details}");

    h.ok("state_import", json!({"path": path, "force": true}));
    let settings = h.ok("settings_get", json!({}));
    assert_eq!(settings.data["DefaultCommand"], "pwsh", "not adopted without adopt_launch_settings");
    assert!(!settings.data["LaunchCommands"].to_string().contains("evil"));

    h.ok("state_import", json!({"path": path, "force": true, "adopt_launch_settings": true}));
    assert_eq!(h.ok("settings_get", json!({"key": "DefaultCommand"})).data["value"], "evil");
}

#[test]
fn import_refuses_a_network_or_relative_ssh_config_file() {
    let h = H::new();
    for bad in ["\\\\evil\\share\\config", "//evil/share/config", "config"] {
        let file = h.dir.path().join("c.csm");
        let backup = json!({"Sessions": [{"Id": "s1", "Name": "box", "SshConfigFile": bad}], "Groups": []});
        std::fs::write(&file, backup.to_string()).unwrap();
        let r = h.ui("state_import", json!({"path": file.display().to_string(), "dry_run": true}));
        assert!(refused(&r), "{bad}: {r:?}");
    }
}

#[test]
fn project_docs_for_an_mcp_client_must_name_a_session() {
    let h = H::new();
    let folder = h.dir.path().join("proj");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("README.md"), "hi").unwrap();
    let path = folder.display().to_string();
    for caller in [mcp(), peer()] {
        let r = h.engine.call(caller.clone(), "project_docs_list", json!({"path": path}));
        assert!(refused(&r), "{caller:?}: {r:?}");
        let r = h.engine.call(caller, "project_doc_get", json!({"path": path, "file": "README.md"}));
        assert!(refused(&r), "{r:?}");
    }
    // The person may still name a folder.
    assert!(h.ui("project_docs_list", json!({"path": path})).is_ok());
    // A network folder is refused for everyone.
    let r = h.ui("project_docs_list", json!({"path": "\\\\evil\\share\\repo"}));
    assert!(r.is_err(), "{r:?}");
}

fn clipboard_within(h: &H, ms: u64) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        if let Some(t) = h.engine.take_clipboard() {
            return Some(t);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

#[test]
fn an_osc52_copy_is_taken_from_the_focused_session_only_and_announced() {
    let h = H::new();
    let a = h.session("pwsh");
    let _b = h.session("pwsh");
    h.ok("session_focus", json!({"session_id": a}));
    let events = h.engine.subscribe();

    // The session in the background is ignored.
    h.pty(1).emit(b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!(clipboard_within(&h, 400), None, "an unfocused session may not set the clipboard");

    // The focused one is accepted, and a toast says who did it.
    h.pty(0).emit(b"\x1b]52;c;aGVsbG8=\x07");
    assert_eq!(clipboard_within(&h, 3000).as_deref(), Some("hello"));
    let mut toast = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while toast.is_none() && std::time::Instant::now() < deadline {
        while let Ok(e) = events.try_recv() {
            if let csm_engine::Event::Toast { message } = e
                && message.starts_with("Copied to the clipboard by ")
            {
                toast = Some(message);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(toast.is_some(), "a toast names the session that copied");
}

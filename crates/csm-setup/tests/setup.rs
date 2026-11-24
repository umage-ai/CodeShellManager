//! Setup, doctor and uninstall against temp homes only: the real configs are never touched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;

use csm_setup::{ActionKind, Host, Outcome, PlanRequest, RemoveOutcome, SERVER_NAME, apply, clients, plan, uninstall};
use serde_json::{Value, json};

fn host() -> (tempfile::TempDir, Host) {
    let dir = tempfile::tempdir().unwrap();
    let mut h = Host::with_home(dir.path());
    // Pin the data folders inside the temp home on every OS.
    h.env.insert("APPDATA".into(), dir.path().join("AppData").to_string_lossy().into_owned());
    h.env.insert("XDG_CONFIG_HOME".into(), dir.path().join(".config").to_string_lossy().into_owned());
    (dir, h)
}

fn cmd() -> Vec<String> {
    vec![if cfg!(windows) { r"C:\tools\csm.exe".into() } else { "/usr/local/bin/csm".into() }, "mcp".into()]
}

fn request(ids: &[&str]) -> PlanRequest {
    PlanRequest { clients: ids.iter().map(|s| (*s).to_owned()).collect(), all_detected: false, command: cmd() }
}

fn config_of(h: &Host, id: &str) -> PathBuf {
    clients(h).into_iter().find(|c| c.id == id).unwrap().config
}

fn write(path: &PathBuf, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

const CLAUDE_JSON: &str = "// my settings\n{\n  /* keep me */\n  \"theme\": \"dark\", // trailing\n  \"mcpServers\": {\n    \"other\": {\"command\": \"x\", \"args\": []},\n  },\n}\n";

#[test]
fn dry_run_shows_the_change_and_writes_nothing() {
    let (_d, h) = host();
    let path = config_of(&h, "claude_code");
    write(&path, CLAUDE_JSON);
    let p = plan(&h, &request(&["claude_code"])).unwrap();
    assert_eq!(p.actions[0].kind, ActionKind::Add);
    let text = p.describe();
    assert!(text.contains("add the entry to Claude Code") && text.contains("+ ") && text.contains(SERVER_NAME), "{text}");
    assert_eq!(fs::read_to_string(&path).unwrap(), CLAUDE_JSON, "a plan never writes");
    let backups = fs::read_dir(path.parent().unwrap()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("csm-backup")).count();
    assert_eq!(backups, 0);
}

#[test]
fn jsonc_comments_survive_and_a_backup_is_kept() {
    let (_d, h) = host();
    let path = config_of(&h, "claude_code");
    write(&path, CLAUDE_JSON);
    let report = apply(&h, &plan(&h, &request(&["claude_code"])).unwrap());
    assert_eq!(report.files[0].outcome, Outcome::Written, "{report:?}");
    let after = fs::read_to_string(&path).unwrap();
    for keep in ["// my settings", "/* keep me */", "// trailing", "\"theme\": \"dark\"", "\"other\""] {
        assert!(after.contains(keep), "lost {keep:?} in:\n{after}");
    }
    let backup = report.files[0].backup.clone().unwrap();
    assert_eq!(fs::read_to_string(backup).unwrap(), CLAUDE_JSON);
    // Reading it back through setup's own parser (which tolerates the comments).
    let c = clients(&h).into_iter().find(|c| c.id == "claude_code").unwrap();
    let entry = csm_setup::config::read_entry(&c, &after).unwrap().unwrap();
    assert_eq!(entry["args"], json!(["mcp"]));
    assert_eq!(entry["type"], "stdio");
    assert!(csm_setup::config::read_entry(&c, &after).is_ok());
    let all = csm_setup::config::parse(&c, &after).unwrap();
    assert_eq!(all["mcpServers"]["other"]["command"], "x");
}

#[test]
fn a_missing_config_is_created_and_uninstall_removes_only_ours() {
    let (_d, h) = host();
    let path = config_of(&h, "cursor");
    assert!(!path.exists());
    let p = plan(&h, &request(&["cursor"])).unwrap();
    assert_eq!(p.actions[0].kind, ActionKind::Create);
    let report = apply(&h, &p);
    assert_eq!(report.files[0].outcome, Outcome::Written);
    assert!(report.files[0].backup.is_none(), "nothing to back up");
    let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["mcpServers"][SERVER_NAME]["command"], cmd()[0]);

    // Running it again changes nothing.
    assert_eq!(plan(&h, &request(&["cursor"])).unwrap().actions[0].kind, ActionKind::Unchanged);

    // The person adds another server, then we uninstall.
    let mut v = v;
    v["mcpServers"]["theirs"] = json!({"command": "y", "args": []});
    fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let dry = uninstall(&h, &[], true).unwrap();
    assert_eq!(dry.files.iter().find(|f| f.client == "cursor").unwrap().outcome, RemoveOutcome::WouldRemove);
    assert!(fs::read_to_string(&path).unwrap().contains(SERVER_NAME), "dry run removes nothing");
    let done = uninstall(&h, &[], false).unwrap();
    assert_eq!(done.files.iter().find(|f| f.client == "cursor").unwrap().outcome, RemoveOutcome::Removed);
    let after: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(after["mcpServers"].get(SERVER_NAME).is_none());
    assert_eq!(after["mcpServers"]["theirs"]["command"], "y");
}

#[test]
fn an_entry_we_did_not_write_is_blocked_and_left_alone() {
    let (_d, h) = host();
    let path = config_of(&h, "cursor");
    let theirs = format!("{{\"mcpServers\": {{\"{SERVER_NAME}\": {{\"command\": \"python\", \"args\": [\"mine.py\"]}}}}}}");
    write(&path, &theirs);
    let p = plan(&h, &request(&["cursor"])).unwrap();
    assert!(p.blocked && p.actions[0].kind == ActionKind::Blocked);
    let report = apply(&h, &p);
    assert!(matches!(report.files[0].outcome, Outcome::Skipped(_)));
    assert_eq!(fs::read_to_string(&path).unwrap(), theirs);
    let un = uninstall(&h, &["cursor".into()], false).unwrap();
    assert!(matches!(un.files[0].outcome, RemoveOutcome::LeftAlone(_)));
    assert_eq!(fs::read_to_string(&path).unwrap(), theirs);
}

#[test]
fn a_config_that_does_not_parse_is_never_overwritten() {
    let (_d, h) = host();
    let path = config_of(&h, "claude_desktop");
    write(&path, "{ this is not json");
    let p = plan(&h, &request(&["claude_desktop"])).unwrap();
    assert!(p.blocked);
    assert!(p.actions[0].reason.as_deref().unwrap().contains("not valid"));
    let report = apply(&h, &p);
    assert!(matches!(report.files[0].outcome, Outcome::Skipped(_)));
    assert_eq!(fs::read_to_string(&path).unwrap(), "{ this is not json");
}

#[test]
fn codex_toml_keeps_comments_and_other_tables() {
    let (_d, h) = host();
    let path = config_of(&h, "codex");
    let original = "# my codex config\nmodel = \"gpt\"\n\n[mcp_servers.other]\ncommand = \"x\"  # keep\nargs = []\n";
    write(&path, original);
    let report = apply(&h, &plan(&h, &request(&["codex"])).unwrap());
    assert_eq!(report.files[0].outcome, Outcome::Written, "{report:?}");
    let after = fs::read_to_string(&path).unwrap();
    for keep in ["# my codex config", "model = \"gpt\"", "[mcp_servers.other]", "# keep"] {
        assert!(after.contains(keep), "lost {keep:?} in:\n{after}");
    }
    assert!(after.contains(&format!("[mcp_servers.{SERVER_NAME}]")) && after.contains("args = [\"mcp\"]"), "{after}");
    let un = uninstall(&h, &["codex".into()], false).unwrap();
    assert_eq!(un.files[0].outcome, RemoveOutcome::Removed);
    let back = fs::read_to_string(&path).unwrap();
    assert!(!back.contains(SERVER_NAME) && back.contains("[mcp_servers.other]"), "{back}");
}

#[test]
fn claude_config_dir_moves_the_claude_code_file() {
    let (d, mut h) = host();
    let custom = d.path().join("elsewhere");
    h.env.insert("CLAUDE_CONFIG_DIR".into(), custom.to_string_lossy().into_owned());
    assert_eq!(config_of(&h, "claude_code"), custom.join(".claude.json"));
    let report = apply(&h, &plan(&h, &request(&["claude_code"])).unwrap());
    assert_eq!(report.files[0].outcome, Outcome::Written);
    assert!(custom.join(".claude.json").is_file());
    assert!(!d.path().join(".claude.json").exists(), "the default location is not touched");
}

#[test]
fn unknown_clients_are_an_error_and_detection_decides_the_default() {
    let (d, h) = host();
    assert!(plan(&h, &request(&["emacs"])).unwrap_err().contains("choose from"));
    assert!(plan(&h, &PlanRequest { command: cmd(), ..Default::default() }).unwrap().actions.is_empty(), "nothing is installed in an empty home");
    fs::create_dir_all(d.path().join(".cursor")).unwrap();
    let p = plan(&h, &PlanRequest { command: cmd(), ..Default::default() }).unwrap();
    assert_eq!(p.actions.len(), 1);
    assert_eq!(p.actions[0].client, "cursor");
}

#[test]
fn a_file_that_changed_since_the_plan_is_not_overwritten() {
    let (_d, h) = host();
    let path = config_of(&h, "cursor");
    write(&path, "{}");
    let p = plan(&h, &request(&["cursor"])).unwrap();
    fs::write(&path, "{\"mcpServers\": {}, \"added\": 1}").unwrap();
    let report = apply(&h, &p);
    assert!(matches!(report.files[0].outcome, Outcome::Failed(_)), "{report:?}");
    assert!(fs::read_to_string(&path).unwrap().contains("added"));
}

#[test]
fn verify_catches_a_stray_change() {
    let (_d, h) = host();
    let c = clients(&h).into_iter().find(|c| c.id == "cursor").unwrap();
    let entry = c.desired_entry(&cmd());
    let before = "{\"keep\": 1}";
    let good = csm_setup::config::set_entry(&c, before, &entry).unwrap();
    assert!(csm_setup::plan::verify(&c, Some(before), &good, &entry).is_ok());
    let tampered = good.replace("\"keep\": 1", "\"keep\": 2");
    assert!(csm_setup::plan::verify(&c, Some(before), &tampered, &entry).unwrap_err().contains("besides our entry"));
    let wrong = csm_setup::config::set_entry(&c, before, &json!({"command": "other", "args": []})).unwrap();
    assert!(csm_setup::plan::verify(&c, Some(before), &wrong, &entry).is_err());
}

#[test]
fn owned_entries_are_recognised_by_program_and_first_argument() {
    use csm_setup::is_owned_entry;
    assert!(is_owned_entry(&json!({"command": "C:\\x\\csm.exe", "args": ["mcp", "--session", "a"]})));
    assert!(is_owned_entry(&json!({"command": "/usr/bin/csm", "args": ["mcp"]})));
    assert!(!is_owned_entry(&json!({"command": "/usr/bin/csm", "args": ["tools"]})));
    assert!(!is_owned_entry(&json!({"command": "python", "args": ["mcp"]})));
    assert!(!is_owned_entry(&json!("csm mcp")));
}

#[test]
fn doctor_without_an_app_says_how_to_fix_each_problem() {
    let (_d, h) = host();
    let runtime = tempfile::tempdir().unwrap();
    let req = csm_setup::DoctorRequest {
        remote: csm_mcp::RemoteOptions { runtime: Some(csm_mcp::RuntimeDir::at(runtime.path().join("rt")).unwrap()), ..Default::default() },
        self_test: None,
        app_dir: Some(_d.path().to_path_buf()),
        check_signing: false,
    };
    let report = csm_setup::doctor(&h, &req);
    let app = report.checks.iter().find(|c| c.name == "app").unwrap();
    assert_eq!(app.status, csm_setup::CheckStatus::Warn);
    assert!(app.remedy.as_deref().unwrap().contains("--start"));
    for c in &report.checks {
        if matches!(c.status, csm_setup::CheckStatus::Warn | csm_setup::CheckStatus::Fail) {
            assert!(c.remedy.is_some(), "{} has no remedy", c.name);
        }
    }
    if cfg!(windows) {
        let conpty = report.checks.iter().find(|c| c.name == "conpty").unwrap();
        assert_eq!(conpty.status, csm_setup::CheckStatus::Warn, "{conpty:?}");
    }
    assert!(report.render().contains("fix:"));
}

#[test]
fn repair_moved_rewrites_only_our_entries_in_the_old_install_folder() {
    let (_d, h) = host();
    let new = vec![r"C:\Program Files\umage.ai\CodeShellManager\csm.exe".to_owned(), "mcp".to_owned()];
    let old = |dir: &str| {
        let dir = dir.replace('\\', "\\\\");
        format!("// keep\n{{\n  \"mcpServers\": {{\n    \"{SERVER_NAME}\": {{\"type\": \"stdio\", \"command\": \"{dir}\", \"args\": [\"mcp\"]}}\n  }}\n}}\n")
    };
    // Claude Code: ours, in the old folder (odd case and slashes). Cursor: ours, but hand-placed elsewhere.
    // Claude Desktop: an entry setup did not write, even though it names the old folder.
    let cc = config_of(&h, "claude_code");
    let cur = config_of(&h, "cursor");
    let cd = config_of(&h, "claude_desktop");
    write(&cc, &old(r"C:\program files\UMAGE\codeshellmanager\csm.exe"));
    write(&cur, &old(r"D:\tools\csm.exe"));
    let hand = old(r"C:\Program Files\umage\CodeShellManager\other.exe").replace("\"mcp\"", "\"serve\"");
    write(&cd, &hand);
    let cur_before = fs::read_to_string(&cur).unwrap();
    let r = csm_setup::repair_moved(&h, &new);
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].outcome, Outcome::Written, "{r:?}");
    assert!(r[0].backup.is_some(), "a backup is made, as setup does");
    let after = fs::read_to_string(&cc).unwrap();
    assert!(after.contains("// keep") && after.contains("umage.ai"), "{after}");
    assert_eq!(fs::read_to_string(&cur).unwrap(), cur_before, "a path outside the old folder is not ours to move");
    assert_eq!(fs::read_to_string(&cd).unwrap(), hand, "an entry setup did not write is left alone");
    // Nothing left to repair, and a client with no entry is never given one.
    assert!(csm_setup::repair_moved(&h, &new).is_empty());
}

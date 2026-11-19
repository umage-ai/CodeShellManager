//! `csm setup`, `csm doctor` and `csm uninstall` through the real binary, with the home folder and
//! every config location pointed at a temp directory. The user's own configs are never touched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn csm(home: &Path, runtime: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_csm"));
    c.args(args)
        .env("USERPROFILE", home)
        .env("HOME", home)
        .env("APPDATA", home.join("AppData"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("CSM_RUNTIME_DIR", runtime)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("CSM_APP");
    c.output().unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)
}

#[test]
fn setup_dry_run_then_apply_then_uninstall() {
    let home = private_tempdir();
    let rt = private_tempdir();
    std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
    let cfg = home.path().join(".cursor").join("mcp.json");

    let dry = csm(home.path(), rt.path(), &["setup", "--dry-run"]);
    assert!(dry.status.success(), "{}", text(&dry));
    let t = text(&dry);
    assert!(t.contains("create Cursor") && t.contains("codeshell") && t.contains("Dry run"), "{t}");
    assert!(!cfg.exists(), "a dry run writes nothing");

    let done = csm(home.path(), rt.path(), &["setup", "--client", "cursor"]);
    assert!(done.status.success(), "{}", text(&done));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert_eq!(v["mcpServers"]["codeshell"]["args"][0], "mcp");

    let json = csm(home.path(), rt.path(), &["setup", "--client", "cursor", "--dry-run", "--json"]);
    let j: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(j["plan"]["actions"][0]["kind"], "unchanged");

    let un = csm(home.path(), rt.path(), &["uninstall"]);
    assert!(un.status.success() && text(&un).contains("removed our entry"), "{}", text(&un));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    assert!(v["mcpServers"].get("codeshell").is_none());
}

#[test]
fn setup_with_nothing_installed_says_so() {
    let home = private_tempdir();
    let rt = private_tempdir();
    let o = csm(home.path(), rt.path(), &["setup"]);
    assert!(o.status.success());
    assert!(text(&o).contains("No supported app found"), "{}", text(&o));
}

#[test]
fn doctor_self_tests_the_mcp_server_and_every_problem_has_a_fix() {
    let home = private_tempdir();
    let rt = private_tempdir();
    let o = csm(home.path(), rt.path(), &["doctor", "--skip-signature", "--json"]);
    let j: Value = serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o)));
    let checks = j["checks"].as_array().unwrap();
    let self_test = checks.iter().find(|c| c["name"] == "mcp:self-test").unwrap();
    assert_eq!(self_test["status"], "ok", "{self_test}");
    let app = checks.iter().find(|c| c["name"] == "app").unwrap();
    assert_eq!(app["status"], "warn");
    for c in checks {
        if c["status"] == "warn" || c["status"] == "fail" {
            assert!(c["remedy"].is_string(), "{c}");
        }
    }
}

/// A temp folder that craft-ipc accepts as a runtime dir: on Unix it must be private (0700);
/// `tempfile` creates 0755 under the usual umask.
fn private_tempdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

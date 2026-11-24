//! The phone remote's checks: the web app (`phone/`, plain JavaScript run by Node) and the end-to-end
//! test that runs it against the real relay and the add-on's host on 127.0.0.1 (`cargo xtask phone-e2e`).

use std::path::Path;
use std::process::Command;

/// The `npm` invocation for this platform. On Windows `npm` is a `.cmd` shim, which only `cmd` finds.
fn npm(args: &[&str], dir: &Path) -> bool {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg("npm");
        c
    } else {
        Command::new("npm")
    };
    match cmd.args(args).current_dir(dir).status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            println!("  npm {} exited with {s}", args.join(" "));
            false
        }
        Err(e) => {
            println!("  could not run npm: {e}");
            false
        }
    }
}

/// `npm test` and `npm run typecheck` in `phone/`, when the folder exists. Its dev dependencies must
/// be installed (`npm ci` in `phone/`).
pub fn web_checks() -> Result<Vec<String>, String> {
    let dir = crate::root().join("phone");
    if !dir.join("package.json").is_file() {
        return Ok(Vec::new());
    }
    let mut problems = Vec::new();
    if !dir.join("node_modules").is_dir() {
        return Err("phone/node_modules is missing: run `npm ci` in phone/ once".into());
    }
    if !npm(&["test"], &dir) {
        problems.push("npm test (phone/) failed".into());
    }
    if !npm(&["run", "typecheck"], &dir) {
        problems.push("npm run typecheck (phone/) failed".into());
    }
    Ok(problems)
}

/// `cargo test -p csm-phone-it --test e2e -- --ignored`: the end-to-end test. Needs Node 22 or later
/// (`CSM_NODE` names another binary). Returns whether it passed.
pub fn e2e() -> bool {
    println!("==== phone-e2e: the phone web app against the real relay and host (loopback only)");
    let status = Command::new("cargo")
        .args(["test", "-p", "csm-phone-it", "--test", "e2e", "--", "--ignored", "--nocapture"])
        .current_dir(crate::root())
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("ok   phone-e2e");
            true
        }
        Ok(_) => {
            println!("FAIL phone-e2e");
            false
        }
        Err(e) => {
            println!("FAIL phone-e2e: could not run cargo: {e}");
            false
        }
    }
}

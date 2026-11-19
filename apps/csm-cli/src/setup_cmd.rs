//! `csm setup`, `csm doctor`, `csm uninstall`: the thin command-line side of `csm-setup`.

use std::path::PathBuf;

use clap::ArgMatches;
use csm_mcp::{RemoteOptions, RuntimeDir};
use csm_setup::{ActionKind, DoctorRequest, Host, Outcome, PlanRequest, RemoveOutcome};
use serde_json::json;

use crate::run::{EXIT_APP, EXIT_OK, EXIT_TOOL, EXIT_USAGE, Env};

fn ids(m: &ArgMatches) -> Vec<String> {
    m.get_many::<String>("client").map(|v| v.cloned().collect()).unwrap_or_default()
}

pub(crate) fn run_setup(m: &ArgMatches) -> i32 {
    let host = Host::current();
    let command = match m.get_one::<String>("command") {
        Some(program) => vec![program.clone(), "mcp".to_owned()],
        None => match csm_setup::default_command() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                return EXIT_USAGE;
            }
        },
    };
    let req = PlanRequest { clients: ids(m), all_detected: false, command };
    let plan = match csm_setup::plan(&host, &req) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    let json_out = m.get_flag("json");
    if plan.actions.is_empty() {
        if json_out {
            println!("{}", json!({"plan": plan, "applied": false}));
        } else {
            println!("No supported app found (Claude Code, Claude Desktop, Codex, Cursor). Name one with --client <id> to set it up anyway.");
        }
        return EXIT_OK;
    }
    if m.get_flag("dry_run") {
        if json_out {
            println!("{}", json!({"plan": plan, "applied": false}));
        } else {
            print!("{}", plan.describe());
            println!("Dry run: nothing was changed. Run without --dry-run to apply {} change(s).", plan.change_count());
        }
        return EXIT_OK;
    }
    let report = csm_setup::apply(&host, &plan);
    if json_out {
        println!("{}", json!({"plan": plan, "applied": true, "report": report}));
    } else {
        for (a, f) in plan.actions.iter().zip(&report.files) {
            let line = match &f.outcome {
                Outcome::Written => format!("connected {} ({})", a.display, f.config.display()),
                Outcome::Unchanged => format!("{} was already connected", a.display),
                Outcome::Skipped(why) => format!("skipped {}: {why}", a.display),
                Outcome::RolledBack(why) => format!("FAILED {}: {why}. The file was restored to what it was.", a.display),
                Outcome::Failed(why) => format!("FAILED {}: {why}", a.display),
            };
            println!("{line}");
            if let Some(b) = &f.backup {
                println!("  backup: {}", b.display());
            }
            if a.kind == ActionKind::Blocked {
                println!("  nothing was written for it");
            }
        }
        println!("Run `csm doctor` to check the connection.");
    }
    if report.ok() { EXIT_OK } else { EXIT_TOOL }
}

pub(crate) fn run_uninstall(m: &ArgMatches) -> i32 {
    let host = Host::current();
    let report = match csm_setup::uninstall(&host, &ids(m), m.get_flag("dry_run")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return EXIT_USAGE;
        }
    };
    if m.get_flag("json") {
        println!("{}", json!({"report": report}));
    } else {
        for f in &report.files {
            let what = match &f.outcome {
                RemoveOutcome::Removed => "removed our entry".to_owned(),
                RemoveOutcome::WouldRemove => "would remove our entry".to_owned(),
                RemoveOutcome::NotPresent => continue,
                RemoveOutcome::LeftAlone(why) => format!("left alone: {why}"),
                RemoveOutcome::Failed(why) => format!("FAILED: {why}"),
            };
            println!("{}: {what} ({})", f.client, f.config.display());
            if let Some(b) = &f.backup {
                println!("  backup: {}", b.display());
            }
        }
        if report.files.iter().all(|f| f.outcome == RemoveOutcome::NotPresent) {
            println!("Nothing to remove: no entry added by setup was found.");
        }
    }
    if report.files.iter().any(|f| matches!(f.outcome, RemoveOutcome::Failed(_))) { EXIT_TOOL } else { EXIT_OK }
}

pub(crate) fn run_doctor(m: &ArgMatches, env: &Env) -> i32 {
    let host = Host::current();
    let runtime = match &env.runtime_dir {
        Some(p) => match RuntimeDir::at(p) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("error: cannot use the runtime directory {}: {e}", p.display());
                return EXIT_APP;
            }
        },
        None => None,
    };
    let exe = std::env::current_exe().ok();
    let self_test = (!m.get_flag("skip_self_test")).then(|| exe.as_ref().map(|e| vec![e.to_string_lossy().into_owned(), "mcp".into(), "--in-process".into()])).flatten();
    let req = DoctorRequest {
        remote: RemoteOptions { runtime, app_path: env.app.clone(), ..Default::default() },
        self_test,
        app_dir: env.app.as_ref().and_then(|a| a.parent().map(PathBuf::from)),
        check_signing: !m.get_flag("skip_signature"),
    };
    let report = csm_setup::doctor(&host, &req);
    if m.get_flag("json") {
        println!("{}", json!({"ok": report.ok(), "checks": report.checks}));
    } else {
        print!("{}", report.render());
        println!("{}", if report.ok() { "No failures." } else { "Something failed: see the fix lines above." });
    }
    if report.ok() { EXIT_OK } else { EXIT_TOOL }
}

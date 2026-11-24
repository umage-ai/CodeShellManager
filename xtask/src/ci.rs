//! `cargo xtask ci`: everything CI runs, in one command, so a green local run means a green CI run.

use std::process::Command;

use crate::{Findings, layers, phone, policy, secrets, tools_doc};

fn cargo(name: &str, args: &[&str]) -> bool {
    println!("==== {name}: cargo {}", args.join(" "));
    match Command::new("cargo").args(args).current_dir(crate::root()).status() {
        Ok(s) if s.success() => {
            println!("ok   {name}");
            true
        }
        Ok(_) => {
            println!("FAIL {name}");
            false
        }
        Err(e) => {
            println!("FAIL {name}: could not run cargo: {e}");
            false
        }
    }
}

fn quick(name: &str, result: Result<Findings, String>) -> bool {
    match result {
        Ok(f) if f.is_empty() => {
            println!("ok   {name}");
            true
        }
        Ok(f) => {
            println!("FAIL {name}");
            for line in f {
                println!("  {line}");
            }
            false
        }
        Err(e) => {
            println!("FAIL {name}: {e}");
            false
        }
    }
}

/// `skip_tests` leaves out `cargo test`; `skip_cargo` leaves out clippy and the tests (CI runs them
/// as separate steps and only wants the static checks here).
pub fn run(skip_tests: bool, skip_cargo: bool) -> bool {
    let mut results = vec![
        quick("layers", layers::check()),
        quick("policy", policy::check()),
        quick("tools --check", tools_doc::run(true)),
        quick("secrets", secrets::check(false)),
    ];
    match phone::web_checks() {
        Ok(problems) if problems.is_empty() => {
            if crate::root().join("phone").is_dir() {
                println!("ok   phone web app (npm test, typecheck)");
            }
            results.push(true);
        }
        Ok(problems) => {
            println!("FAIL phone web app");
            for p in problems {
                println!("  {p}");
            }
            results.push(false);
        }
        Err(e) => {
            println!("FAIL phone web app: {e}");
            results.push(false);
        }
    }
    if !skip_cargo {
        results.push(cargo("clippy", &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]));
        if !skip_tests {
            results.push(cargo("test", &["test", "--workspace"]));
        }
    }
    let failed = results.iter().filter(|ok| !**ok).count();
    println!("{}", if failed == 0 { "ci: all checks passed".to_owned() } else { format!("ci: {failed} check(s) failed") });
    failed == 0
}

//! `cargo xtask <command>`: the repository's own checks.
//!
//! | command | what |
//! |---|---|
//! | `layers` | crate dependencies against the L0-L6 map in `docs/plan.md` |
//! | `policy` | source rules (`syn`): processes, UI-thread blocking, `unsafe`, crate-root attributes |
//! | `tools [--check]` | regenerate (or verify) the catalogue table in `docs/tools.md` |
//! | `secrets [--staged]` | token patterns in tracked (or staged) files |
//! | `mcp-size` | bytes and estimated tokens of the MCP tool definitions per toolset |
//! | `ci [--skip-tests] [--skip-cargo]` | all of the above, plus `cargo test --workspace` and `cargo clippy -D warnings` |
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod ci;
mod layers;
mod mcp_size;
mod policy;
mod secrets;
mod tools_doc;

use std::path::PathBuf;
use std::process::ExitCode;

/// The repository root (the folder holding the workspace `Cargo.toml`).
pub(crate) fn root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<root>/xtask` when run through the alias.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().map(PathBuf::from).unwrap_or_default()
}

/// A check's outcome: problems, one per line, empty when the check passes.
pub(crate) type Findings = Vec<String>;

fn report(name: &str, result: Result<Findings, String>) -> bool {
    match result {
        Ok(f) if f.is_empty() => {
            println!("ok   {name}");
            true
        }
        Ok(f) => {
            println!("FAIL {name}: {} problem(s)", f.len());
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let ok = match args.first().map(String::as_str) {
        Some("layers") => report("layers", layers::check()),
        Some("policy") => report("policy", policy::check()),
        Some("tools") => report("tools", tools_doc::run(flag("--check"))),
        Some("secrets") => report("secrets", secrets::check(flag("--staged"))),
        Some("mcp-size") => match mcp_size::run(flag("--markdown")) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("error: {e}");
                false
            }
        },
        Some("ci") => ci::run(flag("--skip-tests"), flag("--skip-cargo")),
        _ => {
            eprintln!(
                "usage: cargo xtask <layers | policy | tools [--check] | secrets [--staged] | mcp-size [--markdown] | ci [--skip-tests] [--skip-cargo]>"
            );
            return ExitCode::from(2);
        }
    };
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

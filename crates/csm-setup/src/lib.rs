//! Connect CodeShellManager to coding-agent apps: detect them, plan the change, merge a
//! `codeshell` MCP entry (`csm mcp`) into their config without disturbing anything else (comments
//! included), back up, verify, roll back on failure; check the install (`doctor`); remove only
//! what setup added (`uninstall`).
//!
//! After craft-anything's `crates/setup` (MIT OR Apache-2.0, storytold), cut down to the clients
//! CodeShellManager's people use: Claude Code, Claude Desktop, Codex and Cursor.
//!
//! Everything is a function of a [`Host`] (a home folder and a few environment values), so tests
//! run against a temp directory and never touch the real configs.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod clients;
pub mod config;
pub mod doctor;
#[allow(dead_code)]
mod jsonc;
pub mod plan;

pub use clients::{Client, Format, Host, Os, SERVER_NAME, Shape, clients, is_moved_entry, is_owned_entry};
pub use doctor::{Check, CheckStatus, DoctorReport, DoctorRequest, doctor, mcp_self_test};
pub use plan::{
    Action, ActionKind, ApplyReport, FileResult, Outcome, Plan, PlanRequest, RemoveOutcome, UninstallReport, UninstallResult, apply, default_command,
    plan, repair_moved, repair_moved_when_idle, uninstall,
};

/// Ids of the clients detected on this host.
pub fn detected(host: &Host) -> Vec<&'static str> {
    clients(host).into_iter().filter(|c| c.detected()).map(|c| c.id).collect()
}

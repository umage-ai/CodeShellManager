//! `csm`: every registered tool as a command, plus `csm mcp`. The library half exists so the
//! argument mapping and the runner can be tested without spawning the binary.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod args;
mod run;
mod setup_cmd;

pub use run::{EXIT_APP, EXIT_OK, EXIT_TOOL, EXIT_USAGE, Env, run};

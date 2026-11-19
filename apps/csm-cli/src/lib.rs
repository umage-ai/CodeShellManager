//! `csm`: every registered tool as a command, plus `csm mcp`. The library half exists so the
//! argument mapping and the runner can be tested without spawning the binary.
#![forbid(unsafe_code)]

pub mod args;
mod run;

pub use run::{EXIT_APP, EXIT_OK, EXIT_TOOL, EXIT_USAGE, Env, run};

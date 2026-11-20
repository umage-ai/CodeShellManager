//! `csm-core`: the pure logic of CodeShellManager.
//!
//! Nothing here touches the disk, a process, the clock or a UI toolkit. Every function that
//! needs "now" takes it as an argument (milliseconds since the Unix epoch, UTC), so the
//! behaviour is deterministic and testable. Every value that came from a terminal, a state
//! file or an import is hostile input: nothing in this crate panics on it.
//!
//! Ported from the C# `Services/*.cs` and `Models/*.cs` of the WPF app; the C# unit tests are
//! ported alongside as Rust tests and are the specification. Where the C# behaviour is
//! surprising it is kept and marked with a `C# parity` comment.
#![forbid(unsafe_code)]
// Tests may unwrap (repo rule); production code may not.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod alert_detector;
pub mod bulk_restart;
pub mod busy_rules;
pub mod color;
pub mod look;
pub mod models;
pub mod notify_rules;
pub mod probe_gate;
pub mod program_status_payload;
pub mod program_status_records;
pub mod recent_sessions;
pub mod session_manager;
pub mod session_names;
pub mod session_order;
pub mod session_sort;
pub mod shell_integration;
pub mod sidebar_drop;
pub mod sidebar_selection;
pub mod state;
pub mod time;
pub mod window_placement;

mod text;

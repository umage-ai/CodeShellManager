//! Owned wire types of the CodeShellManager peer protocol (`docs/design/remote-csm.md` 2.2).
//!
//! The engine's read model (`csm_engine::Snapshot`, `SessionView`, `Event`) is serialise-only and
//! borrows `&'static str` for its labels. A client on another machine has to read the same data
//! back, so this crate holds owned mirrors of those types that derive both `Serialize` and
//! `Deserialize`. The conversions from the engine types live in `csm-engine`, which may depend on
//! this crate (it sits at L0, below the engine). This crate depends on nothing in the workspace:
//! it is the pure bottom of the layer map.
//!
//! Field names and serde shapes match the engine's own JSON, so a value serialised by the engine
//! reads back here unchanged. Engine data that is not on the wire yet (add-on summaries, layouts,
//! window slots, the inspector tab, the shutdown progress and the look of a session) is left out
//! on purpose; each comes with its own owned mirror when a client needs it.
//!
//! Compatibility: adding a field or an event variant is additive and keeps [`WIRE_VERSION`]. A
//! field that changes meaning bumps it. Consumers must ignore events they do not know.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod event;
mod snapshot;

pub use event::Event;
pub use snapshot::{ClosedView, GroupInfo, GroupView, OpenWithEntry, OpenWithTarget, RunPhase, RunView, SessionView, Snapshot, Status};

/// The version of this wire format. Bumped when a field or variant changes meaning; additive
/// changes keep it.
pub const WIRE_VERSION: u32 = 1;

//! `csm-ui`: the CodeShellManager desktop UI in egui.
//!
//! - [`theme`]: Catppuccin Mocha (dark, default) and Latte (light) as tokens, and the one
//!   spacing / size / type scale. Screens never write a colour or a size as a number.
//! - [`terminal`]: the terminal widget (renderer, input, sources), moved from the Phase 0 spike.
//! - [`model`]: the plain-data snapshot the screens draw ([`model::UiModel`]).
//! - [`backend`]: the seam to whatever owns the state ([`backend::Backend`], [`backend::Dispatcher`]);
//!   every action is `engine.submit(Caller::Ui, "<tool_id>", params)`.
//! - [`demo`]: a backend with the mock-up's data and live shells, until the engine owns sessions.
//! - [`app`] and [`screens`]: the window.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod app;
pub mod backend;
pub mod demo;
mod icon_data;
pub mod icons;
pub mod model;
pub mod screens;
pub mod terminal;
pub mod theme;
pub mod widgets;

pub use app::{Action, CsmApp};
pub use backend::{Backend, Call, Dispatcher, Notice};

/// Starts an engine with the demo tools registered (the ids of `docs/tools.md`).
pub fn demo_engine() -> Result<csm_engine::Engine, csm_engine::ToolError> {
    csm_engine::Engine::start(csm_engine::State::default(), demo::tools::tools())
}

//! Phase 0 terminal spike (throwaway). The parts meant to survive are `csm-pty`, `csm-term` and
//! the renderer, which now lives in `csm-ui::terminal`; this crate is the gate harness around it.

#![forbid(unsafe_code)]

pub mod app;

pub mod logging;
pub mod pane;
pub mod perf;

pub mod theme;
pub mod view;

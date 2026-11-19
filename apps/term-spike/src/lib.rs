//! Phase 0 terminal spike (throwaway). The parts meant to survive are `csm-pty` and `csm-term`;
//! the renderer here is the prototype of `csm-ui`'s terminal widget.

#![forbid(unsafe_code)]

pub mod app;
pub mod atlas;
pub mod boxdraw;
pub mod fonts;
pub mod logging;
pub mod pane;
pub mod perf;
pub mod render;
pub mod theme;
pub mod view;

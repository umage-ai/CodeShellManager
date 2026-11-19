//! `csm-ui`: the CodeShellManager desktop UI in egui.
//!
//! - [`theme`]: Catppuccin Mocha (dark, default) and Latte (light) as tokens, and the one
//!   spacing / size / type scale. Screens never write a colour or a size as a number.
//! - [`terminal`]: the terminal widget (renderer, input, sources).
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod terminal;

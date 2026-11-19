//! CodeShellManager services, pure parts. See the final lib.rs for the module map.
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod argv;
pub mod winpath;

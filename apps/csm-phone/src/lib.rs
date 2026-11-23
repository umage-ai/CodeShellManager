//! The phone remote add-on, host side (design note: docs/design/phone-remote.md).
//!
//! Modules: [`config`] (the add-on settings), [`store`] (host key, device list, first-run rules),
//! [`policy`] (folders, names, launch limits, the in-flight cap), [`csm`] (the CSM calls this add-on
//! may make), [`ops`] (phone ops mapped onto those calls), [`pairing`] (the pairing slot) and [`host`]
//! (the protocol core: Noise channels, pairing, revoke). The relay socket lives in the program only.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod config;
pub mod csm;
pub mod host;
pub mod ops;
pub mod pairing;
pub mod policy;
pub mod relay_client;
pub mod store;

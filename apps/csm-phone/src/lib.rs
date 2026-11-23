//! The phone remote add-on, host side (design note: docs/design/phone-remote.md).
//!
//! Modules: [`config`] (the add-on settings), [`store`] (host key, secret, device list, first-run
//! rules), [`policy`] (folders, names, launch limits, the in-flight cap), [`csm`] (the CSM calls
//! this add-on may make), [`ops`] (phone ops mapped onto those calls), [`pairing`] (the pairing
//! slot), [`host`] (the protocol core: Noise channels, pairing, revoke), [`worker`] (the thread
//! that runs the ops and their CSM calls), [`relay_client`] (the outbound relay connection and the
//! person's commands), [`acl`] (owner-only access to the data files on Windows) and [`shell`] (the
//! stdio JSON-RPC shell that CSM starts).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod acl;
pub mod config;
pub mod csm;
pub mod host;
pub mod ops;
pub mod pairing;
pub mod policy;
pub mod push;
pub mod relay_client;
pub mod screens;
pub mod settings;
pub mod shell;
pub mod store;
pub mod tracker;
pub mod worker;

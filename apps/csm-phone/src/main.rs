//! The phone remote add-on program: the stdio JSON-RPC shell that CSM starts (see `shell`).
//! A console program the host starts with no window.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() {
    csm_phone::shell::run();
}

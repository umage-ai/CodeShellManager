//! The Review add-on's program: speaks JSON-RPC with CSM on stdin and stdout (see the library docs).
//! A console program the host starts with no window.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() {
    csm_review::host::run();
}

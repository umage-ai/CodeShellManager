//! The phone remote add-on program. The stdio shell that talks JSON-RPC with CSM is not built yet.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() {
    eprintln!("csm-phone: the add-on shell is not built yet; see docs/design/phone-remote.md");
    std::process::exit(2);
}

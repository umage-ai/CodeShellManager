#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

fn main() {
    csm_engine::diag::install_panic_hook(csm_engine::diag::logs_dir(None), "csm");
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let code = csm_cli::run(&argv, &csm_cli::Env::from_process());
    std::process::exit(code);
}

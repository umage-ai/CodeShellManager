//! `CodeShellManager.exe`: starts `codeshell.exe` beside it with the same arguments and exits
//! (see the library's docs). A GUI program: no console window opens with it.
#![windows_subsystem = "windows"]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args_os().skip(1).collect();
    let outcome = match std::env::current_exe() {
        Ok(exe) => csm_legacy_launcher::launch(&exe, args),
        Err(_) => csm_legacy_launcher::Outcome::NoFolder,
    };
    if let Some(msg) = outcome.message() {
        // Seen when a script redirects the launcher's output; the box is for everyone else.
        eprintln!("{msg}");
        show_error(&msg);
    }
    ExitCode::from(outcome.exit_code())
}

/// A plain modal message box (no parent window; the launcher has none).
#[cfg(windows)]
fn show_error(msg: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("CodeShellManager")
        .set_description(msg)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(not(windows))]
fn show_error(_msg: &str) {}

//! Test helper: writes each argument it was started with, NUL-separated, to stdout.
//! The tests spawn it with a raw Windows command line and compare what a real process
//! received with what our quoting intended. Rust's std parses the command line with the
//! MSVCRT/CommandLineToArgvW rules every ordinary Windows program uses.
#![forbid(unsafe_code)]

use std::io::Write;

fn main() {
    let mut out = std::io::stdout().lock();
    for arg in std::env::args_os().skip(1) {
        let text = arg.to_string_lossy();
        // Ignoring write errors: there is nobody to tell, and the test sees the short output.
        let _ = out.write_all(text.as_bytes());
        let _ = out.write_all(&[0]);
    }
}

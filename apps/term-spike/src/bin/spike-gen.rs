//! Streams terminal output forever for the stress test: true-colour gradients, box drawing,
//! wide chars and emoji, ligature-prone code, at `--rate` lines per second (0 = unlimited).

#![forbid(unsafe_code)]

use std::io::Write;
use std::time::{Duration, Instant};

fn main() {
    let mut rate: u64 = 2000;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--rate" {
            rate = args.next().and_then(|v| v.parse().ok()).unwrap_or(rate);
        }
    }
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, stdout.lock());
    let samples = [
        "fn main() -> Result<(), Error> { let x = a != b && c <= d || e >= f; x == y }",
        "╭─────────────╮ │ box drawing │ ╰─────────────╯ ┌┬┐├┼┤└┴┘ ═║╔╗╚╝ ▀▄█▌▐░▒▓",
        "wide: 日本語のテキスト 한국어 中文字符 emoji: 🚀 ✅ 🦀 🎉  arrows: -> => <- |> <=> ::",
        "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[9mstrike\x1b[0m \x1b[7minverse\x1b[0m \x1b[2mdim\x1b[0m",
    ];
    let start = Instant::now();
    let mut n: u64 = 0;
    loop {
        let batch = if rate == 0 { 200 } else { (rate / 100).max(1) };
        for _ in 0..batch {
            let _ = write!(out, "\x1b[38;5;244m{n:09}\x1b[0m ");
            for i in 0..24u64 {
                let t = (n * 3 + i * 10) % 256;
                let _ = write!(out, "\x1b[48;2;{};{};{}m ", t, (t * 7) % 256, 255 - t);
            }
            let _ = write!(out, "\x1b[0m \x1b[38;2;137;180;250m{}\x1b[0m\r\n", samples[(n % 4) as usize]);
            n += 1;
        }
        if out.flush().is_err() {
            return; // the terminal went away
        }
        if rate > 0 {
            // Pace against the clock so the average rate holds whatever the write latency.
            let due = start + Duration::from_secs_f64(n as f64 / rate as f64);
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
        }
    }
}

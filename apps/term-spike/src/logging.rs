//! A `log` backend that never blocks the caller: lines go over a channel to a writer thread
//! that appends to a file and echoes to stderr.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};
use std::time::SystemTime;

struct ChannelLogger {
    tx: Mutex<Sender<String>>,
    level: log::LevelFilter,
}

impl log::Log for ChannelLogger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= self.level && (m.target().starts_with("csm") || m.target().starts_with("term_spike") || m.level() <= log::Level::Warn)
    }

    fn log(&self, r: &log::Record<'_>) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let line = format!("[{t:.3}] {:5} {}: {}", r.level(), r.target(), r.args());
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(line);
        }
    }

    fn flush(&self) {}
}

pub fn init(path: PathBuf, level: log::LevelFilter) {
    let (tx, rx) = channel::<String>();
    let spawned = std::thread::Builder::new().name("log-writer".into()).spawn(move || {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
        for line in rx {
            eprintln!("{line}");
            if let Some(f) = file.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    });
    if spawned.is_err() {
        return;
    }
    let logger = ChannelLogger { tx: Mutex::new(tx), level };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(level);
    }
}

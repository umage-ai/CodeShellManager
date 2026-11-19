//! `wsl.exe` probes (`WslDiscoveryService`): the distro list, a distro's `$HOME` and its login
//! shell. Each runs on the calling thread and takes at most [`PROBE_TIMEOUT`], because `wsl.exe`
//! can boot a stopped distro VM. Callers are workers (`launch::resolve`, the `wsl_distros` tool's
//! worker); never the engine or UI thread. Nothing here spawns anything off Windows.
//!
//! Every value that reaches the command line is an argv element and the program is exec'd
//! directly (`-e`): no shell sees the distro or user name. A name that begins with `-` is refused
//! (wsl.exe would read it as an option). The script text is fixed.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use csm_services::wsl::{WslDistro, decode_wsl_output, parse_distros};

use crate::proc;

/// The cap on any one probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

type Key = (String, String);

static HOMES: Mutex<Option<HashMap<Key, String>>> = Mutex::new(None);
static SHELLS: Mutex<Option<HashMap<Key, String>>> = Mutex::new(None);

fn key(distro: &str, user: &str) -> Key {
    (distro.trim().to_lowercase(), user.trim().to_lowercase())
}

/// Distro and user names wsl.exe takes as arguments: not empty (for the distro), no option-like
/// start, no control characters.
fn name_ok(s: &str) -> bool {
    !s.starts_with('-') && !s.chars().any(char::is_control)
}

/// `wsl.exe` arguments for a fixed `sh -c <script>` probe in `distro` (as `user` when not blank).
pub fn probe_argv(distro: &str, user: &str, script: &str) -> Option<Vec<String>> {
    let (distro, user) = (distro.trim(), user.trim());
    if distro.is_empty() || !name_ok(distro) || !name_ok(user) {
        return None;
    }
    let mut v = vec!["-d".to_owned(), distro.to_owned()];
    if !user.is_empty() {
        v.extend(["-u".to_owned(), user.to_owned()]);
    }
    v.extend(["-e", "sh", "-c", script].map(str::to_owned));
    Some(v)
}

/// A `$HOME` we are willing to build a path from: absolute, one line, no control characters.
pub fn parse_home(stdout: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
    (line.starts_with('/') && !line.chars().any(char::is_control)).then(|| line.to_owned())
}

/// `bash` when the probe said so, else `sh` when it said that; `bash` for anything else (the
/// behaviour before the probe existed, so an odd answer never downgrades a working distro).
pub fn parse_shell(stdout: &str) -> &'static str {
    match stdout.trim() {
        "sh" => "sh",
        _ => "bash",
    }
}

/// The installed distros (Docker's internal ones filtered, default first). Empty when WSL is not
/// installed, not on Windows, or `wsl.exe` did not answer in time.
pub fn distros() -> Vec<WslDistro> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Some(out) = proc::capture("wsl.exe", ["-l", "-v"], PROBE_TIMEOUT).filter(|c| c.success) else { return Vec::new() };
    parse_distros(&decode_wsl_output(&out.stdout))
}

/// The distro's `$HOME` for `user` (cached per distro and user; only successes are cached, so a
/// distro that was still booting is asked again).
pub fn home(distro: &str, user: &str) -> Option<String> {
    cached(&HOMES, distro, user, |argv| proc_stdout(argv).and_then(|s| parse_home(&s)), "cd ~ && pwd")
}

/// `bash` or `sh`, whichever the distro can run as a login shell (cached; `bash` on any failure).
pub fn login_shell(distro: &str, user: &str) -> String {
    cached(&SHELLS, distro, user, |argv| proc_stdout(argv).map(|s| parse_shell(&s).to_owned()), "command -v bash >/dev/null 2>&1 && echo bash || echo sh")
        .unwrap_or_else(|| "bash".to_owned())
}

fn proc_stdout(argv: &[String]) -> Option<String> {
    proc::capture_ok("wsl.exe", argv, PROBE_TIMEOUT)
}

fn cached(cache: &Mutex<Option<HashMap<Key, String>>>, distro: &str, user: &str, run: impl FnOnce(&[String]) -> Option<String>, script: &str) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let k = key(distro, user);
    if let Some(v) = cache.lock().unwrap_or_else(PoisonError::into_inner).as_ref().and_then(|m| m.get(&k)) {
        return Some(v.clone());
    }
    let argv = probe_argv(distro, user, script)?;
    let value = run(&argv)?;
    cache.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(HashMap::new).insert(k, value.clone());
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_arguments_are_argv_and_refuse_option_like_names() {
        let v = probe_argv("Ubuntu", "", "cd ~ && pwd").unwrap();
        assert_eq!(v, ["-d", "Ubuntu", "-e", "sh", "-c", "cd ~ && pwd"]);
        let v = probe_argv(" Ubuntu-22.04 ", "alice", "x").unwrap();
        assert_eq!(&v[..4], ["-d", "Ubuntu-22.04", "-u", "alice"]);
        assert!(probe_argv("-x", "", "x").is_none());
        assert!(probe_argv("Ubuntu", "--exec", "x").is_none());
        assert!(probe_argv("", "", "x").is_none());
        assert!(probe_argv("a\nb", "", "x").is_none());
        // A hostile name stays one element.
        let v = probe_argv("a; calc", "$(id)", "x").unwrap();
        assert!(v.contains(&"a; calc".to_owned()) && v.contains(&"$(id)".to_owned()));
    }

    #[test]
    fn home_output_is_validated() {
        assert_eq!(parse_home("/home/alice\n").as_deref(), Some("/home/alice"));
        assert_eq!(parse_home("\n  /root \r\n").as_deref(), Some("/root"));
        assert_eq!(parse_home("not a path"), None);
        assert_eq!(parse_home(""), None);
        assert_eq!(parse_home("/home/a\u{1b}[0m"), None);
    }

    #[test]
    fn shell_defaults_to_bash() {
        assert_eq!(parse_shell("sh\n"), "sh");
        assert_eq!(parse_shell("bash\n"), "bash");
        assert_eq!(parse_shell("garbage"), "bash");
        assert_eq!(parse_shell(""), "bash");
    }

    #[test]
    fn the_cache_remembers_successes_only() {
        if !cfg!(windows) {
            return;
        }
        let cache = Mutex::new(None);
        let mut calls = 0;
        assert_eq!(cached(&cache, "D", "", |_| { calls += 1; None }, "s"), None);
        assert_eq!(cached(&cache, "D", "", |_| { calls += 1; Some("/home/x".into()) }, "s").as_deref(), Some("/home/x"));
        assert_eq!(cached(&cache, "d", " ", |_| { calls += 1; Some("other".into()) }, "s").as_deref(), Some("/home/x"));
        assert_eq!(calls, 2);
    }
}

//! The release channel this build belongs to: **stable** (rc and final releases, and every
//! local or dev build) or **beta** (`-beta.N` tags).
//!
//! It is fixed at compile time by the `CSM_CHANNEL` environment variable (`beta` or `stable`;
//! unset or anything else is stable), which `release-v2.yml` sets for beta builds. Cargo tracks
//! the variable (`option_env!`), so changing it rebuilds this crate.
//!
//! The channel decides the things two installed apps must not share (docs/release.md
//! "Channels"): the data folders (`%APPDATA%\CodeShellManager2` and
//! `%LOCALAPPDATA%\CodeShellManager2` for stable, `…\CodeShellManager2-Beta` for beta) and the
//! IPC app id (`codeshell` / `codeshell-beta`), which names the discovery records and the
//! Windows pipe (`craft-<user>-<app id>-<pid>`), so a beta `csm` only ever talks to a beta app.

/// A release channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Beta,
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if !a[i].eq_ignore_ascii_case(&b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

impl Channel {
    /// The channel a `CSM_CHANNEL` value names: `beta` (any case) is beta, anything else stable.
    pub const fn from_env(v: Option<&str>) -> Channel {
        match v {
            Some(s) if same(s, "beta") => Channel::Beta,
            _ => Channel::Stable,
        }
    }

    /// This build's channel.
    pub const fn current() -> Channel {
        CURRENT
    }

    pub const fn id(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }

    /// The data folder's name under the platform's roaming and local data folders.
    pub const fn app_dir(self) -> &'static str {
        match self {
            Channel::Stable => STABLE_APP_DIR,
            Channel::Beta => BETA_APP_DIR,
        }
    }

    /// The craft-ipc app id: discovery records and the pipe name carry it.
    pub const fn ipc_app_id(self) -> &'static str {
        match self {
            Channel::Stable => "codeshell",
            Channel::Beta => "codeshell-beta",
        }
    }

    /// The product name people see (installer, About).
    pub const fn product_name(self) -> &'static str {
        match self {
            Channel::Stable => "CodeShellManager",
            Channel::Beta => "CodeShellManager Beta",
        }
    }
}

/// The stable channel's data folder name.
pub const STABLE_APP_DIR: &str = "CodeShellManager2";
/// The beta channel's data folder name.
pub const BETA_APP_DIR: &str = "CodeShellManager2-Beta";

/// This build's channel (see the module docs).
pub const CURRENT: Channel = Channel::from_env(option_env!("CSM_CHANNEL"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_beta_means_beta() {
        assert_eq!(Channel::from_env(Some("beta")), Channel::Beta);
        assert_eq!(Channel::from_env(Some("BETA")), Channel::Beta);
        for v in [None, Some(""), Some("stable"), Some("dev"), Some("betax"), Some("bet")] {
            assert_eq!(Channel::from_env(v), Channel::Stable, "{v:?}");
        }
    }

    #[test]
    fn the_channels_share_nothing_they_must_not() {
        assert_eq!(Channel::Stable.app_dir(), "CodeShellManager2");
        assert_eq!(Channel::Beta.app_dir(), "CodeShellManager2-Beta");
        assert_ne!(Channel::Stable.ipc_app_id(), Channel::Beta.ipc_app_id());
        // craft-ipc app ids: ^[a-z][a-z0-9-]{1,31}$
        for c in [Channel::Stable, Channel::Beta] {
            let id = c.ipc_app_id();
            assert!(id.len() <= 32 && id.starts_with(|ch: char| ch.is_ascii_lowercase()));
            assert!(id.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-'));
        }
    }
}

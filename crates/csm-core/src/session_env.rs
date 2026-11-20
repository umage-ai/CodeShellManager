//! The environment a session or run command starts with (issue #162).
//!
//! A child inherits the app's own environment, and the app's environment is whatever the
//! terminal that started it had. Started from inside a Claude Code session, that includes
//! `CLAUDECODE=1` and Claude Code's per-session markers, and a `claude` we then host thinks it
//! is nested inside another one (it refuses, or behaves differently). Variables that name the
//! *launching terminal's session* are removed; the user's own configuration (`PATH`,
//! `CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_GIT_BASH_PATH`, …) is kept.
//!
//! Pure: the caller passes the current variable names and applies the returned changes to the
//! spawn (`Some` sets, `None` removes), so the platform layer stays the only place that knows
//! how an environment block is built.

use std::ffi::{OsStr, OsString};

/// Exact names (compared case-insensitively) of variables that describe the terminal session
/// the app was started from. `TERM_PROGRAM`/`TERM_PROGRAM_VERSION` are not here: they are
/// replaced, not removed (see [`session_env_changes`]).
pub const STRIPPED: &[&str] = &[
    // Claude Code's "you are inside a Claude session" markers.
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_CHILD_NOTES_DIR",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_PID",
    // Other terminals' session ids.
    "WT_SESSION",
    "WT_PROFILE_ID",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
];

/// Prefixes (case-insensitive) of variable families that belong to the launching terminal:
/// VS Code's terminal integration (`VSCODE_INJECTION`, `VSCODE_GIT_IPC_HANDLE`, …).
pub const STRIPPED_PREFIXES: &[&str] = &["VSCODE_"];

/// The program name sessions see in `TERM_PROGRAM`.
pub const TERM_PROGRAM: &str = "CodeShellManager";

/// True when `name` is a launching-terminal marker that a session must not inherit.
#[must_use]
pub fn is_stripped(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    STRIPPED.contains(&upper.as_str()) || STRIPPED_PREFIXES.iter().any(|p| upper.starts_with(p))
}

/// The changes to apply on top of the current environment, whose variable names are `current`:
/// every stripped marker present is removed (`None`), then `TERM_PROGRAM`,
/// `TERM_PROGRAM_VERSION` (= `version`) and `CSM_ENV=1` are set. Names that are not valid
/// Unicode are never markers and are left alone.
pub fn session_env_changes<'a, I>(current: I, version: &str) -> Vec<(OsString, Option<OsString>)>
where
    I: IntoIterator<Item = &'a OsStr>,
{
    let mut out: Vec<(OsString, Option<OsString>)> =
        current.into_iter().filter(|k| k.to_str().is_some_and(is_stripped)).map(|k| (k.to_owned(), None)).collect();
    out.push(("TERM_PROGRAM".into(), Some(TERM_PROGRAM.into())));
    out.push(("TERM_PROGRAM_VERSION".into(), Some(version.into())));
    out.push(("CSM_ENV".into(), Some("1".into())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changes(names: &[&str]) -> Vec<(String, Option<String>)> {
        let owned: Vec<OsString> = names.iter().map(OsString::from).collect();
        session_env_changes(owned.iter().map(OsString::as_os_str), "2.0.0")
            .into_iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect()
    }

    #[test]
    fn claude_code_markers_are_removed_case_insensitively() {
        let c = changes(&["CLAUDECODE", "claude_code_entrypoint", "Claude_Code_Session_Id", "CLAUDE_PID", "WT_SESSION", "VSCODE_INJECTION"]);
        let removed: Vec<&str> = c.iter().filter(|(_, v)| v.is_none()).map(|(k, _)| k.as_str()).collect();
        assert_eq!(removed, ["CLAUDECODE", "claude_code_entrypoint", "Claude_Code_Session_Id", "CLAUDE_PID", "WT_SESSION", "VSCODE_INJECTION"]);
    }

    #[test]
    fn the_users_configuration_is_kept() {
        let c = changes(&[
            "PATH",
            "HOME",
            "CLAUDE_CONFIG_DIR",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_GIT_BASH_PATH",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            "ANTHROPIC_API_KEY",
            "CLAUDECODEX",
            "MY_VSCODE_THING",
        ]);
        assert!(c.iter().all(|(_, v)| v.is_some()), "{c:?}");
    }

    #[test]
    fn csm_markers_are_added_and_term_program_replaced() {
        let c = changes(&["TERM_PROGRAM", "TERM_PROGRAM_VERSION"]);
        assert_eq!(
            c,
            vec![
                ("TERM_PROGRAM".into(), Some("CodeShellManager".into())),
                ("TERM_PROGRAM_VERSION".into(), Some("2.0.0".into())),
                ("CSM_ENV".into(), Some("1".into())),
            ]
        );
    }

    #[test]
    fn non_unicode_names_are_left_alone() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let odd = OsString::from_vec(vec![0x43, 0xff, 0x4c]);
            let c = session_env_changes([odd.as_os_str()], "v");
            assert_eq!(c.len(), 3);
        }
        assert!(!is_stripped(""));
    }
}

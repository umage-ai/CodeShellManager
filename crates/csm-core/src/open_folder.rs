//! The folder Explorer hands to `codeshell.exe --open-folder "%V"` (the "Open in CodeShellManager"
//! right-click entry), cleaned and checked as far as text can tell. The caller still checks that it
//! is a directory that exists (that is a file-system read, so it is not done here).
//!
//! The text is untrusted: it came from a command line anyone can write. Explorer's own `%V` is a
//! plain path, with one catch: a drive root is `C:\`, and inside `"%V"` the trailing backslash
//! escapes the closing quote, so after argv parsing the value is `C:"`. That quote is removed
//! here. Accepted: an absolute local path or a UNC share path. Refused: shell and virtual paths
//! (`::{GUID}`, `shell:`), URLs, device paths (`\\?\`, `\\.\`), wildcards, `.` and `..`, control
//! and format characters, relative paths.

use std::path::{Component, Path, PathBuf};

use crate::program_status_payload::is_format_char;

/// Longest path accepted, in characters (Windows' own limit is far lower for most callers).
pub const MAX_LEN: usize = 4096;

const BACKSLASH: char = '\\';

/// Cleans the value of `--open-folder`. `Err` carries the reason, in words for a toast.
pub fn clean(raw: &str) -> Result<String, String> {
    // The drive-root artefact, and any quoting the shell left on: only at the ends.
    let s = raw.trim().trim_matches('"').trim();
    if s.is_empty() {
        return Err("no folder was given".into());
    }
    if s.chars().count() > MAX_LEN {
        return Err("the folder path is too long".into());
    }
    if s.chars().any(|c| c.is_control() || is_format_char(c)) {
        return Err("the folder path has control characters".into());
    }
    if s.contains('"') {
        return Err("the folder path has quotes in it".into());
    }
    let lower = s.to_lowercase();
    if lower.contains("::{") || lower.starts_with("shell:") {
        return Err("that is a virtual folder, not a folder on disk".into());
    }
    if lower.contains("://") || lower.starts_with("file:") || lower.starts_with("search-ms:") {
        return Err("that is an address, not a folder path".into());
    }
    if is_device_prefix(s) {
        return Err(r"device paths (\\?\ and \\.\) are not accepted".into());
    }
    if s.chars().any(|c| matches!(c, '?' | '*' | '<' | '>' | '|')) {
        return Err("the folder path has characters a folder name cannot have".into());
    }
    // `C:` alone means "the current folder of drive C" to Windows: here it is the drive's root.
    let mut owned = s.to_string();
    if is_bare_drive(&owned) {
        owned.push(BACKSLASH);
    }
    if cfg!(windows) {
        owned = owned.replace('/', "\\");
    }
    // `Path::components` quietly drops a `.` in the middle, so the segments are looked at as text.
    if owned.split(['\\', '/']).any(|seg| seg == "." || seg == "..") {
        return Err("the folder path must not contain `.` or `..`".into());
    }
    let path = Path::new(&owned);
    if !path.is_absolute() {
        return Err("the folder must be an absolute path".into());
    }
    if path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err("the folder path must not contain `.` or `..`".into());
    }
    // Re-joined from its parts: no doubled or trailing separators (the root keeps its own).
    let tidy: PathBuf = path.components().collect();
    tidy.to_str().map(str::to_owned).ok_or_else(|| "the folder path is not valid text".into())
}

fn is_device_prefix(s: &str) -> bool {
    let n = s.replace('/', "\\");
    n.starts_with(r"\\?\") || n.starts_with(r"\\.\")
}

/// `C:` (a drive letter and a colon, nothing else).
fn is_bare_drive(s: &str) -> bool {
    let mut it = s.chars();
    matches!((it.next(), it.next(), it.next()), (Some(d), Some(':'), None) if d.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn explorer_paths_pass_and_a_drive_root_loses_the_quote_it_ate() {
        assert_eq!(clean(r"C:\Users\me\code").unwrap(), r"C:\Users\me\code");
        assert_eq!(clean(r"C:\Users\me\code\").unwrap(), r"C:\Users\me\code");
        assert_eq!(clean(r"D:\My Projects\a b").unwrap(), r"D:\My Projects\a b");
        // `"C:\"` through argv parsing is `C:"`.
        assert_eq!(clean("C:\"").unwrap(), r"C:\");
        assert_eq!(clean(r"C:\").unwrap(), r"C:\");
        assert_eq!(clean("C:").unwrap(), r"C:\");
        assert_eq!(clean("c:").unwrap(), r"c:\");
        assert_eq!(clean("\"C:\\\"").unwrap(), r"C:\");
        assert_eq!(clean("C:/Users/me").unwrap(), r"C:\Users\me");
        assert_eq!(clean(r"\\server\share\dir").unwrap(), r"\\server\share\dir");
        assert_eq!(clean(r"\\server\share").unwrap(), r"\\server\share\");
    }

    #[test]
    fn hostile_and_virtual_paths_are_refused_with_a_reason() {
        for raw in [
            "",
            "   ",
            "\"",
            r"relative\dir",
            r"..\up",
            r"C:\a\..\b",
            r"C:\a\.\b",
            "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
            r"C:\x::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
            "shell:Downloads",
            "SHELL:::{645FF040-5081-101B-9F08-00AA002F954E}",
            r"\\?\C:\Windows",
            r"\\.\PhysicalDrive0",
            "//?/C:/Windows",
            "https://example.com/x",
            "file:///C:/x",
            r"C:\a*b",
            r"C:\a?b",
            r"C:\a|b",
            "C:\\a\"b",
            "C:\\a\nb",
            "C:\\a\u{202e}b",
            "C:\\a\u{0}b",
        ] {
            let why = clean(raw).expect_err(raw);
            assert!(!why.is_empty(), "{raw:?}");
        }
        assert!(clean(&format!("C:\\{}", "a".repeat(MAX_LEN))).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn a_posix_absolute_path_passes() {
        assert_eq!(clean("/home/me/code/").unwrap(), "/home/me/code");
        assert_eq!(clean("/").unwrap(), "/");
    }
}

//! Lexical Windows path helpers with .NET `System.IO.Path` semantics.
//!
//! The WPF app compares and validates *Windows* paths (programs for "Open with...", worktree
//! folders, `\\wsl$` shares). `std::path` follows the host OS, which would make these rules
//! (and their tests) differ between Windows and Linux, so the Windows behaviour is spelled
//! out here. Nothing touches the disk.

/// `\` or `/`.
#[must_use]
pub fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// The last path segment (`Path.GetFileName`): everything after the last separator.
#[must_use]
pub fn file_name(path: &str) -> &str {
    match path.rfind(is_sep) {
        Some(i) => path.get(i + 1..).unwrap_or(""),
        None => path,
    }
}

/// `Path.GetExtension`: from the last `.` of the file name, or "" when there is none or the
/// dot is the last character.
#[must_use]
pub fn extension(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => name.get(i..).unwrap_or(""),
        _ => "",
    }
}

/// `Path.GetFileNameWithoutExtension`.
#[must_use]
pub fn file_name_without_extension(path: &str) -> &str {
    let name = file_name(path);
    match name.rfind('.') {
        Some(i) => name.get(..i).unwrap_or(name),
        None => name,
    }
}

/// `Path.IsPathFullyQualified` on Windows: a drive with a separator (`C:\x`), or two leading
/// separators / a leading separator and `?` (UNC and device paths). `C:x` and `\x` are not.
#[must_use]
pub fn is_fully_qualified(path: &str) -> bool {
    let mut it = path.chars();
    let (Some(a), Some(b)) = (it.next(), it.next()) else {
        return false;
    };
    if is_sep(a) {
        return b == '?' || is_sep(b);
    }
    match it.next() {
        Some(c) => b == ':' && is_sep(c) && a.is_ascii_alphabetic(),
        None => false,
    }
}

/// `Path.IsPathRooted` on Windows: leading separator, or a drive letter and colon (`C:x`).
#[must_use]
pub fn is_rooted(path: &str) -> bool {
    let mut it = path.chars();
    match (it.next(), it.next()) {
        (Some(a), _) if is_sep(a) => true,
        (Some(a), Some(b)) => a.is_ascii_alphabetic() && b == ':',
        _ => false,
    }
}

/// `Path.GetPathRoot` for fully-qualified paths: `C:\`, or `\\server\share` for UNC; "" when
/// the path has no root we recognise.
#[must_use]
pub fn path_root(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    if let (Some(&a), Some(&b)) = (chars.first(), chars.get(1)) {
        if a.is_ascii_alphabetic() && b == ':' {
            return match chars.get(2) {
                Some(&c) if is_sep(c) => chars.iter().take(3).collect(),
                _ => chars.iter().take(2).collect(),
            };
        }
        if is_sep(a) && is_sep(b) {
            // \\server\share
            let rest: String = chars.iter().skip(2).collect();
            let mut parts = rest.split(is_sep);
            let server = parts.next().unwrap_or("");
            let share = parts.next().unwrap_or("");
            return if share.is_empty() { format!("\\\\{server}") } else { format!("\\\\{server}\\{share}") };
        }
    }
    String::new()
}

/// Lexical `Path.GetFullPath` for a path that is already fully qualified: separators become
/// `\`, repeated separators collapse (after a UNC prefix), `.` and `..` resolve without
/// climbing above the root, and trailing dots and spaces on a segment are dropped, as
/// `GetFullPathNameW` does (so `C:\Windows .\x` is `C:\Windows\x`).
#[must_use]
pub fn normalize_full(path: &str) -> String {
    let unified: String = path.chars().map(|c| if c == '/' { '\\' } else { c }).collect();
    let root = path_root(&unified);
    let rest = unified.get(root.len()..).unwrap_or("");
    let mut stack: Vec<&str> = Vec::new();
    for seg in rest.split('\\') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => {
                let trimmed = other.trim_end_matches(['.', ' ']);
                if trimmed.is_empty() {
                    // "..." or ". ." collapse to nothing, like a "." segment
                    continue;
                }
                stack.push(trimmed);
            }
        }
    }
    let mut out = root.clone();
    let drive_root = out.ends_with('\\');
    for (i, seg) in stack.iter().enumerate() {
        if i > 0 || !drive_root {
            out.push('\\');
        }
        out.push_str(seg);
    }
    if stack.is_empty() && root.len() == 2 && root.ends_with(':') {
        out.push('\\');
    }
    out
}

/// `Path.GetFullPath(path, basePath)` lexically: joins onto the base unless the path is
/// already fully qualified, then normalises.
#[must_use]
pub fn get_full_path(path: &str, base: &str) -> String {
    let joined = if is_fully_qualified(path) {
        path.to_string()
    } else if path.starts_with(is_sep) {
        // rooted on the base's drive/share
        format!("{}{}", path_root(base), path)
    } else {
        format!("{}\\{}", base.trim_end_matches(is_sep), path)
    };
    normalize_full(&joined)
}

/// Ordinal ignore-case equality (`StringComparison.OrdinalIgnoreCase`).
#[must_use]
pub fn eq_ic(a: &str, b: &str) -> bool {
    a.to_uppercase() == b.to_uppercase()
}

/// Ordinal ignore-case `starts_with`.
#[must_use]
pub fn starts_with_ic(s: &str, prefix: &str) -> bool {
    s.to_uppercase().starts_with(&prefix.to_uppercase())
}

/// Ordinal ignore-case `ends_with`.
#[must_use]
pub fn ends_with_ic(s: &str, suffix: &str) -> bool {
    s.to_uppercase().ends_with(&suffix.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_extensions_follow_dotnet() {
        assert_eq!(file_name(r"C:\a\b.exe"), "b.exe");
        assert_eq!(file_name("a/b/c"), "c");
        assert_eq!(file_name("plain"), "plain");
        assert_eq!(extension(r"C:\a\b.EXE"), ".EXE");
        assert_eq!(extension("a."), "");
        assert_eq!(extension(".bashrc"), ".bashrc");
        assert_eq!(extension("noext"), "");
        assert_eq!(file_name_without_extension("a.b.c"), "a.b");
        assert_eq!(file_name_without_extension(".bashrc"), "");
        assert_eq!(file_name_without_extension("a."), "a");
        assert_eq!(file_name_without_extension(r"C:\x\pwsh.exe"), "pwsh");
    }

    #[test]
    fn fully_qualified_rules() {
        assert!(is_fully_qualified(r"C:\x"));
        assert!(is_fully_qualified("c:/x"));
        assert!(is_fully_qualified(r"\\server\share"));
        assert!(is_fully_qualified(r"\\?\C:\x"));
        assert!(!is_fully_qualified("C:x"));
        assert!(!is_fully_qualified(r"\x"));
        assert!(!is_fully_qualified("editor.exe"));
        assert!(!is_fully_qualified(""));
        assert!(!is_fully_qualified("C:"));
    }

    #[test]
    fn rooted_rules() {
        assert!(is_rooted(r"C:\x"));
        assert!(is_rooted("C:x"));
        assert!(is_rooted("/home"));
        assert!(!is_rooted(".git"));
        assert!(!is_rooted(""));
    }

    #[test]
    fn roots() {
        assert_eq!(path_root(r"C:\a\b"), r"C:\");
        assert_eq!(path_root(r"\\srv\share\x"), r"\\srv\share");
        assert_eq!(path_root("rel"), "");
    }

    #[test]
    fn normalizes_lexically() {
        assert_eq!(normalize_full(r"C:\Tools\..\Windows\System32\a.exe"), r"C:\Windows\System32\a.exe");
        assert_eq!(normalize_full("C:/a//b/./c/"), r"C:\a\b\c");
        assert_eq!(normalize_full(r"C:\.."), r"C:\");
        assert_eq!(normalize_full(r"C:\Windows .\x"), r"C:\Windows\x");
        assert_eq!(normalize_full(r"\\wsl$\Ubuntu\home\u\repo\.git"), r"\\wsl$\Ubuntu\home\u\repo\.git");
        assert_eq!(normalize_full(r"\\srv\share\..\..\x"), r"\\srv\share\x");
    }

    #[test]
    fn full_path_joins_relative() {
        assert_eq!(get_full_path(".git", r"C:\src\app"), r"C:\src\app\.git");
        assert_eq!(get_full_path(r"..\x", r"C:\src\app"), r"C:\src\x");
        assert_eq!(get_full_path(r"D:\y", r"C:\src"), r"D:\y");
    }

    #[test]
    fn ignore_case_helpers() {
        assert!(eq_ic("ABC", "abc"));
        assert!(starts_with_ic("//WSL$/x", "//wsl$/"));
        assert!(ends_with_ic("a/.GIT", "/.git"));
    }
}

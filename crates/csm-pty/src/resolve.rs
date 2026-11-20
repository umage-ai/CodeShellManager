//! Program lookup for CreateProcess. Never searches the current directory (CreateProcess's own
//! search does, which is a hijack: a `git.exe` in a cloned repo would run). Only `.exe`/`.com`
//! are accepted: a `.bat`/`.cmd` runs through cmd.exe, which re-parses arguments (BatBadBut).

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

const EXTS: [&str; 2] = [".exe", ".com"];

fn refuse(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn ext_ok(p: &Path) -> Result<bool, io::Error> {
    match p.extension().and_then(OsStr::to_str).map(str::to_ascii_lowercase) {
        None => Ok(false),
        Some(e) if e == "exe" || e == "com" => Ok(true),
        Some(e) if e == "bat" || e == "cmd" => Err(refuse(format!("refusing a batch file as a program: {}", p.display()))),
        // An unknown extension (e.g. "python3.12") is treated as part of the name.
        Some(_) => Ok(false),
    }
}

fn candidates(base: &Path) -> Result<Vec<PathBuf>, io::Error> {
    if ext_ok(base)? {
        return Ok(vec![base.to_path_buf()]);
    }
    Ok(EXTS
        .iter()
        .map(|e| {
            let mut s: OsString = base.as_os_str().to_owned();
            s.push(e);
            PathBuf::from(s)
        })
        .collect())
}

/// Resolves `program` to a full path. Absolute paths are taken as given (with `.exe`/`.com`
/// appended when there is no extension); bare names are searched on the absolute entries of
/// `path_var`; relative paths with a separator are refused.
pub fn find_program(program: &OsStr, path_var: Option<&OsStr>) -> io::Result<PathBuf> {
    let p = Path::new(program);
    if p.as_os_str().is_empty() {
        return Err(refuse("empty program".into()));
    }
    if p.is_absolute() {
        for c in candidates(p)? {
            if c.is_file() {
                return Ok(c);
            }
        }
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} not found", p.display())));
    }
    if p.components().count() != 1 || p.has_root() {
        return Err(refuse(format!("refusing a relative program path: {}", p.display())));
    }
    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(path_var) {
            if !dir.is_absolute() {
                continue; // a relative PATH entry is the current directory by another name
            }
            for c in candidates(&dir.join(p))? {
                if c.is_file() {
                    return Ok(c);
                }
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, format!("{} not found on PATH", p.display())))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn finds_cmd_on_system_path() {
        let path = std::env::var_os("PATH");
        let found = find_program(OsStr::new("cmd"), path.as_deref()).unwrap();
        assert!(found.is_absolute());
        assert!(found.to_string_lossy().to_ascii_lowercase().ends_with("cmd.exe"));
    }

    #[test]
    fn relative_paths_and_batch_files_are_refused() {
        let path = std::env::var_os("PATH");
        assert!(find_program(OsStr::new(".\\evil.exe"), path.as_deref()).is_err());
        assert!(find_program(OsStr::new("sub\\evil.exe"), path.as_deref()).is_err());
        let e = find_program(OsStr::new("thing.cmd"), path.as_deref()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn relative_path_entries_are_skipped() {
        // "." on PATH must not make a file in the current directory resolvable.
        let e = find_program(OsStr::new("definitely-not-a-real-program"), Some(OsStr::new("."))).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }
}

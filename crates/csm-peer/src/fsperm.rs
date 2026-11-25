//! Files only the current user can read: the host key, the device list, the audit log.
//!
//! Unix: the folder is 0700 and every file is created 0600. Windows: the folder's inherited
//! permissions are replaced with one entry for the current user (`icacls`, started by argv with
//! the System32 path, no shell), and the files inherit it. The result says whether it worked;
//! a system where it cannot (no `icacls`, an unusual account) still runs, and `peer_status`
//! shows `private_files_restricted: false`.

use std::io::Write;
use std::path::Path;

/// Creates `dir` (and parents) limited to the current user. `true` when the limit is in place.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<bool> {
    std::fs::create_dir_all(dir)?;
    restrict(dir)
}

#[cfg(unix)]
fn restrict(dir: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(true)
}

#[cfg(windows)]
fn restrict(dir: &Path) -> std::io::Result<bool> {
    // The grant names the account by the SID of this process's token (what `whoami /user` reports),
    // never by `USERNAME` / `USERDOMAIN`, which any parent process can set to anything.
    let Some(sid) = current_user_sid() else { return Ok(false) };
    let Some(icacls) = system32("icacls.exe") else { return Ok(false) };
    let who = std::ffi::OsString::from(format!("*{sid}:(OI)(CI)F"));
    let args = [dir.as_os_str().to_owned(), "/inheritance:r".into(), "/grant:r".into(), who];
    Ok(csm_engine::proc::capture(icacls, args, std::time::Duration::from_secs(10)).is_some_and(|c| c.success))
}

/// A program in `%SystemRoot%\System32`, by its full path (never found through `PATH`).
#[cfg(windows)]
fn system32(name: &str) -> Option<std::path::PathBuf> {
    let root = std::env::var_os("SystemRoot").filter(|r| !r.is_empty())?;
    Some(std::path::PathBuf::from(root).join("System32").join(name))
}

/// The SID of the user this process runs as: `whoami /user` (System32 path, no shell), whose CSV
/// line is `"DOMAIN\name","S-1-5-..."`. This crate forbids `unsafe`, so the token is not read
/// through the Win32 API directly.
#[cfg(windows)]
fn current_user_sid() -> Option<String> {
    let whoami = system32("whoami.exe")?;
    let out = csm_engine::proc::capture(whoami, ["/user", "/fo", "csv", "/nh"], std::time::Duration::from_secs(10)).filter(|c| c.success)?;
    parse_whoami_sid(&out.text())
}

/// The SID in the output of `whoami /user /fo csv /nh`, if it is one.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_whoami_sid(csv: &str) -> Option<String> {
    let line = csv.lines().map(str::trim).find(|l| !l.is_empty())?;
    let sid = line.rsplit(',').next()?.trim().trim_matches('"');
    let rest = sid.strip_prefix("S-1-")?;
    let ok = !rest.is_empty() && rest.len() < 180 && rest.chars().all(|c| c.is_ascii_digit() || c == '-') && !rest.starts_with('-') && !rest.ends_with('-');
    ok.then(|| sid.to_owned())
}

#[cfg(not(any(unix, windows)))]
fn restrict(_dir: &Path) -> std::io::Result<bool> {
    Ok(false)
}

/// Writes a private KEY: as [`write_private`], but only after the folder is confirmed limited to
/// the current user. If the limit cannot be put in place (no `icacls`, no SID) nothing is written:
/// a key must not be created where other accounts could read it.
pub fn write_secret(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && !ensure_private_dir(dir)?
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the folder could not be limited to your user account, so the key was not written",
        ));
    }
    write_private(path, data)
}

/// Writes `data` to `path` through a temporary file in the same folder, so a crash never leaves a
/// half-written key or list. The file is created readable by the current user only (Unix: 0600).
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_atomically_and_replaces() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("k.json");
        write_private(&p, b"one").unwrap();
        write_private(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!p.with_extension("tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn files_and_folder_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("peer");
        assert!(ensure_private_dir(&dir).unwrap());
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let p = dir.join("host.key");
        write_private(&p, b"x").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn the_whoami_sid_is_read_from_its_csv_line_and_nothing_else_passes() {
        assert_eq!(parse_whoami_sid("\"DESKTOP\\allan\",\"S-1-5-21-1004336348-1177238915-682003330-1001\"\r\n").as_deref(), Some("S-1-5-21-1004336348-1177238915-682003330-1001"));
        for bad in ["", "ERROR: nope", "\"a\",\"not-a-sid\"", "\"a\",\"S-1-\"", "\"a\",\"S-1-5-21-1) (OI)(CI)F /grant Everyone:F\""] {
            assert_eq!(parse_whoami_sid(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_key_is_written_when_the_folder_is_private() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("peer").join("host.key");
        match write_secret(&p, b"k") {
            Ok(()) => assert_eq!(std::fs::read(&p).unwrap(), b"k"),
            // A system where the limit cannot be applied: the key must not exist.
            Err(e) => {
                assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
                assert!(!p.exists(), "no key was written without the limit");
            }
        }
    }
}

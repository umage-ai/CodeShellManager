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
    let Some(user) = std::env::var_os("USERNAME").filter(|u| !u.is_empty()) else { return Ok(false) };
    let Some(root) = std::env::var_os("SystemRoot").filter(|r| !r.is_empty()) else { return Ok(false) };
    let icacls = std::path::PathBuf::from(root).join("System32").join("icacls.exe");
    let mut who = std::ffi::OsString::new();
    if let Some(domain) = std::env::var_os("USERDOMAIN").filter(|d| !d.is_empty()) {
        who.push(domain);
        who.push("\\");
    }
    who.push(user);
    who.push(":(OI)(CI)F");
    let args = [dir.as_os_str().to_owned(), "/inheritance:r".into(), "/grant:r".into(), who];
    Ok(csm_engine::proc::capture(icacls, args, std::time::Duration::from_secs(10)).is_some_and(|c| c.success))
}

#[cfg(not(any(unix, windows)))]
fn restrict(_dir: &Path) -> std::io::Result<bool> {
    Ok(false)
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
}

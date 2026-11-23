//! Owner-only access for the add-on's own files on Windows: the inherited access list is replaced
//! with one entry, Full control for the current user, by `icacls` (started by its System32 path
//! with argv, no shell, no console window). On Unix the store sets 0600 with `std::fs` instead.
//!
//! This is the add-on's only process start, and xtask's process policy lists this file for it. The
//! add-on does not depend on `csm-engine` (it is a separate program), so it starts `icacls` itself.
//!
//! The limit is stated in the design note (section 4.2): code running as the same user can read the
//! files. This keeps other local accounts out, and keeps the files out of a backup taken as another
//! user. It does not stop the owner's own agents.

use std::path::Path;

/// Replaces the access list of `path` with Full control for the current user. `Err` when it could
/// not be done: the caller must not keep a secret in that file.
#[cfg(windows)]
pub fn restrict_to_current_user(path: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let user = std::env::var_os("USERNAME").filter(|u| !u.is_empty()).ok_or("the account name is not set")?;
    let root = std::env::var_os("SystemRoot").filter(|r| !r.is_empty()).ok_or("the system folder is not set")?;
    let icacls = std::path::PathBuf::from(root).join("System32").join("icacls.exe");
    let mut grant = user;
    grant.push(":F");
    let status = Command::new(icacls)
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(grant)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("icacls could not start: {e}"))?;
    if status.success() { Ok(()) } else { Err("icacls did not set the access list".into()) }
}

/// Not Windows: the store sets the Unix mode itself, so nothing is done here.
#[cfg(not(windows))]
pub fn restrict_to_current_user(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// After the restriction the access list names the current user and nobody broad: no Everyone,
    /// no Users. Reads the list back with the same `icacls` the restriction used.
    #[test]
    fn a_restricted_file_is_readable_by_the_current_user_only() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("host-key");
        std::fs::write(&p, [1u8; 32]).unwrap();
        restrict_to_current_user(&p).unwrap();
        let root = std::env::var_os("SystemRoot").unwrap();
        let out = std::process::Command::new(std::path::PathBuf::from(root).join("System32").join("icacls.exe")).arg(&p).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        let user = std::env::var("USERNAME").unwrap().to_lowercase();
        assert!(text.contains(&user), "the current user is listed: {text}");
        assert!(!text.contains("everyone") && !text.contains("builtin\\users"), "no broad group is listed: {text}");
    }
}

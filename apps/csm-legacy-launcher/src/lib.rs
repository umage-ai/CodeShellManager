//! `CodeShellManager.exe`, the 0.x app's exe name, kept as a tiny launcher (docs/release.md,
//! "Risks for existing WPF users"): the stable MSI replaces the WPF app in the same folder, and
//! taskbar pins, desktop shortcuts, Start tiles and people's own scripts that name
//! `<install folder>\CodeShellManager.exe` would otherwise break. The app itself stays
//! `codeshell.exe`; the MSI's own shortcuts point at it.
//!
//! It starts `codeshell.exe` from its **own** folder (the full path, never a PATH lookup), passes
//! its arguments on as argv (std's MSVCRT quoting, no shell, nothing interpolated), and exits at
//! once without waiting. It is a Windows GUI program, so no console window flashes.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The app's file name (`codeshell.exe` on Windows).
pub fn target_name() -> String {
    format!("codeshell{}", std::env::consts::EXE_SUFFIX)
}

/// `codeshell.exe` in the launcher's own folder (`None` for a bare file name: never PATH).
pub fn target_beside(launcher: &Path) -> Option<PathBuf> {
    launcher.parent().filter(|d| !d.as_os_str().is_empty()).map(|d| d.join(target_name()))
}

/// What happened.
#[derive(Debug)]
pub enum Outcome {
    /// The app was started (the launcher does not wait for it).
    Started,
    /// No `codeshell.exe` beside the launcher.
    Missing(PathBuf),
    /// The launcher's own folder is unknown.
    NoFolder,
    /// The app is there but did not start.
    Failed(PathBuf, String),
}

impl Outcome {
    /// The launcher's exit code: 0 when the app started.
    pub fn exit_code(&self) -> u8 {
        match self {
            Outcome::Started => 0,
            Outcome::Failed(..) => 1,
            Outcome::Missing(_) => 2,
            Outcome::NoFolder => 3,
        }
    }

    /// What to tell the person (and the log) when the app did not start.
    pub fn message(&self) -> Option<String> {
        match self {
            Outcome::Started => None,
            Outcome::Missing(p) => Some(format!(
                "CodeShellManager could not start: {} is missing.\n\nReinstall CodeShellManager, or start {} from its folder.",
                p.display(),
                target_name()
            )),
            Outcome::NoFolder => Some(format!("CodeShellManager could not start: the folder of {} is unknown.", target_name())),
            Outcome::Failed(p, e) => Some(format!("CodeShellManager could not start {}: {e}", p.display())),
        }
    }
}

/// Starts the app beside `launcher` with `args` (the launcher's own, without its program name).
pub fn launch(launcher: &Path, args: Vec<OsString>) -> Outcome {
    let Some(target) = target_beside(launcher) else { return Outcome::NoFolder };
    if !target.is_file() {
        return Outcome::Missing(target);
    }
    let mut cmd = Command::new(&target);
    cmd.args(args);
    #[cfg(windows)]
    {
        // The app is a GUI program, so this changes nothing for it; it keeps a console program
        // (a broken install, the tests' stand-in) from flashing a window.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.spawn() {
        Ok(_child) => Outcome::Started,
        Err(e) => Outcome::Failed(target, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn the_app_is_looked_for_beside_the_launcher_only() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("CodeShellManager.exe");
        assert_eq!(target_beside(&launcher).unwrap(), dir.path().join(target_name()));
        assert!(target_beside(Path::new("CodeShellManager.exe")).is_none(), "no folder: never a PATH lookup");
    }

    #[test]
    fn a_missing_app_is_reported_with_its_path_and_a_non_zero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("CodeShellManager.exe");
        let out = launch(&launcher, vec!["--inbox".into()]);
        let Outcome::Missing(p) = &out else { panic!("{out:?}") };
        assert_eq!(p, &dir.path().join(target_name()));
        assert_eq!(out.exit_code(), 2);
        let msg = out.message().unwrap();
        assert!(msg.contains(&p.display().to_string()) && msg.contains("Reinstall"), "{msg}");
        assert_eq!(launch(Path::new("CodeShellManager.exe"), vec![]).exit_code(), 3);
        assert!(Outcome::Started.message().is_none());
    }

    #[test]
    fn a_folder_named_like_the_app_is_not_the_app() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(target_name())).unwrap();
        assert!(matches!(launch(&dir.path().join("CodeShellManager.exe"), vec![]), Outcome::Missing(_)));
    }
}

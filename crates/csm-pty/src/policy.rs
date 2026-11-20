//! Pure decisions about which ConPTY implementation to use. No FFI, so unit-tested everywhere.
//! Port of `ConPtyResolver` / `BundledFailurePolicy` (CLAUDE.md "Bundled ConPTY").

use std::path::{Path, PathBuf};

pub const CONPTY_DLL: &str = "conpty.dll";
pub const OPENCONSOLE_EXE: &str = "OpenConsole.exe";

/// Should the bundled ConPTY be tried at all? Whether it then actually LOADS is a separate step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundledDecision {
    Try { dll: PathBuf, reason: String },
    Skip { reason: String },
}

/// `base_dir` is the folder of the running executable. Both files must be there: conpty.dll
/// launches OpenConsole.exe from its own folder, and a conpty.dll without it is not the
/// implementation that was measured.
pub fn decide_bundled(prefer_bundled: bool, base_dir: Option<&Path>, file_exists: impl Fn(&Path) -> bool) -> BundledDecision {
    if !prefer_bundled {
        return BundledDecision::Skip { reason: "disabled by setting".into() };
    }
    let Some(base) = base_dir else {
        return BundledDecision::Skip { reason: "no application directory".into() };
    };
    if !base.is_absolute() {
        return BundledDecision::Skip { reason: format!("refusing a relative application directory: {}", base.display()) };
    }
    for name in [CONPTY_DLL, OPENCONSOLE_EXE] {
        if !file_exists(&base.join(name)) {
            return BundledDecision::Skip { reason: format!("{name} not found in {}", base.display()) };
        }
    }
    BundledDecision::Try { dll: base.join(CONPTY_DLL), reason: format!("found {CONPTY_DLL} + {OPENCONSOLE_EXE} in {}", base.display()) }
}

/// Repeated bundled `CreatePseudoConsole` failures switch the process to inbox only after
/// [`BundledFailurePolicy::FAILURES_BEFORE_DEMOTION`] in a row; a success resets the count.
/// One transient failure (resource pressure, an AV scan racing OpenConsole.exe) should not cost
/// the rest of the run the bundled ConPTY. Not thread-safe; the caller holds its lock.
#[derive(Debug, Default)]
pub struct BundledFailurePolicy {
    consecutive: u32,
}

impl BundledFailurePolicy {
    pub const FAILURES_BEFORE_DEMOTION: u32 = 3;

    /// Records a failure. Returns the consecutive count and whether this one demotes.
    pub fn record_failure(&mut self) -> (u32, bool) {
        self.consecutive = self.consecutive.saturating_add(1);
        (self.consecutive, self.consecutive >= Self::FAILURES_BEFORE_DEMOTION)
    }

    pub fn record_success(&mut self) {
        self.consecutive = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs_base() -> PathBuf {
        if cfg!(windows) { PathBuf::from(r"C:\app") } else { PathBuf::from("/app") }
    }

    #[test]
    fn disabled_by_setting() {
        let d = decide_bundled(false, Some(&abs_base()), |_| true);
        assert!(matches!(d, BundledDecision::Skip { .. }));
    }

    #[test]
    fn both_files_required() {
        let base = abs_base();
        let only_dll = |p: &Path| p.ends_with(CONPTY_DLL);
        match decide_bundled(true, Some(&base), only_dll) {
            BundledDecision::Skip { reason } => assert!(reason.contains(OPENCONSOLE_EXE)),
            other => panic!("unexpected {other:?}"),
        }
        match decide_bundled(true, Some(&base), |_| true) {
            BundledDecision::Try { dll, .. } => assert_eq!(dll, base.join(CONPTY_DLL)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn relative_base_is_refused() {
        let d = decide_bundled(true, Some(Path::new("relative\\dir")), |_| true);
        assert!(matches!(d, BundledDecision::Skip { .. }));
    }

    #[test]
    fn three_consecutive_failures_demote_and_success_resets() {
        let mut p = BundledFailurePolicy::default();
        assert_eq!(p.record_failure(), (1, false));
        assert_eq!(p.record_failure(), (2, false));
        p.record_success();
        assert_eq!(p.record_failure(), (1, false));
        assert_eq!(p.record_failure(), (2, false));
        assert_eq!(p.record_failure(), (3, true));
    }
}

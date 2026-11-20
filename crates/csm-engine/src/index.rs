//! Where the search index lives and how the engine gets one.
//!
//! Per `docs/plan.md` section 7 the index is a cache of what terminals printed, so it goes in the
//! LOCAL (non-roaming) app data dir, not beside `state.json` in the roaming one. Isolation
//! follows the state file: `--clean` and an isolated run (`CSM_CONFIG_DIR` empty) use an
//! in-memory index and never touch the disk; a state file somewhere else (`--config-dir`,
//! `CSM_CONFIG_DIR`, `CSM_STATE_PATH`, a portable install) gets its index beside it.

use std::path::{Path, PathBuf};

use csm_index::{Index, IndexLocation, IndexOptions};

use crate::state::StateOptions;

/// The file name of the index.
pub const INDEX_FILE: &str = "index.db";
use crate::persist::APP_DIR;

/// How the engine gets its search index.
#[derive(Clone, Default)]
pub enum IndexSetting {
    /// No index: output is not indexed and the search and note tools say so (tests, the demo).
    #[default]
    Off,
    /// Derive the location from the state file (see the module docs) and open it at start-up.
    Auto,
    /// Use this one (tests, embedders).
    Handle(Index),
}

/// The index location for a run. Pure apart from reading the platform's data dirs.
pub fn location(save_path: Option<&Path>, clean: bool) -> IndexLocation {
    if clean {
        return IndexLocation::Memory;
    }
    let Some(save) = save_path else { return IndexLocation::Memory };
    let Some(dir) = save.parent() else { return IndexLocation::Memory };
    let roaming = dirs::data_dir().map(|d| d.join(APP_DIR));
    if roaming.as_deref() == Some(dir)
        && let Some(local) = dirs::data_local_dir()
    {
        return IndexLocation::File(local.join(APP_DIR).join(INDEX_FILE));
    }
    IndexLocation::File(dir.join(INDEX_FILE))
}

/// Starts the index the options ask for (`None` for [`IndexSetting::Off`] or a thread that
/// would not start).
pub(crate) fn open(options: &StateOptions) -> Option<Index> {
    match &options.index {
        IndexSetting::Off => None,
        IndexSetting::Handle(h) => Some(h.clone()),
        IndexSetting::Auto => {
            let loc = location(options.save_path.as_deref(), options.clean);
            if let IndexLocation::File(p) = &loc {
                log::info!("index: {}", p.display());
            } else {
                log::info!("index: in memory (nothing is written to disk)");
            }
            match Index::start(IndexOptions::new(loc)) {
                Ok(i) => Some(i),
                Err(e) => {
                    log::warn!("index: {e}");
                    None
                }
            }
        }
    }
}

/// The WPF app's `output.db` (`%AppData%\CodeShellManager\output.db`).
pub fn wpf_output_db() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("CodeShellManager").join("output.db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_and_isolated_runs_use_memory() {
        let p = PathBuf::from("somewhere").join("state.json");
        assert_eq!(location(Some(&p), true), IndexLocation::Memory);
        assert_eq!(location(None, false), IndexLocation::Memory);
    }

    #[test]
    fn a_custom_state_dir_gets_its_index_beside_it() {
        let p = PathBuf::from("custom").join("dir").join("state.json");
        assert_eq!(location(Some(&p), false), IndexLocation::File(PathBuf::from("custom").join("dir").join(INDEX_FILE)));
    }

    #[test]
    fn the_default_install_keeps_the_index_in_the_local_data_dir() {
        let (Some(roaming), Some(local)) = (dirs::data_dir(), dirs::data_local_dir()) else { return };
        let state = roaming.join(APP_DIR).join("state.json");
        assert_eq!(location(Some(&state), false), IndexLocation::File(local.join(APP_DIR).join(INDEX_FILE)));
    }
}

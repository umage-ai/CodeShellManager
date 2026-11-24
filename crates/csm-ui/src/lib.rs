//! `csm-ui`: the CodeShellManager desktop UI in egui.
//!
//! - [`theme`]: Catppuccin Mocha (dark, default) and Latte (light) as tokens, and the one
//!   spacing / size / type scale. Screens never write a colour or a size as a number.
//! - [`terminal`]: the terminal widget (renderer, input, sources), moved from the Phase 0 spike.
//! - [`model`]: the plain-data snapshot the screens draw ([`model::UiModel`]).
//! - [`backend`]: the seam to whatever owns the state ([`backend::Backend`], [`backend::ToolPort`]);
//!   every action is a tool call on the port (for the engine, `engine.submit(Caller::Ui, ...)`).
//! - [`engine_backend`]: the real backend over `csm-engine` ([`EngineBackend`]).
//! - [`demo`]: a backend with the mock-up's data (`codeshell --demo`, screenshots, UI tests).
//! - [`pane_tools`]: the `pane_*` tools (the Free layout's shape is the UI's).
//! - [`view_overlay`]: the per-client copy of the view state, and the UI-only calls applied to it
//!   (not wired into the app yet).
//! - [`app`] and [`screens`]: the window.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod alt_tap;
pub mod app;
pub mod backend;
pub mod background;
pub mod demo;
pub mod docs_model;
pub mod focus;
pub mod list_keys;
pub mod engine_backend;
pub mod git_model;
mod icon_data;
pub mod icons;
pub mod model;
pub mod menu_nav;
pub mod notify;
pub mod pane_tools;
pub mod placement;
pub mod remote_window;
pub mod screens;
pub mod start_window;
pub mod terminal;
pub mod theme;
pub mod usage_model;
pub mod view_overlay;
pub mod widgets;
pub mod zoom;

pub use app::{Action, CsmApp};
pub use backend::{Backend, Call, EnginePort, Notice, ToolInfo, ToolPort};

pub use engine_backend::{EngineBackend, EngineOptions};

/// The tools the app registers on top of the engine's own: the `pane_*` tools and honest
/// "not built yet" stand-ins for the ids the engine doesn't have yet (runs, git, notes, search,
/// open folder / with, grants). Pass to [`csm_engine::Engine::start`].
pub fn app_tools() -> Vec<csm_engine::ToolSpec> {
    let mut v = demo::tools::stand_ins(demo::tools::StandIn::NotBuilt);
    v.extend(pane_tools::tools());
    v
}

/// An in-memory search index with a little of what the demo's sessions "printed", so Ctrl+F and
/// the palette's search row show real results in `--demo`, screenshots and UI tests.
fn demo_index() -> csm_engine::IndexSetting {
    use csm_engine::csm_index::{Index, IndexOptions};
    let Ok(ix) = Index::start(IndexOptions::memory()) else { return csm_engine::IndexSetting::Off };
    let lines: [(&str, &str, &str, &str); 6] = [
        ("odulodo", "Odulodo", r"C:\GitHub\Odulodo", "Do you want to run `dotnet ef database update`? approval needed for the migration"),
        (
            "web",
            "Odulodo.Web",
            r"C:\GitHub\Odulodo.Web",
            "error TS2345: Argument of type 'BrandFacet' is not assignable to parameter of type 'Facet'",
        ),
        ("web", "Odulodo.Web", r"C:\GitHub\Odulodo.Web", "vite v6 building for production... 214 modules transformed"),
        (
            "rs",
            "CodeShellManager-rs",
            r"C:\GitHub\CodeShellManager-rs",
            "test result: ok. 142 passed; 0 failed; the terminal widget keeps pace with 4 MB/s of output",
        ),
        (
            "csm",
            "CodeShellManager",
            r"C:\GitHub\CodeShellManager",
            "Merged the migration: the terminal search index moved to the local app data folder",
        ),
        (
            "craft",
            "craft-anything",
            r"C:\src\craft-anything",
            "error[E0599]: no method named `frobnicate` found for struct `Engine` in the current scope",
        ),
    ];
    for (id, name, folder, text) in lines {
        ix.note_session(id, name, folder, "");
        ix.feed(id, format!("{text}\r\n").as_bytes());
    }
    // Settings → Usage.
    for (command, sessions, seconds) in [("claude", 42, 31 * 3600 + 20 * 60), ("pwsh", 17, 4 * 3600 + 5 * 60), ("codex", 3, 40 * 60)] {
        for _ in 0..sessions {
            ix.record_usage_start(command);
        }
        ix.record_usage_seconds(command, seconds);
    }
    csm_engine::IndexSetting::Handle(ix)
}

/// The demo's Windows Terminal settings (what `profile_list` answers in `--demo` and UI tests,
/// instead of this machine's real files).
const DEMO_WT_SETTINGS: &str = r##"{
  "profiles": {
    "defaults": { "font": { "face": "Cascadia Mono", "size": 11 } },
    "list": [
      { "name": "PowerShell", "commandline": "pwsh.exe -NoLogo", "startingDirectory": "C:\\GitHub\\Odulodo",
        "colorScheme": "Campbell Powershell", "cursorShape": "bar" },
      { "name": "Ubuntu", "commandline": "wsl.exe -d Ubuntu", "colorScheme": "One Half Dark",
        "cursorShape": "underscore", "padding": "12", "font": { "size": 13 } }
    ]
  }
}"##;

/// Starts an engine for the demo backend: echoing stand-ins and `demo_set_status`, and a PTY
/// factory that refuses, so nothing the demo submits ever starts a process. The demo's sessions
/// live in [`demo::DemoBackend`], not in this engine.
pub fn demo_engine() -> Result<csm_engine::Engine, csm_engine::ToolError> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    demo_engine_with_peer(std::sync::Arc::new(demo::peer::DemoPeerHost::new(now)))
}

/// [`demo_engine`] with a given peer host (the UI tests and screenshots drive a [`demo::peer::DemoPeerHost`]).
pub fn demo_engine_with_peer(host: std::sync::Arc<dyn csm_engine::PeerHost>) -> Result<csm_engine::Engine, csm_engine::ToolError> {
    demo_engine_inner(csm_core::models::AppState::default(), host)
}

/// [`demo_engine`] over a given app state (the view overlay's tests need groups and sessions).
#[cfg(test)]
fn demo_engine_with(app: csm_core::models::AppState) -> Result<csm_engine::Engine, csm_engine::ToolError> {
    demo_engine_inner(app, std::sync::Arc::new(csm_engine::NoPeerHost))
}

fn demo_engine_inner(app: csm_core::models::AppState, peer_host: std::sync::Arc<dyn csm_engine::PeerHost>) -> Result<csm_engine::Engine, csm_engine::ToolError> {
    let pty: csm_engine::PtyFactory = std::sync::Arc::new(|_| Err(std::io::Error::other("the demo starts no processes")));
    let options = csm_engine::StateOptions {
        pty,
        restore_on_start: false,
        index: demo_index(),
        terminal_profiles: csm_engine::TerminalProfiles::Texts(vec![("Stable".into(), DEMO_WT_SETTINGS.into())]),
        peer_host,
        ..csm_engine::StateOptions::default()
    };
    let state = csm_engine::State::new(app, options);
    let mut tools = demo::tools::stand_ins(demo::tools::StandIn::Echo);
    tools.extend(demo::tools::demo_only());
    tools.extend(pane_tools::tools());
    csm_engine::Engine::start(state, tools)
}

//! The demo backend (`codeshell --demo`, screenshots, UI tests): the mock-up's sessions and
//! groups, Claude sessions shown as transcripts, shells as live `csm-pty` + `csm-term` terminals
//! (pwsh or cmd), and every tool's effect applied to its own model when the engine's reply comes
//! back.
//!
//! The calls still go through a real engine ([`crate::demo_engine`]) with the real tools, so
//! params are checked against the real schemas. But the demo's sessions and groups are not in
//! that engine, so an engine-owned tool usually answers "not found": the demo applies a call
//! unless the engine rejected the params themselves (`InvalidParams`, `UnknownTool`). The demo
//! engine's PTY factory refuses, so nothing here starts a process through it.

pub mod peer;
pub mod tools;
pub mod transcripts;

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use csm_engine::{Event, Reply};
use csm_term::GridSize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::backend::{Backend, Call, Notice, ToolPort};
use crate::model::*;
use crate::terminal::{LocalTerminal, Repaint, StaticTerminal, TerminalSource};
use crate::theme::{ThemeKind, Tokens};
use tools::*;

/// How the demo runs.
#[derive(Clone)]
pub struct DemoOptions {
    /// Run real shells for the shell sessions (and for new sessions). Off: everything is a
    /// static transcript (screenshots, tests).
    pub live: bool,
    /// The shell argv for live sessions (`pwsh -NoLogo`, else `cmd`), resolved before the UI starts.
    pub shell: Vec<OsString>,
    /// Where live shells start.
    pub cwd: Option<PathBuf>,
    /// A fixed clock for deterministic ages; `None` uses the real one.
    pub now_ms: Option<u64>,
    pub repaint: Option<Repaint>,
    pub theme: ThemeKind,
}

impl Default for DemoOptions {
    fn default() -> Self {
        DemoOptions { live: false, shell: vec!["cmd".into()], cwd: None, now_ms: None, repaint: None, theme: ThemeKind::Mocha }
    }
}

/// A status ages from "now minus this many minutes" in the demo data.
fn mins(m: u64) -> u64 {
    m * 60_000
}

/// The transcript grid: wide enough for the boxes, tall so content stays bottom-aligned when a
/// pane shrinks.
const TRANSCRIPT: GridSize = GridSize { cols: 100, rows: 48, cell_width: 1, cell_height: 1 };

pub struct DemoBackend {
    opts: DemoOptions,
    model: UiModel,
    snap: Arc<UiModel>,
    terms: HashMap<SessionId, Arc<dyn TerminalSource>>,
    /// Sessions shown as transcripts (redrawn when the theme changes).
    static_ids: std::collections::BTreeSet<SessionId>,
    closed: Vec<SessionView>,
    next_id: u64,
    started: Instant,
    /// Shutdown board animation (app_quit).
    quitting: Option<Instant>,
    /// Restore rail animation (`--demo-restore`).
    restoring: Option<Instant>,
    /// The session last focused per group, as the engine's `UiState::group_focus` (#137).
    group_focus: HashMap<String, SessionId>,
    /// Add-on screens as a test or a screenshot set them up (see [`DemoBackend::screens_handle`]).
    screens: Arc<std::sync::Mutex<DemoScreens>>,
    /// The remote window this demo stands for (`set_remote`); none for the demo's own sessions.
    remote: Option<crate::remote_window::RemoteStatus>,
    /// The last frame's panes as the UI reported them (`show_panes`), for the tests.
    panes: PanesLog,
}

/// The panes the UI last reported: the visible session ids and the focused one.
pub type PanesLog = Arc<Mutex<Option<(Vec<String>, Option<String>)>>>;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[allow(clippy::too_many_arguments)]
fn session(id: &str, group: &str, name: &str, kind: Kind, cmd: &str, folder: &str, branch: Option<&str>, status: Status, accent: u8) -> SessionView {
    SessionView {
        id: id.into(),
        name: name.into(),
        group: Some(group.into()),
        kind,
        command: cmd.into(),
        folder: folder.into(),
        branch: branch.map(Into::into),
        status,
        accent: Accent::Palette(accent),
        started: "2 h 14 min ago · resumes on restart".into(),
        open_with: demo_targets(kind, folder),
        ..SessionView::default()
    }
}

fn runs_dotnet() -> Vec<RunView> {
    vec![
        RunView {
            id: "build".into(),
            label: "build".into(),
            command_line: "dotnet build -c Release".into(),
            is_default: true,
            state: RunState::Passed,
            detail: "passed · exit 0".into(),
            output: "Restore complete (0.6s)\n  Odulodo.Core succeeded → bin/Release/net10.0/Odulodo.Core.dll\n  Odulodo.Api succeeded → bin/Release/net10.0/Odulodo.Api.dll\n\nBuild succeeded in 8.4s\n    0 Warning(s)\n    0 Error(s)".into(),
            ..RunView::default()
        },
        RunView { id: "test".into(), label: "test".into(), command_line: "dotnet test --no-build".into(), state: RunState::Running, detail: "running · 38 s".into(), ..RunView::default() },
        RunView { id: "publish".into(), label: "publish".into(), command_line: "dotnet publish -r win-x64".into(), detail: "not run yet".into(), ..RunView::default() },
    ]
}

fn runs_npm() -> Vec<RunView> {
    vec![
        RunView {
            id: "dev".into(),
            label: "dev".into(),
            command_line: "npm run dev".into(),
            is_default: true,
            state: RunState::Running,
            detail: "running · 4 min".into(),
            output: "> odulodo-web@2.14.0 dev\n> vite\n\n  VITE v6.2.0  ready in 412 ms\n\n  ➜  Local:   http://localhost:5173/\n  ➜  Network: use --host to expose\n\n09:41:12 [vite] hmr update /src/search/FacetPanel.tsx\n09:42:03 [vite] hmr update /src/search/facets.ts".into(),
            ..RunView::default()
        },
        RunView { id: "test".into(), label: "test".into(), command_line: "npm test -- --run".into(), state: RunState::Passed, detail: "passed · exit 0".into(), ..RunView::default() },
        RunView { id: "build".into(), label: "build".into(), command_line: "npm run build".into(), state: RunState::Failed, detail: "failed · exit 1".into(), ..RunView::default() },
    ]
}

/// The mock-up's data (`SESS0`, `GROUPS`, `FOLDERS`, the initial layouts).
pub fn demo_model(now: u64, theme: ThemeKind) -> UiModel {
    use Kind::*;
    use Status::*;
    // Accent indexes into the palette order (red, peach, yellow, green, teal, sapphire, sky,
    // blue, lavender, mauve, pink, maroon).
    let mut s = vec![
        session("odulodo", "odulodo", "Odulodo", Local, "claude", "C:\\GitHub\\Odulodo", Some("main"), Approval, 9),
        session("web", "odulodo", "Odulodo.Web", Local, "claude", "C:\\GitHub\\Odulodo.Web", Some("feat/brand-facet"), Working, 3),
        session(
            "wt",
            "odulodo",
            "search-sort",
            Local,
            "claude",
            "C:\\GitHub\\Odulodo.Web\\.worktrees\\search-sort",
            Some("feat/search-sort"),
            Question,
            4,
        ),
        session("webps", "odulodo", "Odulodo.Web (PS)", Local, "pwsh", "C:\\GitHub\\Odulodo.Web", Some("feat/brand-facet"), Idle, 3),
        session("csm", "umage", "CodeShellManager", Local, "claude", "C:\\GitHub\\CodeShellManager", Some("main"), Done, 7),
        session("rs", "umage", "CodeShellManager-rs", Local, "claude", "C:\\GitHub\\CodeShellManager-rs", Some("phase0/term-spike"), Working, 6),
        session("craft", "umage", "craft-anything", Local, "claude", "C:\\src\\craft-anything", Some("main"), Error, 10),
        session("news", "umage", "OptimizelyNewsletter", Local, "claude", "C:\\GitHub\\OptimizelyNewsletter", Some("main"), Asleep, 8),
        session("build", "ops", "build-01", Ssh, "bash", "deploy@build-01:/srv/odulodo", None, Idle, 2),
        session("gw", "ops", "api-gateway", Wsl, "claude", "Ubuntu-24.04 · ~/src/api-gateway", Some("main"), Working, 11),
        session("stg", "ops", "staging-db", Ssh, "bash", "ops@stg-db-02", None, Asleep, 0),
    ];
    let set = |s: &mut Vec<SessionView>, id: &str, f: &dyn Fn(&mut SessionView)| {
        if let Some(x) = s.iter_mut().find(|x| x.id == id) {
            f(x);
        }
    };
    set(&mut s, "odulodo", &|x| {
        x.dirty = true;
        x.status_since_ms = Some(now - mins(4));
        x.detail = "Bash: dotnet test tests/Odulodo.Tests".into();
        x.runs = runs_dotnet();
    });
    set(&mut s, "web", &|x| {
        x.dirty = true;
        x.ahead = 2;
        x.progress = Some(40);
        x.runs = runs_npm();
        // Claude Code Remote Control is on: continued from the phone.
        x.remote_control = true;
        x.remote_control_url = Some("https://claude.ai/code/session_01DemoRemoteControlLink".into());
    });
    set(&mut s, "build", &|x| {
        // A colour the remote's prompt hook pushed (OSC 9001): "Reset accent colour" applies.
        x.accent = Accent::Rgb([0xe0, 0x8f, 0x62]);
        x.accent_overridden = true;
    });
    // Odulodo.Web's checkouts share one repo root; search-sort is a linked worktree of it.
    for id in ["web", "webps", "wt"] {
        set(&mut s, id, &|x| x.repo_root = Some(crate::git_model::DEMO_REPO.into()));
    }
    set(&mut s, "wt", &|x| {
        x.parent = Some("web".into());
        x.worktree_of = Some("Odulodo.Web".into());
        x.status_since_ms = Some(now - mins(1));
        x.detail = "Which sort options should results offer?".into();
    });
    set(&mut s, "webps", &|x| {
        x.dirty = true;
        x.ahead = 2;
    });
    set(&mut s, "csm", &|x| {
        x.status_since_ms = Some(now - mins(6));
        x.detail = "Bumped to 0.11.1, release build is clean".into();
        x.runs = runs_dotnet();
    });
    set(&mut s, "rs", &|x| x.ahead = 5);
    for (id, m) in LAST_USED_MIN {
        set(&mut s, id, &|x| x.last_used_ms = i64::try_from(now.saturating_sub(mins(m))).ok());
    }
    set(&mut s, "craft", &|x| {
        x.dirty = true;
        x.status_since_ms = Some(now - mins(12));
        x.detail = "API error 529, the turn ended".into();
    });
    set(&mut s, "gw", &|x| x.progress = Some(70));
    // The Board: each session's project is its folder's repository (the demo has no git).
    for x in &mut s {
        let leaf = x.folder.rsplit(['\\', '/']).next().unwrap_or_default().to_string();
        x.project = format!("repo:{}", x.folder);
        x.project_name = leaf;
    }

    let groups = vec![
        GroupView { id: "odulodo".into(), name: "Odulodo".into(), torn_off: false, collapsed: false },
        GroupView { id: "umage".into(), name: "Umage".into(), torn_off: false, collapsed: false },
        GroupView { id: "ops".into(), name: "Ops".into(), torn_off: false, collapsed: false },
    ];
    let side = |pinned: &[&str], grid: GridPreset| Layout {
        mode: LayoutMode::Side,
        pinned: pinned.iter().map(|p| p.to_string()).collect(),
        grid,
        ..Layout::default()
    };
    let mut layouts = BTreeMap::new();
    let mut odu = side(&["odulodo", "web", "webps"], GridPreset::default());
    odu.tree = Some(SplitNode::Split {
        dir: SplitDir::Row,
        ratios: vec![0.52, 0.48],
        children: vec![
            SplitNode::leaf("odulodo"),
            SplitNode::Split { dir: SplitDir::Col, ratios: vec![0.58, 0.42], children: vec![SplitNode::leaf("web"), SplitNode::leaf("webps")] },
        ],
    });
    odu.floats = vec![FloatPane { session: "wt".into(), rect: [470.0, 260.0, 440.0, 300.0] }];
    layouts.insert("odulodo".into(), odu);
    layouts.insert("umage".into(), side(&["csm", "rs"], GridPreset::default()));
    layouts.insert("ops".into(), side(&["build", "gw"], GridPreset::new(2, 1)));
    layouts.insert("all".into(), side(&["odulodo", "web", "csm", "gw"], GridPreset::default()));

    let folder = |name: &str, path: &str, branch: &str, tag: &str, open: u32| FolderSuggestion {
        name: name.into(),
        path: path.into(),
        branch: Some(branch.into()),
        tag: tag.into(),
        open,
        recent: tag.starts_with("Recent"),
    };
    let folders = vec![
        folder("Odulodo.Admin", "C:\\GitHub\\Odulodo.Admin", "main", "Repo under C:\\GitHub", 0),
        folder("Odulodo", "C:\\GitHub\\Odulodo", "main", "Recent · 2 h", 1),
        folder("Odulodo.Web", "C:\\GitHub\\Odulodo.Web", "feat/brand-facet", "Recent · 20 min", 2),
        folder("Odulodo.Infra", "C:\\GitHub\\Odulodo.Infra", "main", "Repo under C:\\GitHub", 0),
        folder("CodeShellManager", "C:\\GitHub\\CodeShellManager", "main", "Recent · 1 h", 1),
        folder("CodeShellManager-rs", "C:\\GitHub\\CodeShellManager-rs", "phase0/term-spike", "Recent · 5 min", 1),
        folder("craft-anything", "C:\\src\\craft-anything", "main", "Recent · 3 h", 1),
        folder("OptimizelyNewsletter", "C:\\GitHub\\OptimizelyNewsletter", "main", "Asleep in Umage", 0),
        folder("Umage.Website", "C:\\GitHub\\Umage.Website", "develop", "Repo under C:\\GitHub", 0),
        folder("AITestTool", "C:\\GitHub\\AITestTool", "main", "Repo under C:\\GitHub", 0),
    ];
    let mut notes = BTreeMap::new();
    notes.insert(
        "C:\\GitHub\\Odulodo.Web".to_string(),
        "## Odulodo.Web\n- Brand facet: use brand.keyword, the\n  analyzer lowercases brand\n- Ask Allan before touching the CMS\n  listing sort\n- Staging runs on build-01, /srv/odulodo\n\nTODO\n- [ ] snapshot tests for FacetPanel\n- [x] category facet size 20 -> 30".to_string(),
    );
    UiModel {
        remote: false,
        stale: false,
        // The built-in Claude usage add-on is on, so the screenshots show usage.
        addons: vec![demo_claude_usage(true)],
        remotes: Vec::new(),
        slash_presets: Vec::new(),
        sessions: s,
        groups,
        group_mode: GroupMode::Rail,
        rail: Scope::Group("odulodo".into()),
        layouts,
        custom_grids: Vec::new(),
        active: Some("web".into()),
        inspector: None,
        inbox_panel: InboxPanel::default(),
        inspector_width: None,
        restore: None,
        shutdown: None,
        folders,
        launch_commands: vec!["claude".into(), "pwsh".into(), "claude --model opus".into(), "npm run dev".into()],
        notes,
        theme,
        now_ms: now,
        wpf_import: None,
        git: crate::git_model::demo_details(),
        usage: crate::usage_model::demo_details(),
        docs: crate::docs_model::demo_details(),
        folder_worktrees: BTreeMap::new(),
        open_with: demo_open_with(),
        pending_open_with: Vec::new(),
        update: None,
        windows: BTreeMap::new(),
        recently_closed: Vec::new(),
        tasks: std::sync::Arc::new(demo_tasks(now)),
    }
}

/// The Board's demo tasks (Odulodo.Web's project): one in each task column.
fn demo_tasks(now: u64) -> Vec<csm_core::tasks::Task> {
    use csm_core::tasks::{Task, TaskState};
    let at = i64::try_from(now).unwrap_or(0);
    let task = |id: &str, title: &str, state: TaskState, assignee: Option<&str>, order: i64| Task {
        id: id.into(),
        project: "repo:C:\\GitHub\\Odulodo.Web".into(),
        title: title.into(),
        body: String::new(),
        state,
        assignee: assignee.map(str::to_owned),
        created_by: csm_core::tasks::CREATED_BY_PERSON.into(),
        source: "ui".into(),
        progress: None,
        order,
        created_ms: at - 3_600_000,
        updated_ms: at - 600_000,
    };
    vec![
        task("t1", "Snapshot tests for FacetPanel", TaskState::Backlog, None, 0),
        task("t2", "Category facet size 20 to 30", TaskState::Backlog, None, 1),
        task("t3", "Brand facet uses brand.keyword", TaskState::Review, Some("web"), 0),
        task("t4", "Bump the Elasticsearch client", TaskState::Done, Some("web"), 0),
    ]
}

/// The Recent menu's order in the demo: minutes since each session was last focused (the
/// active one, `web`, just now; `stg` never).
const LAST_USED_MIN: [(&str, u64); 10] = [
    ("web", 0),
    ("odulodo", 3),
    ("csm", 12),
    ("webps", 25),
    ("wt", 50),
    ("rs", 95),
    ("news", 60 * 5),
    ("gw", 60 * 26),
    ("build", 60 * 50),
    ("craft", 60 * 24 * 9),
];

/// The mock-up's Open with list.
pub fn demo_open_with() -> Vec<OpenWithEntryView> {
    let e = |id: &str, label: &str, exe: &str, remote: bool, default: bool| OpenWithEntryView {
        id: id.into(),
        label: label.into(),
        executable: exe.into(),
        program: exe.rsplit(['\\', '/']).next().unwrap_or(exe).into(),
        local_args: Some("{folder}".into()),
        wsl_args: remote.then(|| "--folder-uri vscode-remote://wsl+{distro:uri}{linuxPath:uri}".into()),
        ssh_args: remote.then(|| "--folder-uri vscode-remote://ssh-remote+{userAtHost:uri}{remoteFolder:uri}".into()),
        is_default: default,
    };
    vec![
        e("vscode", "VS Code", r"C:\Users\you\AppData\Local\Programs\Microsoft VS Code\Code.exe", true, true),
        e("vs", "Visual Studio 2026", r"C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\devenv.exe", false, false),
        e("rider", "Rider", r"C:\Users\you\AppData\Local\Programs\Rider\bin\rider64.exe", false, false),
    ]
}

/// What the demo's menus show for a session: the entries for its kind, with a plausible command.
fn demo_targets(kind: Kind, folder: &str) -> Vec<OpenWithItem> {
    demo_open_with()
        .into_iter()
        .filter(|e| kind == Kind::Local || e.ssh_args.is_some())
        .map(|e| OpenWithItem {
            command: format!("\"{}\" \"{folder}\"", e.executable),
            entry_id: e.id,
            label: e.label,
            program: e.program,
            is_default: e.is_default,
        })
        .collect()
}

fn parse<P: DeserializeOwned>(call: &Call) -> Option<P> {
    serde_json::from_value(call.params.clone()).ok()
}

impl DemoBackend {
    pub fn new(opts: DemoOptions) -> Self {
        let now = opts.now_ms.unwrap_or_else(now_ms);
        let model = demo_model(now, opts.theme);
        let mut b = DemoBackend {
            opts,
            snap: Arc::new(model.clone()),
            model,
            terms: HashMap::new(),
            static_ids: std::collections::BTreeSet::new(),
            closed: Vec::new(),
            next_id: 1,
            started: Instant::now(),
            quitting: None,
            restoring: None,
            group_focus: HashMap::new(),
            screens: Arc::default(),
            remote: None,
            panes: Arc::default(),
        };
        let ids: Vec<SessionId> = b.model.sessions.iter().filter(|s| s.status.is_live()).map(|s| s.id.clone()).collect();
        for id in ids {
            b.make_terminal(&id);
        }
        b
    }

    /// Mutable access for tests and screenshot setups (e.g. open the inspector on a tab).
    pub fn model_mut(&mut self) -> &mut UiModel {
        &mut self.model
    }

    /// Publish edits made through [`DemoBackend::model_mut`].
    pub fn publish(&mut self) {
        if self.opts.now_ms.is_none() {
            self.model.now_ms = now_ms();
        }
        self.model.recently_closed = self
            .closed
            .iter()
            .map(|s| crate::model::ClosedSessionView { name: s.name.clone(), subtitle: s.folder.clone(), command: s.command.clone() })
            .collect();
        self.snap = Arc::new(self.model.clone());
    }

    /// Starts the restore rail animation (sessions come up one by one).
    pub fn start_restore(&mut self) {
        self.restoring = Some(Instant::now());
        let total = self.model.sessions.iter().filter(|s| s.status.is_live()).count() as u32;
        self.model.restore = Some(RestoreProgress { done: 0, total, current: None });
        self.publish();
    }

    /// A static frame of the restore rail at `done` of the live sessions (screenshots).
    pub fn set_restore(&mut self, done: u32) {
        let live: Vec<String> = self.model.sessions.iter().filter(|s| s.status.is_live()).map(|s| s.name.clone()).collect();
        self.model.restore = Some(RestoreProgress { done, total: live.len() as u32, current: live.get(done as usize).cloned() });
        self.publish();
    }

    /// A static frame of the shutdown board `t` seconds in (screenshots).
    pub fn set_shutdown(&mut self, t: f32) {
        self.model.shutdown = Some(self.board_at(t));
        self.publish();
    }

    fn board_at(&self, t: f32) -> ShutdownBoard {
        let rows = self
            .model
            .sessions
            .iter()
            .filter(|s| s.status.is_live())
            .enumerate()
            .map(|(i, s)| {
                let start = i as f32 * 1.6;
                let dur = if s.is_claude() { 2.3 + (i % 3) as f32 * 1.1 } else { 0.4 };
                let forced = s.id == "gw";
                let (state, elapsed) = if t < start {
                    (ShutdownRowState::Pending, 0.0)
                } else if forced && t >= start + 10.0 {
                    (ShutdownRowState::Forced, 10.0)
                } else if !forced && t >= start + dur {
                    (ShutdownRowState::Clean, dur)
                } else {
                    (ShutdownRowState::Closing, t - start)
                };
                ShutdownRow { session: s.id.clone(), state, elapsed_s: elapsed }
            })
            .collect();
        ShutdownBoard { rows, budget_used_s: t.min(30.0), budget_s: 30.0 }
    }

    /// The theme mode setting (`dark`, `light`, `system`; `mocha` and `latte` too).
    fn set_theme_mode(&mut self, v: &str) {
        self.model.theme = ThemeKind::from_id(v).unwrap_or_else(|| crate::engine_backend::theme_from_setting(v));
        // Transcripts carry theme colours in their bytes: redraw them. Live terminals
        // recolour by themselves (the view draws with the theme's palette).
        let ids: Vec<SessionId> = self.static_ids.iter().cloned().collect();
        for id in ids {
            self.make_terminal(&id);
        }
    }

    fn make_terminal(&mut self, id: &str) {
        let Some(s) = self.model.session(id).cloned() else { return };
        let live_shell = self.opts.live && !s.is_claude() && s.kind == Kind::Local;
        let source: Arc<dyn TerminalSource> = if live_shell {
            let repaint = self.opts.repaint.clone().unwrap_or_else(|| Arc::new(|| {}));
            let palette = Tokens::for_kind(self.model.theme).terminal_style().palette;
            Arc::new(LocalTerminal::new(s.name.clone(), self.opts.shell.clone(), self.opts.cwd.clone(), palette, repaint))
        } else {
            let rows = usize::from(TRANSCRIPT.rows);
            let dark = self.model.theme == ThemeKind::Mocha;
            let bytes = transcripts::for_session(id, s.status, rows, dark)
                .unwrap_or_else(|| if s.is_claude() { transcripts::welcome(&s.folder, rows, dark) } else { b"$ ".to_vec() });
            Arc::new(StaticTerminal::new(TRANSCRIPT, &bytes))
        };
        if live_shell {
            self.static_ids.remove(id);
        } else {
            self.static_ids.insert(id.to_string());
        }
        self.terms.insert(id.to_string(), source);
    }

    fn session_mut(&mut self, id: &str) -> Option<&mut SessionView> {
        self.model.sessions.iter_mut().find(|s| s.id == id)
    }

    fn layout_mut(&mut self, key: &str) -> &mut Layout {
        self.model.layouts.entry(key.to_string()).or_default()
    }

    /// The scope a session's layout changes go to: the panes' scope.
    fn scope_key(&self) -> String {
        self.model.pane_scope().key()
    }

    fn set_status(&mut self, id: &str, status: Status) {
        let now = self.model.now_ms;
        if let Some(s) = self.session_mut(id) {
            if s.status != status {
                s.status_since_ms = Some(now);
            }
            s.status = status;
            if status != Status::Working {
                s.progress = None;
            }
            if s.detail.is_empty() || !status.needs_you() && !status.unseen() {
                s.detail = match status {
                    Status::Approval => "Bash: npm test".into(),
                    Status::Question => "Which option should it use?".into(),
                    Status::Done => "The turn finished".into(),
                    Status::Error => "The turn ended with an error".into(),
                    _ => String::new(),
                };
            }
        }
        // A transcript follows its status where the demo has one for it.
        if !self.opts.live || self.model.session(id).is_some_and(SessionView::is_claude) {
            self.make_terminal(id);
        }
    }

    /// Focus: switch the rail tab to the session's group, wake it, acknowledge done/error.
    fn focus(&mut self, id: &str) -> Vec<Notice> {
        let mut out = Vec::new();
        let Some(s) = self.model.session(id).cloned() else { return out };
        if s.status == Status::Asleep {
            out.extend(self.wake(id));
        }
        if self.model.group_mode == GroupMode::Rail
            && self.model.rail != Scope::All
            && let Some(g) = &s.group
        {
            self.model.rail = Scope::Group(g.clone());
        }
        self.model.active = Some(id.to_string());
        if let Some(g) = &s.group {
            self.group_focus.insert(g.clone(), id.to_string());
        }
        // As the engine does: a minimized pane comes back, a different maximized one gives way.
        for (key, l) in self.model.layouts.iter_mut() {
            let in_scope = *key == Scope::All.key() || s.group.as_deref() == Some(key.as_str());
            l.reveal(id, in_scope);
        }
        let now = i64::try_from(self.model.now_ms).ok();
        if let Some(x) = self.session_mut(id) {
            x.last_used_ms = now;
        }
        if s.status.unseen() {
            // Seen now: done/error become idle (the mock acknowledges after a moment).
            self.set_status(id, Status::Idle);
        }
        out.push(Notice::Focus(id.to_string()));
        out
    }

    fn wake(&mut self, id: &str) -> Vec<Notice> {
        let Some(s) = self.model.session(id).cloned() else { return vec![] };
        let next = if s.is_claude() { Status::Working } else { Status::Idle };
        self.set_status(id, next);
        self.make_terminal(id);
        let how = if s.is_claude() { " and resumed its conversation." } else { "." };
        vec![Notice::Toast(format!("{} woke up{how}", s.name))]
    }

    fn sleep(&mut self, id: &str) -> Vec<Notice> {
        let Some(s) = self.model.session(id).cloned() else { return vec![] };
        self.set_status(id, Status::Asleep);
        self.terms.remove(id);
        if self.model.active.as_deref() == Some(id) {
            self.model.active = self
                .model
                .sessions
                .iter()
                .find(|x| x.group == s.group && x.status.is_live())
                .or_else(|| self.model.sessions.iter().find(|x| x.status.is_live()))
                .map(|x| x.id.clone());
        }
        vec![Notice::Toast(format!("{} is asleep. Click it in the sidebar to wake it.", s.name))]
    }

    fn close(&mut self, id: &str) -> Vec<Notice> {
        let Some(pos) = self.model.sessions.iter().position(|s| s.id == id) else { return vec![] };
        let s = self.model.sessions.remove(pos);
        self.terms.remove(id);
        for l in self.model.layouts.values_mut() {
            l.pinned.retain(|p| p != id);
            l.floats.retain(|f| f.session != id);
            l.torn.retain(|t| t != id);
            l.minimized.retain(|m| m != id);
            if l.maximized.as_deref() == Some(id) {
                l.maximized = None;
            }
            l.tree = l.tree.as_ref().and_then(|t| t.without(id));
        }
        if self.model.active.as_deref() == Some(id) {
            self.model.active = self.model.sessions.iter().find(|x| x.status.is_live()).map(|x| x.id.clone());
        }
        let name = s.name.clone();
        self.closed.insert(0, s);
        self.closed.truncate(10);
        vec![Notice::Toast(format!("Closed {name}. Ctrl+Shift+T reopens it."))]
    }

    fn new_session(&mut self, p: SessionNewParams) -> Vec<Notice> {
        let id = format!("n{}", self.next_id);
        self.next_id += 1;
        let program = p.command.clone().unwrap_or_else(|| "claude".into());
        let command = match p.args.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => format!("{program} {a}"),
            None => program,
        };
        let kind: Kind = p.kind.clone().map(Into::into).unwrap_or_default();
        let leaf = p.folder.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(&p.folder).to_string();
        let first = command.split_whitespace().next().unwrap_or("claude").to_string();
        let name = p.name.clone().unwrap_or_else(|| match kind {
            Kind::Ssh => p.ssh.as_ref().map_or(leaf.clone(), |s| s.host.clone()),
            Kind::Wsl => p.wsl.as_ref().map_or(leaf.clone(), |w| format!("{} {}", w.distro, w.folder.clone().unwrap_or_default())),
            Kind::Local if first == "claude" => leaf.clone(),
            Kind::Local => format!("{leaf} ({first})"),
        });
        let group = p.group.clone().or_else(|| self.model.pane_scope().key().ne("all").then(|| self.model.pane_scope().key()));
        let branch = self.model.folders.iter().find(|f| f.path == p.folder).and_then(|f| f.branch.clone());
        let folder = match (&kind, &p.ssh, &p.wsl) {
            (Kind::Ssh, Some(s), _) => {
                let user = s.user.clone().map(|u| format!("{u}@")).unwrap_or_default();
                format!("{user}{}:{}", s.host, s.remote_folder.clone().unwrap_or_default())
            }
            (Kind::Wsl, _, Some(w)) => format!("{} · {}", w.distro, w.folder.clone().unwrap_or_else(|| "~".into())),
            _ => p.folder.clone(),
        };
        let accent = (csm_core::color::palette().iter().position(|c| *c == csm_core::color::hex_color(&folder)).unwrap_or(7)) as u8;
        let s = SessionView {
            id: id.clone(),
            name: name.clone(),
            group: group.clone(),
            kind,
            command: command.clone(),
            folder: folder.clone(),
            branch,
            status: Status::Idle,
            accent: Accent::Palette(accent),
            started: "just now".into(),
            look: p.look.clone().filter(|l| !l.is_empty()),
            ..SessionView::default()
        };
        let mut s = s;
        // A folder in the fixture repository: its repo root, and (in a linked worktree) the
        // main checkout's session it nests under, as the engine backend's mapping does.
        if kind == Kind::Local && crate::git_model::demo_worktrees(&folder).is_some() {
            let root = crate::git_model::DEMO_REPO;
            s.repo_root = Some(root.into());
            if crate::git_model::folder_key(&folder) != crate::git_model::folder_key(root) {
                s.worktree_of = Some("Odulodo.Web".into());
                s.parent = self.model.sessions.iter().find(|x| x.repo_root.as_deref() == Some(root) && x.worktree_of.is_none()).map(|x| x.id.clone());
            }
        }
        self.model.sessions.push(s);
        if self.opts.live && first != "claude" && kind == Kind::Local {
            let repaint = self.opts.repaint.clone().unwrap_or_else(|| Arc::new(|| {}));
            let argv: Vec<OsString> = command.split_whitespace().map(OsString::from).collect();
            let cwd = (!p.folder.is_empty()).then(|| PathBuf::from(&p.folder));
            let palette = Tokens::for_kind(self.model.theme).terminal_style().palette;
            self.terms.insert(id.clone(), Arc::new(LocalTerminal::new(name.clone(), argv, cwd, palette, repaint)));
        } else {
            self.make_terminal(&id);
        }
        let key = group.clone().unwrap_or_else(|| "all".into());
        self.layout_mut(&key).pinned.push(id.clone());
        let mut out = Vec::new();
        if p.focus.unwrap_or(true) {
            out.extend(self.focus(&id));
        }
        out.push(Notice::Toast(format!("Started {command} in {folder}.")));
        out
    }

    fn apply(&mut self, call: &Call) -> Vec<Notice> {
        let toast = |s: String| vec![Notice::Toast(s)];
        let sid = parse::<SessionIdParams>(call).map(|p| p.session_id);
        let name_of = |me: &Self, id: &Option<String>| -> String {
            id.as_deref().and_then(|i| me.model.session(i)).map_or_else(|| "the session".into(), |s| s.name.clone())
        };
        match call.id.as_str() {
            // The dashboard: built from the demo's request history, like the engine would.
            "usage_report" => {
                let names = self.model.sessions.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
                match crate::usage_model::demo_report(&call.params, &names) {
                    Some(v) => {
                        self.model.usage.report = Some(v);
                        self.model.usage.report_error = None;
                    }
                    None => self.model.usage.report_error = Some("Not a date: use YYYY-MM-DD.".into()),
                }
                vec![]
            }
            // The built-in Claude usage add-on, turned on and off as the engine does.
            "addon_enable" | "addon_disable" if call.params["id"] == csm_engine::addons::CLAUDE_USAGE_ID => {
                let on = call.id == "addon_enable";
                self.model.addons.retain(|a| a.id != csm_engine::addons::CLAUDE_USAGE_ID);
                self.model.addons.insert(0, demo_claude_usage(on));
                vec![]
            }
            "session_new" => parse::<SessionNewParams>(call).map(|p| self.new_session(p)).unwrap_or_default(),
            "session_focus" => sid.map(|id| self.focus(&id)).unwrap_or_default(),
            "session_wake" => sid.map(|id| self.wake(&id)).unwrap_or_default(),
            "session_sleep" => sid.map(|id| self.sleep(&id)).unwrap_or_default(),
            "session_close" => parse::<CloseParams>(call).map(|p| self.close(&p.session_id)).unwrap_or_default(),
            "session_restart" => {
                let n = name_of(self, &sid);
                if let Some(id) = &sid {
                    self.make_terminal(id);
                }
                toast(format!("Restarting {n}… it resumes its conversation."))
            }
            "session_rename" => {
                if let Some(p) = parse::<RenameParams>(call)
                    && let Some(s) = self.session_mut(&p.session_id)
                {
                    s.name = p.name;
                }
                vec![]
            }
            "session_edit" => {
                if let Some(p) = parse::<SessionEditParams>(call)
                    && let Some(s) = self.session_mut(&p.session_id)
                {
                    if let Some(n) = p.name {
                        s.name = n;
                    }
                    if let Some(c) = p.command {
                        s.command = c;
                    }
                    if let Some(f) = p.folder {
                        s.folder = f;
                    }
                }
                toast("Saved. The change applies on the next restart.".into())
            }
            "session_reopen_closed" => {
                let index = call.params.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).unwrap_or(0);
                if index >= self.closed.len() {
                    return toast("Nothing was closed recently.".into());
                }
                let s = self.closed.remove(index);
                let id = s.id.clone();
                let name = s.name.clone();
                self.model.sessions.push(s);
                self.make_terminal(&id);
                let mut out = self.focus(&id);
                out.push(Notice::Toast(format!("Reopened {name}.")));
                out
            }
            "inbox_next" => {
                let list: Vec<SessionId> = self.model.needs_you().iter().chain(self.model.unseen().iter()).map(|s| s.id.clone()).collect();
                if list.is_empty() {
                    return toast("Nothing needs you.".into());
                }
                let i = self.model.active.as_ref().and_then(|a| list.iter().position(|x| x == a));
                let next = i.map_or(0, |i| (i + 1) % list.len());
                list.get(next).cloned().map(|id| self.focus(&id)).unwrap_or_default()
            }
            "session_look_set" => {
                if let Some(p) = parse::<LookSetParams>(call)
                    && let Some(s) = self.session_mut(&p.session_id)
                {
                    // The demo has no Windows Terminal: a profile by name changes nothing.
                    if p.clear {
                        s.look = None;
                    } else if let Some(l) = p.look {
                        s.look = Some(l).filter(|l| !l.is_empty());
                    }
                }
                vec![]
            }
            "session_accent_reset" => {
                if let Some(id) = &sid
                    && let Some(s) = self.session_mut(id)
                {
                    s.accent = Accent::Palette(2);
                    s.accent_overridden = false;
                }
                vec![]
            }
            "session_status_clear" => {
                if let Some(id) = &sid {
                    self.set_status(id, Status::Idle);
                }
                toast("Status cleared. The heuristic watches this session again.".into())
            }
            "demo_set_status" => {
                if let Some(p) = parse::<DemoStatusParams>(call)
                    && let Some(st) = Status::from_id(&p.status)
                {
                    if st == Status::Asleep {
                        return self.sleep(&p.session_id);
                    }
                    self.set_status(&p.session_id, st);
                    if st == Status::Working
                        && let Some(s) = self.session_mut(&p.session_id)
                    {
                        s.progress = None;
                    }
                }
                vec![]
            }
            "session_send_text" => {
                if let Some(p) = parse::<SendTextParams>(call)
                    && let Some(t) = self.terms.get(&p.session_id)
                {
                    t.write(&csm_term::input::paste(csm_term::input::Untrusted(&p.text), t.mode()));
                    if p.submit {
                        t.write(b"\r");
                    }
                }
                toast("Sent.".into())
            }
            "session_send_text_many" => {
                // As the engine: every live member of the group that isn't mid-turn.
                let Some(text) = call.params["text"].as_str() else { return vec![] };
                let submit = call.params["submit"].as_bool().unwrap_or(false);
                let group = call.params["group_id"].as_str();
                let ids: Vec<SessionId> = match (group, call.params["session_ids"].as_array()) {
                    (Some(g), _) => self.model.sessions.iter().filter(|s| s.group.as_deref() == Some(g)).map(|s| s.id.clone()).collect(),
                    (None, Some(list)) => list.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
                    _ => vec![],
                };
                let (mut sent, mut skipped) = (0, 0);
                for id in ids {
                    let ready =
                        self.model.session(&id).is_some_and(|s| s.status.is_live() && !matches!(s.status, Status::Working | Status::Starting));
                    match self.terms.get(&id).filter(|_| ready) {
                        Some(t) => {
                            t.write(&csm_term::input::paste(csm_term::input::Untrusted(text), t.mode()));
                            if submit {
                                t.write(b"\r");
                            }
                            sent += 1;
                        }
                        None => skipped += 1,
                    }
                }
                toast(format!("Sent to {sent} sessions; skipped {skipped}."))
            }
            "session_send_command" => {
                // The demo's sessions aren't the engine's: type the command into the demo terminal.
                let sid = call.params["session_id"].as_str().unwrap_or_default();
                let Some(cmd) = call.params["command"].as_str() else { return vec![] };
                if let Some(t) = self.terms.get(sid) {
                    t.write(&csm_term::input::typed(cmd, false).0);
                    t.write(b"\r");
                }
                toast(format!("Sent {cmd}."))
            }
            "group_new" => {
                let Some(p) = parse::<GroupNewParams>(call) else { return vec![] };
                let id = format!("g{}", self.next_id);
                self.next_id += 1;
                self.model.groups.push(GroupView { id: id.clone(), name: p.name.clone(), torn_off: false, collapsed: false });
                if self.model.group_mode == GroupMode::Rail {
                    self.model.rail = Scope::Group(id);
                }
                toast(format!("Created {}. Drag sessions onto it, or use Move to group.", p.name))
            }
            "group_rename" => {
                if let Some(p) = parse::<GroupRenameParams>(call)
                    && let Some(g) = self.model.groups.iter_mut().find(|g| g.id == p.group_id)
                {
                    g.name = p.name;
                }
                vec![]
            }
            "group_delete" => {
                let Some(p) = parse::<GroupIdParams>(call) else { return vec![] };
                self.model.groups.retain(|g| g.id != p.group_id);
                for s in &mut self.model.sessions {
                    if s.group.as_deref() == Some(p.group_id.as_str()) {
                        s.group = None;
                    }
                }
                if self.model.rail == Scope::Group(p.group_id.clone()) {
                    self.model.rail = Scope::All;
                }
                toast("Deleted the group. Its sessions are in Ungrouped; nothing was closed.".into())
            }
            "session_move_to_group" => {
                let Some(p) = parse::<MoveToGroupParams>(call) else { return vec![] };
                let gname = p.group_id.as_deref().and_then(|g| self.model.group(g)).map_or("Ungrouped".to_string(), |g| g.name.clone());
                if let Some(s) = self.session_mut(&p.session_id) {
                    s.group = p.group_id.clone();
                }
                // Before / after an anchor, found after taking the moved session out (as the
                // engine does), so a downward move never overshoots.
                let anchor = p.before.clone().map(|a| (a, false)).or_else(|| p.after.clone().map(|a| (a, true)));
                if let Some((a, after)) = anchor
                    && a != p.session_id
                    && let Some(i) = self.model.sessions.iter().position(|s| s.id == p.session_id)
                {
                    let moved = self.model.sessions.remove(i);
                    let at = self.model.sessions.iter().position(|s| s.id == a).map_or(self.model.sessions.len(), |j| if after { j + 1 } else { j });
                    self.model.sessions.insert(at.min(self.model.sessions.len()), moved);
                    return vec![];
                }
                toast(format!("Moved to {gname}."))
            }
            "group_sleep_all" => {
                let Some(p) = parse::<GroupIdParams>(call) else { return vec![] };
                let ids: Vec<SessionId> = self
                    .model
                    .sessions
                    .iter()
                    .filter(|s| s.group.as_deref() == Some(p.group_id.as_str()) && s.status.is_live())
                    .map(|s| s.id.clone())
                    .collect();
                for id in &ids {
                    self.sleep(id);
                }
                let g = self.model.group(&p.group_id).map_or_else(String::new, |g| g.name.clone());
                toast(format!("{} sessions in {g} are asleep.", ids.len()))
            }
            "group_close_all" => {
                let Some(p) = parse::<GroupForceParams>(call) else { return vec![] };
                let ids: Vec<SessionId> =
                    self.model.sessions.iter().filter(|s| s.group.as_deref() == Some(p.group_id.as_str())).map(|s| s.id.clone()).collect();
                for id in &ids {
                    self.close(id);
                }
                toast(format!("Closed {} sessions. Ctrl+Shift+T reopens them one by one.", ids.len()))
            }
            "group_restart_all" => {
                let g = parse::<GroupForceParams>(call).and_then(|p| self.model.group(&p.group_id).map(|g| g.name.clone())).unwrap_or_default();
                toast(format!("Restarting every session in {g}, one at a time."))
            }
            "window_placement_set" => {
                // The demo engine knows none of the demo's groups or sessions: keep it here.
                let p = &call.params;
                let n = |k: &str| p.get(k).and_then(serde_json::Value::as_i64).and_then(|v| i32::try_from(v).ok()).unwrap_or(0);
                if let Some(w) = p.get("window").and_then(serde_json::Value::as_str) {
                    let placement = csm_core::models::WindowPlacementState {
                        left: n("left"),
                        top: n("top"),
                        width: n("width"),
                        height: n("height"),
                        dpi: n("dpi"),
                    };
                    let maximized = p.get("maximized").and_then(serde_json::Value::as_bool).unwrap_or(false);
                    self.model.windows.insert(w.to_string(), csm_core::models::WindowSlot { placement, maximized });
                }
                vec![]
            }
            "session_sleep_many" | "session_wake_many" | "session_restart_many" | "session_close_many" => {
                let Some(p) = parse::<SessionIdsParams>(call) else { return vec![] };
                let mut n = 0;
                for id in &p.session_ids {
                    let live = self.model.session(id).is_some_and(|s| s.status.is_live());
                    match call.id.as_str() {
                        "session_sleep_many" if live => drop(self.sleep(id)),
                        "session_wake_many" if !live => drop(self.wake(id)),
                        "session_restart_many" if live => self.make_terminal(id),
                        "session_close_many" => drop(self.close(id)),
                        _ => continue,
                    }
                    n += 1;
                }
                toast(match call.id.as_str() {
                    "session_sleep_many" => format!("{n} sessions are asleep."),
                    "session_wake_many" => format!("Waking {n} sessions."),
                    "session_restart_many" => format!("Restarting {n} sessions, one at a time."),
                    _ => format!("Closed {n} sessions. Ctrl+Shift+T reopens them one by one."),
                })
            }
            "session_move_many" => {
                let Some(p) = parse::<MoveManyParams>(call) else { return vec![] };
                let ids: Vec<SessionId> = self.model.sessions.iter().filter(|s| p.session_ids.contains(&s.id)).map(|s| s.id.clone()).collect();
                for id in &ids {
                    if let Some(s) = self.session_mut(id) {
                        s.group = p.group_id.clone();
                    }
                }
                // As the engine: the block goes before / after the anchor (or after the group's
                // last other row), in sidebar order.
                let anchor = p.before.clone().map(|a| (a, false)).or_else(|| p.after.clone().map(|a| (a, true))).or_else(|| {
                    self.model.sessions.iter().rev().find(|s| s.group == p.group_id && !ids.contains(&s.id)).map(|s| (s.id.clone(), true))
                });
                if let Some((a, after)) = anchor.filter(|(a, _)| !ids.contains(a)) {
                    let block: Vec<SessionView> = self.model.sessions.iter().filter(|s| ids.contains(&s.id)).cloned().collect();
                    self.model.sessions.retain(|s| !ids.contains(&s.id));
                    let at = self.model.sessions.iter().position(|s| s.id == a).map_or(self.model.sessions.len(), |j| if after { j + 1 } else { j });
                    let at = at.min(self.model.sessions.len());
                    self.model.sessions.splice(at..at, block);
                }
                let gname = p.group_id.as_deref().and_then(|g| self.model.group(g)).map_or("Ungrouped".to_string(), |g| g.name.clone());
                toast(format!("Moved {} sessions to {gname}.", ids.len()))
            }
            "session_sort" => {
                use csm_core::session_sort::{self as sort, SortField, SortKey};
                let Some(p) = parse::<SortParams>(call) else { return vec![] };
                let Some(field) = SortField::from_id(&p.by) else { return vec![] };
                let desc = p.descending.unwrap_or_else(|| field.default_descending());
                let scope: Option<Option<String>> = match p.group.as_deref() {
                    None | Some("all") => None,
                    Some("") => Some(None),
                    Some(g) => Some(Some(g.to_string())),
                };
                let in_scope = |s: &SessionView| scope.as_ref().is_none_or(|g| &s.group == g);
                let keys: Vec<SortKey> = self
                    .model
                    .sessions
                    .iter()
                    .filter(|s| in_scope(s))
                    .map(|s| SortKey {
                        id: s.id.clone(),
                        name: s.name.clone(),
                        folder: s.folder.clone(),
                        last_active_ms: s.status_since_ms.and_then(|m| i64::try_from(m).ok()),
                        branch: s.branch.clone(),
                        dirty: s.status.is_live().then_some(s.dirty),
                        repo_root: s.worktree_of.clone().or_else(|| s.branch.as_ref().map(|_| s.folder.clone())),
                    })
                    .collect();
                let order = sort::sorted_ids(&keys, field, desc);
                sort::reorder_in_place(&mut self.model.sessions, |s| s.id.as_str(), in_scope, &order);
                toast(format!("Sorted {} sessions by {}.", order.len(), field.label().to_lowercase()))
            }
            "session_duplicate" => {
                let Some(p) = parse::<DuplicateParams>(call) else { return vec![] };
                let Some(i) = self.model.sessions.iter().position(|s| s.id == p.session_id) else { return vec![] };
                let Some(parent) = self.model.sessions.get(i).cloned() else { return vec![] };
                let taken: Vec<String> = self.model.sessions.iter().map(|s| s.name.clone()).collect();
                let id = format!("n{}", self.next_id);
                self.next_id += 1;
                let name = csm_core::session_names::duplicate_name(&parent.name, &taken);
                let copy = SessionView {
                    id: id.clone(),
                    name: name.clone(),
                    status: Status::Idle,
                    detail: String::new(),
                    status_since_ms: None,
                    accent_overridden: false,
                    started: "just now".into(),
                    ..parent.clone()
                };
                self.model.sessions.insert(i + 1, copy);
                self.make_terminal(&id);
                let mut out = if p.focus.unwrap_or(true) { self.focus(&id) } else { vec![] };
                out.push(Notice::Toast(format!("Duplicated {} as {name}.", parent.name)));
                out
            }
            "group_tear_off" | "group_dock" => {
                let Some(p) = parse::<GroupIdParams>(call) else { return vec![] };
                let torn = call.id == "group_tear_off";
                if let Some(g) = self.model.groups.iter_mut().find(|g| g.id == p.group_id) {
                    g.torn_off = torn;
                    let name = g.name.clone();
                    if torn && self.model.rail == Scope::Group(p.group_id.clone()) {
                        self.model.rail = Scope::All;
                    }
                    return toast(if torn {
                        format!("{name} now has its own window. Closing it docks the group back.")
                    } else {
                        format!("{name} is back in the main window.")
                    });
                }
                vec![]
            }
            "view_set" => {
                let Some(p) = parse::<ViewSetParams>(call) else { return vec![] };
                if let Some(m) = p.view {
                    self.model.group_mode = m.into();
                    if self.model.group_mode == GroupMode::Rail
                        && let Some(g) = self.model.active_session().and_then(|s| s.group.clone())
                    {
                        self.model.rail = Scope::Group(g);
                    }
                }
                if let Some(r) = p.rail_tab {
                    let scope = Scope::from_key(&r);
                    self.model.rail = scope.clone();
                    // The panes follow the tab: keep the active session inside it.
                    let inside = self.model.active_session().is_some_and(|s| match &scope {
                        Scope::All => true,
                        Scope::Group(g) => s.group.as_deref() == Some(g.as_str()),
                    });
                    if !inside {
                        // The session last focused in the group, else its first live one (#137).
                        let remembered = match &scope {
                            Scope::Group(g) => self
                                .group_focus
                                .get(g)
                                .and_then(|id| self.model.session(id))
                                .filter(|s| s.group.as_deref() == Some(g.as_str()) && s.status.is_live())
                                .map(|s| s.id.clone()),
                            Scope::All => None,
                        };
                        let first =
                            remembered.or_else(|| self.model.scope_sessions(&scope).iter().find(|s| s.status.is_live()).map(|s| s.id.clone()));
                        if first.is_none() {
                            return toast(format!("Every session in {} is asleep. Click one to wake it.", self.model.scope_name(&scope)));
                        }
                        self.model.active = first;
                    }
                }
                vec![]
            }
            "layout_set" => {
                let Some(p) = parse::<LayoutSetParams>(call) else { return vec![] };
                let key = p.group.clone().unwrap_or_else(|| self.scope_key());
                let mut remember = None;
                let l = self.layout_mut(&key);
                if let Some(m) = p.mode {
                    l.mode = m.into();
                }
                let preset = p.preset.as_deref().and_then(csm_core::models::parse_grid_preset).map(|(c, r)| GridPreset::new(c, r));
                if let Some(g) = preset {
                    l.grid = g;
                    l.mode = LayoutMode::Grid;
                }
                if p.cols.is_some() || p.rows.is_some() {
                    l.grid = GridPreset::new(p.cols.unwrap_or(l.grid.cols), p.rows.unwrap_or(l.grid.rows));
                    l.mode = LayoutMode::Grid;
                    remember = Some(l.grid);
                }
                if let Some(f) = &p.free {
                    l.set_free(FreeLayout::from_value(Some(f)));
                }
                // As the engine: a custom size is remembered for the Grid menu, newest first.
                if let Some(g) = remember.filter(|g| !g.is_preset()) {
                    self.model.custom_grids.retain(|x| *x != g);
                    self.model.custom_grids.insert(0, g);
                    self.model.custom_grids.truncate(csm_core::models::MAX_CUSTOM_GRIDS);
                }
                vec![]
            }
            "grid_preset_forget" => {
                let Some(p) = parse::<GridParam>(call) else { return vec![] };
                self.model.custom_grids.retain(|g| (g.cols, g.rows) != (p.cols, p.rows));
                vec![]
            }
            "layout_pin" | "layout_unpin" => {
                let Some(p) = parse::<PinParams>(call) else { return vec![] };
                let key = p.group.clone().unwrap_or_else(|| self.scope_key());
                let pin = call.id == "layout_pin";
                let name = self.model.session(&p.session_id).map_or_else(String::new, |s| s.name.clone());
                let l = self.layout_mut(&key);
                l.pinned.retain(|x| x != &p.session_id);
                if pin {
                    l.pinned.push(p.session_id);
                }
                toast(if pin { format!("Pinned {name} to Side by side.") } else { format!("Unpinned {name}.") })
            }
            "pane_float" => {
                let Some(p) = parse::<PaneParams>(call) else { return vec![] };
                let key = p.group.clone().unwrap_or_else(|| self.scope_key());
                let live: Vec<SessionId> =
                    self.model.scope_sessions(&Scope::from_key(&key)).iter().filter(|s| s.status.is_live()).map(|s| s.id.clone()).collect();
                self.layout_mut(&key).float_pane(&p.session_id, &live);
                toast("Floating. Drag its header to move it, an edge or corner to resize it.".into())
            }
            "pane_dock" => {
                let Some(p) = parse::<PaneParams>(call) else { return vec![] };
                let key = p.group.clone().unwrap_or_else(|| self.scope_key());
                let was_torn = self.layout_mut(&key).dock_pane(&p.session_id);
                toast(if was_torn { "Docked back into the main window.".into() } else { "Docked to the right of the split tree.".into() })
            }
            "pane_tear_off" => {
                let Some(p) = parse::<PaneParams>(call) else { return vec![] };
                let key = p.group.clone().unwrap_or_else(|| self.scope_key());
                self.layout_mut(&key).tear_off_pane(&p.session_id);
                toast("In its own window now. Closing that window docks it back.".into())
            }
            "pane_minimize" | "pane_maximize" | "pane_restore" => {
                let Some(p) = parse::<PaneParams>(call) else { return vec![] };
                let key = p.group.clone().unwrap_or_else(|| self.scope_key());
                let l = self.layout_mut(&key);
                match call.id.as_str() {
                    "pane_minimize" => l.minimize_pane(&p.session_id),
                    "pane_maximize" => l.maximize_pane(&p.session_id),
                    _ => {
                        l.restore_pane(&p.session_id);
                    }
                }
                vec![]
            }
            "group_collapse" => {
                if let Some(p) = parse::<CollapseParams>(call)
                    && let Some(g) = self.model.groups.iter_mut().find(|g| g.id == p.group_id)
                {
                    g.collapsed = p.collapsed;
                }
                vec![]
            }
            "inspector_show" => {
                if let Some(p) = parse::<InspectorShowParams>(call) {
                    self.model.inspector = match p.tab {
                        InspectorParam::Notes => Some(InspectorTab::Notes),
                        InspectorParam::Runs => Some(InspectorTab::Runs),
                        InspectorParam::Git => Some(InspectorTab::Git),
                        InspectorParam::Session => Some(InspectorTab::Session),
                        InspectorParam::Usage => Some(InspectorTab::Usage),
                        InspectorParam::Docs => Some(InspectorTab::Docs),
                        InspectorParam::Hidden => None,
                    };
                }
                vec![]
            }
            "inbox_panel_set" => {
                let Some(p) = parse::<InboxPanelParams>(call) else { return vec![] };
                if let Some(o) = p.open {
                    self.model.inbox_panel.open = o;
                    if !o {
                        return toast("Inbox hidden. Ctrl+J still jumps to the next one; the badge next to Sessions brings it back.".into());
                    }
                }
                if let Some(h) = p.height.filter(|h| h.is_finite()) {
                    self.model.inbox_panel.height = h.clamp(crate::theme::size::INBOX_MIN_H, crate::theme::size::INBOX_MAX_H);
                }
                vec![]
            }
            "inspector_panel_set" => {
                let reset = call.params.get("reset").and_then(Value::as_bool).unwrap_or(false);
                self.model.inspector_width = if reset { None } else { call.params.get("width").and_then(Value::as_f64).map(|w| w as f32).filter(|w| w.is_finite()) };
                vec![]
            }
            "run_start" | "run_stop" => {
                let Some(p) = parse::<RunParams>(call) else { return vec![] };
                let start = call.id == "run_start";
                let Some(s) = self.session_mut(&p.session_id) else { return vec![] };
                let run = match &p.run_id {
                    Some(r) => s.runs.iter_mut().find(|x| &x.id == r),
                    None => {
                        let i = s.runs.iter().position(|r| r.is_default).unwrap_or(0);
                        s.runs.get_mut(i)
                    }
                };
                let Some(run) = run else { return toast("This session has no run commands yet.".into()) };
                let label = run.label.clone();
                if start {
                    run.state = RunState::Running;
                    run.detail = "running · just now".into();
                    toast(format!("Started {label} in its own background terminal."))
                } else {
                    run.state = RunState::NotRun;
                    run.detail = "stopped".into();
                    toast(format!("Stopped {label}."))
                }
            }
            "git_fetch" => toast("Fetched origin. Up to date.".into()),
            "git_pull" => toast("Pull (fast-forward only): already up to date.".into()),
            "git_switch_branch" => toast("Switched branch.".into()),
            "worktree_new" => toast("Created the worktree and opened a session in it.".into()),
            "worktree_remove" => toast("Refused: this worktree has uncommitted changes.".into()),
            "worktree_list" => {
                // The fixture's Odulodo.Web worktrees, asked by session (the row menu) or by
                // folder (the quick-open). Anything else is not a repository.
                let folder = call.params.get("folder").and_then(Value::as_str).map(str::to_string);
                match (folder, sid) {
                    (Some(f), _) => {
                        let rows = crate::git_model::demo_worktrees(&f).unwrap_or_default();
                        self.model.folder_worktrees.insert(crate::git_model::folder_key(&f), rows);
                    }
                    (None, Some(id)) => {
                        let rows = self.model.session(&id).and_then(|s| crate::git_model::demo_worktrees(&s.folder));
                        if let Some(rows) = rows {
                            self.model.git.entry(id).or_default().worktrees = Some(rows);
                        }
                    }
                    (None, None) => {}
                }
                vec![]
            }
            "open_folder" => toast(format!(
                "Opened {} in the file manager.",
                sid.as_deref().and_then(|i| self.model.session(i)).map_or("the folder", |s| s.folder.as_str())
            )),
            "open_with" => {
                let p = parse::<OpenWithParams>(call);
                let entry = p.as_ref().and_then(|p| p.entry.clone()).unwrap_or_else(|| "VS Code".into());
                toast(format!("Opening the folder in {entry}."))
            }
            "note_set" => {
                if let Some(p) = parse::<NoteSetParams>(call) {
                    let folder = p.folder.or_else(|| p.session_id.as_deref().and_then(|s| self.model.session(s)).map(|s| s.folder.clone()));
                    if let Some(f) = folder {
                        self.model.notes.insert(f, p.text);
                    }
                }
                vec![]
            }
            "settings_set" => {
                if let Some(p) = parse::<SettingsSetParams>(call)
                    && p.key.eq_ignore_ascii_case("theme")
                    && let Some(v) = p.value.as_str()
                {
                    self.set_theme_mode(v);
                }
                vec![]
            }
            "theme_set" => {
                if let Some(v) = call.params.get("mode").and_then(Value::as_str) {
                    self.set_theme_mode(v);
                }
                vec![]
            }
            "app_quit" => {
                self.quitting = Some(Instant::now());
                self.model.shutdown = Some(self.board_at(0.0));
                vec![]
            }
            _ => vec![],
        }
    }
}

impl Backend for DemoBackend {
    fn model(&self) -> Arc<UiModel> {
        self.snap.clone()
    }

    fn remote(&self) -> Option<crate::remote_window::RemoteStatus> {
        self.remote.clone()
    }

    fn show_panes(&mut self, visible: &[String], driver: Option<&str>) {
        if let Ok(mut log) = self.panes.lock() {
            *log = Some((visible.to_vec(), driver.map(str::to_owned)));
        }
    }

    fn terminal(&self, id: &str) -> Option<Arc<dyn TerminalSource>> {
        self.terms.get(id).cloned()
    }

    fn on_reply(&mut self, call: &Call, reply: &Reply, _port: &dyn ToolPort) -> Vec<Notice> {
        use csm_engine::ToolError;
        // The screen tools change the demo's own screens (the demo engine has no add-ons).
        if call.id.starts_with("screen_") && self.apply_screen_call(call) {
            return Vec::new();
        }
        let out = match reply {
            // The fixture's folders are Windows paths, which the engine refuses as relative on
            // other systems: the demo answers `worktree_list` from its fixture regardless.
            Err(e @ (ToolError::InvalidParams(_) | ToolError::UnknownTool(_))) if call.id != "worktree_list" => vec![Notice::Toast(e.to_string())],
            // The demo's sessions aren't the engine's: its "not found" (and the like) is about
            // the engine's empty state, not the demo's.
            _ => self.apply(call),
        };
        self.publish();
        out
    }

    fn on_event(&mut self, event: &Event) -> Vec<Notice> {
        // The demo engine's own toasts and quit are about its empty state: the demo shows its own.
        match event {
            Event::FocusRequested { session_id } => {
                let out = self.focus(session_id);
                self.publish();
                out
            }
            _ => vec![],
        }
    }

    fn screens(&self) -> Vec<csm_engine::ScreenInfo> {
        self.screen_lock().instances.clone()
    }

    fn screen_declared(&self) -> Vec<csm_engine::DeclaredScreen> {
        self.screen_lock().declared.clone()
    }

    fn screen_tree(&self, instance: &str) -> Option<Arc<csm_core::addons::screen::ScreenTree>> {
        self.screen_lock().trees.get(instance).cloned()
    }

    fn screen_event(&mut self, instance: &str, event: csm_core::addons::screen::ScreenEvent) -> Result<(), String> {
        let mut g = self.screen_lock();
        if let Some(m) = &g.refuse_events {
            return Err(m.clone());
        }
        g.events.push((instance.to_owned(), event));
        Ok(())
    }

    fn tick(&mut self, ctx: &egui::Context) -> Vec<Notice> {
        let _ = self.started;
        let mut out = Vec::new();
        if let Some(t0) = self.quitting {
            let t = t0.elapsed().as_secs_f32() * 2.0;
            self.model.shutdown = Some(self.board_at(t));
            self.publish();
            if t > 24.0 {
                self.quitting = None;
                out.push(Notice::QuitReady);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
            }
        }
        if let Some(t0) = self.restoring {
            let done = (t0.elapsed().as_secs_f32() / 0.9) as u32;
            let total = self.model.restore.as_ref().map_or(0, |r| r.total);
            if done > total {
                self.restoring = None;
                self.model.restore = None;
                self.publish();
            } else {
                self.set_restore(done);
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
            }
        }
        // OSC 52 copies from live demo shells (#156); the demo has no settings, so it is on.
        out.extend(self.terms.values().filter_map(|t| t.take_clipboard()).map(Notice::Clipboard));
        out
    }
}

impl DemoBackend {
    /// Value of a JSON param, for screens that want to show what a call did.
    pub fn describe(call: &Call) -> String {
        format!("{} {}", call.id, Value::to_string(&call.params))
    }
}

/// The add-on screens the demo backend shows: what a running add-on would have declared and
/// opened, with the validated trees, and the events the UI sent to its "plugin". A test keeps the
/// handle from [`DemoBackend::screens_handle`] and changes it while the app runs (a re-render is
/// replacing a tree here).
#[derive(Default)]
pub struct DemoScreens {
    pub declared: Vec<csm_engine::DeclaredScreen>,
    pub instances: Vec<csm_engine::ScreenInfo>,
    pub trees: HashMap<String, Arc<csm_core::addons::screen::ScreenTree>>,
    /// Events the UI sent, most recent last: (instance, event).
    pub events: Vec<(String, csm_core::addons::screen::ScreenEvent)>,
    /// When set, `screen_event` fails with this message (a busy plugin).
    pub refuse_events: Option<String>,
}

impl DemoBackend {
    /// Makes this demo stand for a remote window to `status`'s host (the strip, the hidden
    /// local-only items). The demo still answers its own sessions: it is a stand-in, not a link.
    pub fn set_remote(&mut self, status: Option<crate::remote_window::RemoteStatus>) {
        self.model.remote = status.is_some();
        self.model.stale = matches!(status, Some(crate::remote_window::RemoteStatus { link: crate::remote_window::RemoteLink::Offline, .. }));
        self.remote = status;
        self.snap = Arc::new(self.model.clone());
    }

    /// The panes the UI last reported through `Backend::show_panes` (a test reads it; `None` before
    /// the first frame).
    pub fn panes_log(&self) -> PanesLog {
        self.panes.clone()
    }

    /// The handle to the demo's add-on screens.
    pub fn screens_handle(&self) -> Arc<std::sync::Mutex<DemoScreens>> {
        self.screens.clone()
    }

    fn screen_lock(&self) -> std::sync::MutexGuard<'_, DemoScreens> {
        self.screens.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The effect of the screen tools on the demo's screens (the real engine applied it).
    fn apply_screen_call(&mut self, call: &Call) -> bool {
        let Some(instance) = call.params.get("instance").and_then(Value::as_str) else { return false };
        let mut g = self.screen_lock();
        match call.id.as_str() {
            "screen_close" => {
                g.instances.retain(|i| i.instance != instance);
                g.trees.remove(instance);
            }
            "screen_pop_out" | "screen_dock" => {
                let popped = call.id == "screen_pop_out";
                if let Some(i) = g.instances.iter_mut().find(|i| i.instance == instance) {
                    i.popped_out = popped;
                }
            }
            _ => return false,
        }
        true
    }
}

/// The built-in Claude usage add-on as the engine lists it, on or off.
pub fn demo_claude_usage(on: bool) -> csm_engine::addons::AddonSummary {
    let mut settings = csm_core::models::AppSettings::default();
    if on {
        settings.addon_approvals.insert(csm_engine::addons::CLAUDE_USAGE_ID.into(), csm_engine::addons::BUILTIN_DIGEST.into());
    }
    csm_engine::addons::claude_usage_summary(&settings)
}

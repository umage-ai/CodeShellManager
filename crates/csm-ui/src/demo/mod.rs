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

pub mod tools;
pub mod transcripts;

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use csm_engine::{Event, Reply};
use csm_term::GridSize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::backend::{Backend, Call, Notice};
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
}

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
    });
    set(&mut s, "build", &|x| {
        // A colour the remote's prompt hook pushed (OSC 9001): "Reset accent colour" applies.
        x.accent = Accent::Rgb([0xe0, 0x8f, 0x62]);
        x.accent_overridden = true;
    });
    set(&mut s, "wt", &|x| {
        x.parent = Some("web".into());
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
    set(&mut s, "craft", &|x| {
        x.dirty = true;
        x.status_since_ms = Some(now - mins(12));
        x.detail = "API error 529, the turn ended".into();
    });
    set(&mut s, "gw", &|x| x.progress = Some(70));

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
        sessions: s,
        groups,
        group_mode: GroupMode::Rail,
        rail: Scope::Group("odulodo".into()),
        layouts,
        active: Some("web".into()),
        inspector: None,
        inbox_panel: InboxPanel::default(),
        restore: None,
        shutdown: None,
        folders,
        launch_commands: vec!["claude".into(), "pwsh".into(), "claude --model opus".into(), "npm run dev".into()],
        notes,
        theme,
        now_ms: now,
        wpf_import: None,
        git: crate::git_model::demo_details(),
        open_with: demo_open_with(),
    }
}

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
            ..SessionView::default()
        };
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
                if self.closed.is_empty() {
                    return toast("Nothing was closed recently.".into());
                }
                let s = self.closed.remove(0);
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
                        let first = self.model.scope_sessions(&scope).iter().find(|s| s.status.is_live()).map(|s| s.id.clone());
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
                let l = self.layout_mut(&key);
                if let Some(m) = p.mode {
                    l.mode = m.into();
                }
                let preset = p.preset.as_deref().and_then(|x| {
                    let (c, r) = x.split_once(['x', '×']).unwrap_or((x, "1"));
                    Some(GridPreset::new(c.trim().parse().ok()?, r.trim().parse().ok()?))
                });
                if let Some(g) = preset {
                    l.grid = g;
                    l.mode = LayoutMode::Grid;
                }
                if p.cols.is_some() || p.rows.is_some() {
                    l.grid = GridPreset::new(p.cols.unwrap_or(l.grid.cols), p.rows.unwrap_or(l.grid.rows));
                    l.mode = LayoutMode::Grid;
                }
                if let Some(f) = &p.free {
                    l.set_free(FreeLayout::from_value(Some(f)));
                }
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
                toast("Floating. Drag its header to move it.".into())
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
                    let k = ThemeKind::from_id(v).unwrap_or_else(|| crate::engine_backend::theme_from_setting(v));
                    self.model.theme = k;
                    // Transcripts carry theme colours in their bytes: redraw them. Live terminals
                    // recolour by themselves (the view draws with the theme's palette).
                    let ids: Vec<SessionId> = self.static_ids.iter().cloned().collect();
                    for id in ids {
                        self.make_terminal(&id);
                    }
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

    fn terminal(&self, id: &str) -> Option<Arc<dyn TerminalSource>> {
        self.terms.get(id).cloned()
    }

    fn on_reply(&mut self, call: &Call, reply: &Reply) -> Vec<Notice> {
        use csm_engine::ToolError;
        let out = match reply {
            Err(e @ (ToolError::InvalidParams(_) | ToolError::UnknownTool(_))) => vec![Notice::Toast(e.to_string())],
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
        out
    }
}

impl DemoBackend {
    /// Value of a JSON param, for screens that want to show what a call did.
    pub fn describe(call: &Call) -> String {
        format!("{} {}", call.id, Value::to_string(&call.params))
    }
}

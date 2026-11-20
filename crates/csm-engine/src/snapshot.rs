//! The per-frame read model: an immutable [`Snapshot`] the engine republishes after every state
//! change. The UI takes it with [`crate::Engine::view`] (one short lock to clone an `Arc`) and
//! never waits on the engine thread. It is not a tool because it is read every frame; the tools
//! `session_list`, `inbox_list`, `view_get` and `layout_get` return the same values.

use std::collections::{BTreeMap, HashMap};

use csm_core::models::{GroupLayout, InspectorTab, SessionKind, WindowSlot};
use serde::Serialize;

use crate::session::{Phase, TerminalHandle};
use crate::state::{Progress, State};
use crate::status::{self, Status};

/// How groups appear (docs/plan.md "Three group views").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupView {
    /// A strip of group tabs; one group at a time (sidebar and panes).
    #[default]
    Rail,
    /// Every group as a collapsible section.
    Headers,
    /// No group UI.
    Flat,
}

/// One session, as every surface shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionView {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    /// Local folder (WSL: the `\\wsl$` mirror; SSH: empty).
    pub folder: String,
    /// The sidebar subtitle (leaf folder, host, `distro: leaf`).
    pub folder_short: String,
    pub command: String,
    /// Empty when ungrouped.
    pub group_id: String,
    /// `#rrggbb` (or an OSC 9001 override).
    pub accent: String,
    /// The accent was pushed by the program (OSC 9001); `session_accent_reset` goes back to the
    /// folder's colour.
    pub accent_override: bool,
    pub status: Status,
    /// One word for `status`.
    pub status_label: &'static str,
    /// 0..=100 while working, when the program reports it.
    pub progress: Option<u8>,
    /// The program's own message (OSC 7501 `msg`), or the heuristic's line.
    pub status_message: Option<String>,
    /// Has a PTY (or is starting one). False when asleep.
    pub live: bool,
    pub asleep: bool,
    pub exit_code: Option<i32>,
    /// The branch: polled from the folder, or pushed with OSC 9001 (which wins).
    pub branch: Option<String>,
    pub git_dirty: bool,
    /// What the branch tracks (`origin/main`); `None` when nothing.
    pub git_upstream: Option<String>,
    /// Commits ahead of / behind the upstream; `None` without one (or when it is gone).
    pub git_ahead: Option<i32>,
    pub git_behind: Option<i32>,
    /// Changed paths (`git status`).
    pub git_changes: usize,
    /// The git menu's header line (`main → origin/main · ↑2 · 3 changed · worktree of X`).
    pub git_summary: Option<String>,
    /// In a linked worktree: the main repo's name.
    pub worktree_of: Option<String>,
    /// The main working tree's root (canonical); every worktree of a repo shares it.
    pub repo_root: Option<String>,
    /// A git operation is running on this session's repository.
    pub git_busy: bool,
    /// The terminal title (OSC 0/2), when the program set one.
    pub title: Option<String>,
    pub is_claude: bool,
    /// Speaks OSC 7501 (its status is reported, not guessed).
    pub reports_status: bool,
    /// Epoch ms of the last focus (the Recent menu).
    pub last_activity_ms: Option<i64>,
    /// Epoch ms when it entered the inbox.
    pub inbox_since_ms: Option<i64>,
    /// "bundled ConPTY" / "inbox ConPTY" / "openpty", once started.
    pub backend: Option<&'static str>,
    /// Its run commands and each one's last run (the Run button, the chips, the Runs tab).
    pub runs: Vec<crate::runs::RunView>,
    /// The Open with entries that apply to it, with the exact command each would run.
    pub open_with: Vec<crate::tools::open::OpenWithTarget>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub expanded: bool,
    pub torn_off: bool,
    pub session_count: usize,
    /// Sessions of this group in the inbox (the rail tab's badge).
    pub needs_you: usize,
    /// Unseen done/error results (the rail tab's dot).
    pub unseen: usize,
}

/// The read model. `version` increases with every republish.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub version: u64,
    /// In sidebar order (the persisted order), live and asleep.
    pub sessions: Vec<SessionView>,
    pub groups: Vec<GroupInfo>,
    pub view: GroupView,
    /// The rail's group; `None` = "All".
    pub rail_tab: Option<String>,
    /// Layout per group id, plus [`csm_core::models::ALL_LAYOUT_KEY`].
    pub layouts: BTreeMap<String, GroupLayout>,
    /// The layout key the panes use right now (the rail tab, or `__ALL__`).
    pub layout_key: String,
    /// Session ids, needs-you first (oldest first), then unseen results (oldest first).
    pub inbox: Vec<String>,
    pub focused: Option<String>,
    pub inbox_open: bool,
    pub inbox_height: Option<f32>,
    pub inspector: Option<InspectorTab>,
    /// Restore loop progress while it runs.
    pub restore: Option<Progress>,
    /// Bulk restart progress while it runs.
    pub restart: Option<Progress>,
    /// Quit progress (the shutdown board) while it runs.
    pub quit: Option<Progress>,
    /// The Open with list (the editor in Settings).
    pub open_with: Vec<crate::tools::open::OpenWithView>,
    /// Saved window bounds (`window_placement_set`): `main` (the state file's `LastPlacement` and
    /// `WindowMaximized`), `group:<id>` and `pane:<session id>` for torn-off windows.
    pub windows: BTreeMap<String, WindowSlot>,
    #[serde(skip)]
    pub(crate) terminals: HashMap<String, TerminalHandle>,
    /// Output of each run command's last run, by `(session id, run id)`.
    #[serde(skip)]
    pub(crate) run_outputs: HashMap<(String, String), crate::runs::RunOutputHandle>,
}

impl Snapshot {
    pub fn session(&self, id: &str) -> Option<&SessionView> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// The terminal of a live session (the same as [`crate::Engine::terminal`]).
    pub fn terminal(&self, id: &str) -> Option<&TerminalHandle> {
        self.terminals.get(id)
    }

    /// Every run output handle (the UI's waker sums their generations to repaint on output).
    pub fn run_outputs(&self) -> impl Iterator<Item = &crate::runs::RunOutputHandle> {
        self.run_outputs.values()
    }

    /// The output of a run command's last run (read it at most once a frame).
    pub fn run_output(&self, session_id: &str, run_id: &str) -> Option<&crate::runs::RunOutputHandle> {
        self.run_outputs.get(&(session_id.to_owned(), run_id.to_owned()))
    }

    /// The layout of the current context (default when never set).
    pub fn current_layout(&self) -> GroupLayout {
        self.layouts.get(&self.layout_key).cloned().unwrap_or_default()
    }

    /// Sessions the current context shows: the rail tab's group, or every session.
    pub fn visible_sessions(&self) -> impl Iterator<Item = &SessionView> {
        let tab = if self.view == GroupView::Rail { self.rail_tab.clone() } else { None };
        self.sessions.iter().filter(move |s| tab.as_ref().is_none_or(|g| &s.group_id == g))
    }
}

pub(crate) fn group_view_of(state: &State) -> GroupView {
    use csm_core::models::GroupDisplayMode;
    match state.app.settings.group_display_mode {
        GroupDisplayMode::FilterStrip => GroupView::Rail,
        GroupDisplayMode::InlineHeaders => GroupView::Headers,
        GroupDisplayMode::None => GroupView::Flat,
    }
}

/// The layout key in use: the rail tab's group, else `__ALL__`.
pub(crate) fn layout_key(state: &State) -> String {
    match (group_view_of(state), &state.app.ui.rail_tab) {
        (GroupView::Rail, Some(g)) => g.clone(),
        _ => csm_core::models::ALL_LAYOUT_KEY.to_owned(),
    }
}

/// Builds a session's view (also updates when it entered the inbox).
pub(crate) fn session_view(state: &State, s: &csm_core::models::ShellSession) -> SessionView {
    let rt = state.rt.sessions.get(&s.id);
    let (st, progress, message) = status::derive(s.is_dormant, state.rt.restarting.contains(&s.id), rt);
    let accent = s.color_override.clone().unwrap_or_else(|| csm_core::color::hex_color(&s.accent_key()).to_owned());
    let name = if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() };
    let git = crate::tools::git::view_fields(state, s);
    SessionView {
        id: s.id.clone(),
        name,
        kind: match s.kind {
            SessionKind::Local => "local",
            SessionKind::Ssh => "ssh",
            SessionKind::Wsl => "wsl",
        },
        folder: s.working_folder.clone(),
        folder_short: s.folder_short(),
        command: s.command.clone(),
        group_id: s.group_id.clone(),
        accent,
        accent_override: s.color_override.is_some(),
        status: st,
        status_label: st.label(),
        progress,
        status_message: message,
        live: !s.is_dormant && (rt.is_some() || state.rt.restarting.contains(&s.id)),
        asleep: s.is_dormant,
        exit_code: rt.and_then(|r| match r.phase {
            Phase::Exited { code } => code,
            _ => None,
        }),
        branch: git.branch,
        git_dirty: git.dirty,
        git_upstream: git.upstream,
        git_ahead: git.ahead,
        git_behind: git.behind,
        git_changes: git.changes,
        git_summary: git.summary,
        worktree_of: git.worktree_of,
        repo_root: git.repo_root,
        git_busy: git.busy,
        title: rt.and_then(|r| r.title.clone()).filter(|t| !t.is_empty()),
        is_claude: crate::launch::is_claude_command(&s.command),
        reports_status: rt.is_some_and(|r| r.has_program_status),
        last_activity_ms: s.last_activity_at.epoch_ms(),
        inbox_since_ms: state.rt.inbox_since.get(&s.id).copied(),
        backend: rt.and_then(|r| r.backend).map(|b| b.label()),
        runs: crate::runs::views(state, s),
        open_with: crate::tools::open::targets(&state.app.settings.open_with, s),
    }
}

/// Builds the snapshot. Updates `rt.inbox_since` (entering / leaving the inbox) first, so the
/// inbox order is "waiting longest first".
pub(crate) fn build(state: &mut State, version: u64) -> Snapshot {
    let now = crate::session::now_ms();
    let mut sessions: Vec<SessionView> = state.app.sessions.iter().map(|s| session_view(state, s)).collect();
    for v in &mut sessions {
        if v.status.in_inbox() {
            let since = *state.rt.inbox_since.entry(v.id.clone()).or_insert(now);
            v.inbox_since_ms = Some(since);
        } else {
            state.rt.inbox_since.remove(&v.id);
            v.inbox_since_ms = None;
        }
    }
    state.rt.inbox_since.retain(|id, _| sessions.iter().any(|s| &s.id == id));

    let mut inbox: Vec<&SessionView> = sessions.iter().filter(|s| s.status.in_inbox()).collect();
    inbox.sort_by_key(|s| (!s.status.needs_you(), s.inbox_since_ms.unwrap_or(i64::MAX)));
    let inbox: Vec<String> = inbox.into_iter().map(|s| s.id.clone()).collect();

    let groups = state
        .app
        .groups
        .iter()
        .map(|g| {
            let mine = sessions.iter().filter(|s| s.group_id == g.id);
            let (mut count, mut needs, mut unseen) = (0, 0, 0);
            for s in mine {
                count += 1;
                if s.status.needs_you() {
                    needs += 1;
                } else if s.status.in_inbox() {
                    unseen += 1;
                }
            }
            GroupInfo {
                id: g.id.clone(),
                name: g.name.clone(),
                expanded: g.is_expanded,
                torn_off: state.app.ui.torn_off_groups.contains(&g.id),
                session_count: count,
                needs_you: needs,
                unseen,
            }
        })
        .collect();

    let terminals = state.rt.sessions.iter().map(|(id, rt)| (id.clone(), TerminalHandle(rt.io.clone()))).collect();

    let ui = &state.app.ui;
    let layouts = ui.layouts.clone();
    let mut windows = ui.windows.clone();
    if let Some(p) = state.app.last_placement {
        windows.insert("main".into(), WindowSlot { placement: p, maximized: state.app.window_maximized });
    }
    Snapshot {
        version,
        sessions,
        groups,
        view: group_view_of(state),
        rail_tab: ui.rail_tab.clone(),
        layouts,
        layout_key: layout_key(state),
        inbox,
        focused: state.rt.focused.clone(),
        inbox_open: ui.inbox_open,
        inbox_height: ui.inbox_height,
        inspector: ui.inspector,
        restore: state.rt.restore,
        restart: state.rt.restart,
        quit: state.rt.quit,
        open_with: crate::tools::open::list_view(&state.app.settings.open_with),
        windows,
        terminals,
        run_outputs: crate::runs::handles(state),
    }
}

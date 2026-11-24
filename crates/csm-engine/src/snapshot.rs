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
    /// A short note a process add-on set on a session it opened ("running remotely"); already part of `folder_short`.
    pub annotation: Option<String>,
    /// The process add-on that opened it, when one did.
    pub owner_addon: Option<String>,
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
    /// The Claude conversation it runs (see `ShellSession::claude_session_id`).
    pub claude_session_id: Option<String>,
    /// Speaks OSC 7501 (its status is reported, not guessed).
    pub reports_status: bool,
    /// Claude Code Remote Control is on: the session can be continued from claude.ai or the
    /// Claude app (read from its transcript; local Claude sessions only).
    pub remote_control: bool,
    /// The session's claude.ai link while Remote Control is on, when known (http/https only).
    pub remote_control_url: Option<String>,
    /// Epoch ms of the last focus (the Recent menu).
    pub last_activity_ms: Option<i64>,
    /// Epoch ms when it entered the inbox.
    pub inbox_since_ms: Option<i64>,
    /// Epoch ms when it entered its current status (every session; the Board's "time in column").
    pub status_since_ms: Option<i64>,
    /// Its project (the Board's tasks): `repo:<root>` in a git repository, else `group:<id>`
    /// (`csm_core::tasks::project_key`).
    pub project: String,
    /// The project's name: the repository's folder, or the group's name.
    pub project_name: String,
    /// "bundled ConPTY" / "inbox ConPTY" / "openpty", once started.
    pub backend: Option<&'static str>,
    /// Its run commands and each one's last run (the Run button, the chips, the Runs tab).
    pub runs: Vec<crate::runs::RunView>,
    /// The Open with entries that apply to it, with the exact command each would run.
    pub open_with: Vec<crate::tools::open::OpenWithTarget>,
    /// The session's own terminal look (colours, font size, cursor, padding); `None` follows
    /// the app's settings. `session_look_set` changes it.
    pub look: Option<csm_core::look::Look>,
    /// Claude subagents running now / known (`session_subagents` has the list).
    pub subagents_running: usize,
    pub subagents_total: usize,
    /// The full report the Board draws (`session_subagents` returns the same data).
    #[serde(skip)]
    pub subagents: Option<std::sync::Arc<csm_core::subagents::SubagentsReport>>,
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

/// A recently closed session, as the Recent menu lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClosedView {
    pub name: String,
    /// The folder, `user@host` or `distro: folder`.
    pub subtitle: String,
    pub command: String,
}

/// The read model. `version` increases with every republish.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub version: u64,
    /// Add-on folders and their state (Settings > Add-ons, the palette, the slash menu).
    pub addons: Vec<crate::addons::AddonSummary>,
    /// The built-in add-ons (Claude usage) and their state; listed first in Settings > Add-ons.
    pub builtin_addons: Vec<crate::addons::AddonSummary>,
    /// What enabled process add-ons offer under "Remote…" (quick-open and the palette).
    pub remotes: Vec<crate::plugins::RemoteView>,
    /// The New session forms enabled process add-ons offer, with their loaded choices and submit state.
    pub new_sessions: Vec<crate::tools::new_session::NewSessionView>,
    /// The slash menu's list: the person's presets, then enabled add-ons' presets.
    pub slash_presets: Vec<csm_core::models::SlashCommand>,
    /// In sidebar order (the persisted order), live and asleep.
    pub sessions: Vec<SessionView>,
    pub groups: Vec<GroupInfo>,
    pub view: GroupView,
    /// The rail's group; `None` = "All".
    pub rail_tab: Option<String>,
    /// Layout per group id, plus [`csm_core::models::ALL_LAYOUT_KEY`].
    pub layouts: BTreeMap<String, GroupLayout>,
    /// Remembered custom grid sizes `(cols, rows)`, newest first (the Grid menu; `grid_preset_forget`).
    pub custom_grids: Vec<(u8, u8)>,
    /// The layout key the panes use right now (the rail tab, or `__ALL__`).
    pub layout_key: String,
    /// Session ids, needs-you first (oldest first), then unseen results (oldest first).
    pub inbox: Vec<String>,
    pub focused: Option<String>,
    pub inbox_open: bool,
    pub inbox_height: Option<f32>,
    /// The inspector's width (logical pixels) when dragged; `None` is the default.
    pub inspector_width: Option<f32>,
    pub inspector: Option<InspectorTab>,
    /// Restore loop progress while it runs.
    pub restore: Option<Progress>,
    /// Bulk restart progress while it runs.
    pub restart: Option<Progress>,
    /// Quit progress (the shutdown board) while it runs.
    pub quit: Option<Progress>,
    /// The whole quit's wait budget (the per-session wait times the Claude sessions to close).
    pub quit_budget_ms: Option<u64>,
    /// How many Claude sessions the running quit closes at once.
    pub quit_slots: Option<u32>,
    /// The Open with list (the editor in Settings).
    pub open_with: Vec<crate::tools::open::OpenWithView>,
    /// Open with entries from CodeShellManager 0.x waiting for the person's answer (the consent
    /// card; `open_with_pending_answer`). Empty: nothing to ask.
    pub pending_open_with: Vec<crate::tools::open::OpenWithView>,
    /// SSH port forwards that are running (`session_port_forwards`); the Session tab lists them.
    pub port_forwards: Vec<crate::port_forward::PortForwardView>,
    /// A newer release than the running build, not dismissed: the title bar's badge
    /// (`update_check`, `update_dismiss`). `None`: nothing to show.
    pub update: Option<crate::tools::update::UpdateView>,
    /// Saved window bounds (`window_placement_set`): `main` (the state file's `LastPlacement` and
    /// `WindowMaximized`), `group:<id>` and `pane:<session id>` for torn-off windows.
    pub windows: BTreeMap<String, WindowSlot>,
    /// Closed sessions that can be reopened, newest first (`session_reopen_closed` by index).
    pub recently_closed: Vec<ClosedView>,
    /// The Board's tasks, every project (`tasks.json`).
    pub tasks: std::sync::Arc<Vec<csm_core::tasks::Task>>,
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
    let (st, progress, message) = status::derive(s.is_dormant, state.rt.restarting.contains(&s.id), rt, state.rt.plugin_marks.get(&s.id));
    let annotation = state.rt.plugin_marks.get(&s.id).and_then(|m| m.annotation.clone());
    let folder_short = match &annotation {
        Some(a) => format!("{} · {a}", s.folder_short()),
        None => s.folder_short(),
    };
    let accent = s.color_override.clone().unwrap_or_else(|| csm_core::color::hex_color(&s.accent_key()).to_owned());
    let name = if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() };
    let git = crate::tools::git::view_fields(state, s);
    let project = csm_core::tasks::project_key(git.repo_root.as_deref(), &s.group_id);
    SessionView {
        id: s.id.clone(),
        name,
        kind: match s.kind {
            SessionKind::Local => "local",
            SessionKind::Ssh => "ssh",
            SessionKind::Wsl => "wsl",
        },
        folder: s.working_folder.clone(),
        folder_short,
        annotation,
        owner_addon: Some(s.owner_addon.clone()).filter(|a| !a.is_empty()),
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
        claude_session_id: s.claude_session_id.clone(),
        reports_status: rt.is_some_and(|r| r.has_program_status),
        remote_control: rt.is_some_and(|r| r.remote_control.is_some()),
        remote_control_url: rt.and_then(|r| r.remote_control.as_ref()).and_then(|r| r.url.clone()),
        last_activity_ms: s.last_activity_at.epoch_ms(),
        inbox_since_ms: state.rt.inbox_since.get(&s.id).copied(),
        status_since_ms: state.rt.status_since.get(&s.id).map(|(_, t)| *t),
        project: project.clone(),
        project_name: csm_core::tasks::project_label(&project, |g| group_name(state, g)),
        backend: rt.and_then(|r| r.backend).map(|b| b.label()),
        runs: crate::runs::views(state, s),
        open_with: crate::tools::open::targets(&state.app.settings.open_with, s),
        look: Some(csm_core::look::Look::from_session(s)).filter(|l| !l.is_empty()),
        subagents_running: state.rt.subagents.get(&s.id).map_or(0, |r| r.running),
        subagents_total: state.rt.subagents.get(&s.id).map_or(0, |r| r.total()),
        subagents: state.rt.subagents.get(&s.id).cloned(),
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
    // When each session entered its status; a change goes to the status log (the index merges
    // flicker and keeps 30 days).
    for v in &mut sessions {
        let since = match state.rt.status_since.get(&v.id) {
            Some((st, t)) if *st == v.status => *t,
            _ => {
                state.rt.status_since.insert(v.id.clone(), (v.status, now));
                if let Some(ix) = &state.rt.index {
                    ix.record_status(&v.id, v.status.id(), now);
                }
                now
            }
        };
        v.status_since_ms = Some(since);
    }
    state.rt.status_since.retain(|id, _| sessions.iter().any(|s| &s.id == id));

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

    // The tasks are shared, not copied, until they change (bodies are up to 16 KiB each).
    let tasks = match &state.rt.tasks_view {
        Some((g, a)) if *g == state.rt.tasks_gen => a.clone(),
        _ => {
            let a = std::sync::Arc::new(state.rt.tasks.tasks.clone());
            state.rt.tasks_view = Some((state.rt.tasks_gen, a.clone()));
            a
        }
    };

    let ui = &state.app.ui;
    let layouts = ui.layouts.clone();
    let mut windows = ui.windows.clone();
    if let Some(p) = state.app.last_placement {
        windows.insert("main".into(), WindowSlot { placement: p, maximized: state.app.window_maximized });
    }
    Snapshot {
        addons: crate::addons::summaries(&state.rt.addons),
        builtin_addons: crate::addons::builtin_summaries(&state.app.settings),
        remotes: crate::plugins::remote_views(&state.rt.addons),
        new_sessions: crate::tools::new_session::views(&state.rt.addons, &state.rt.new_sessions),
        slash_presets: crate::addons::slash_presets(&state.rt.addons, &state.app.settings.claude_commands),
        version,
        sessions,
        groups,
        view: group_view_of(state),
        rail_tab: ui.rail_tab.clone(),
        layouts,
        custom_grids: ui.custom_grids.clone(),
        layout_key: layout_key(state),
        inbox,
        focused: state.rt.focused.clone(),
        inbox_open: ui.inbox_open,
        inbox_height: ui.inbox_height,
        inspector_width: ui.inspector_width,
        inspector: ui.inspector,
        restore: state.rt.restore,
        restart: state.rt.restart,
        quit: state.rt.quit,
        quit_budget_ms: state.rt.quit_budget_ms,
        quit_slots: state.rt.quit_slots,
        port_forwards: {
            crate::port_forward::prune(&mut state.rt.forwards);
            crate::port_forward::live_views(&state.rt.forwards)
        },
        open_with: crate::tools::open::list_view(&state.app.settings.open_with),
        pending_open_with: crate::tools::open::list_view(&state.app.pending_open_with),
        update: crate::tools::update::view(state),
        windows,
        recently_closed: state
            .app
            .recently_closed
            .iter()
            .map(|e| ClosedView { name: e.name.clone(), subtitle: e.subtitle(), command: e.command.clone() })
            .collect(),
        tasks,
        terminals,
        run_outputs: crate::runs::handles(state),
    }
}

/// A group's name, for project labels.
fn group_name(state: &State, id: &str) -> Option<String> {
    state.app.groups.iter().find(|g| g.id == id).map(|g| g.name.clone())
}

/// A session's project (`csm_core::tasks::project_key`), from the same git facts its view shows.
pub(crate) fn project_of(state: &State, s: &csm_core::models::ShellSession) -> String {
    let git = crate::tools::git::view_fields(state, s);
    csm_core::tasks::project_key(git.repo_root.as_deref(), &s.group_id)
}

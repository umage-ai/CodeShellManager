//! The read model as it travels: the snapshot, the session and group views, and the run and
//! Open-with rows inside a session. Every type owns its strings and derives both directions.

use serde::{Deserialize, Serialize};

/// The whole read model at one version. `version` increases with every republish on the server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u64,
    /// In sidebar order (the persisted order), live and asleep.
    pub sessions: Vec<SessionView>,
    pub groups: Vec<GroupInfo>,
    pub view: GroupView,
    /// The rail's group; `None` = "All".
    pub rail_tab: Option<String>,
    /// The layout key the panes use right now (the rail tab, or `__ALL__`).
    pub layout_key: String,
    /// Session ids, needs-you first (oldest first), then unseen results (oldest first).
    pub inbox: Vec<String>,
    pub focused: Option<String>,
    pub inbox_open: bool,
    pub inbox_height: Option<f32>,
    /// Closed sessions that can be reopened, newest first.
    pub recently_closed: Vec<ClosedView>,
    /// The Open with list (the editor in Settings).
    pub open_with: Vec<OpenWithEntry>,
}

impl Snapshot {
    pub fn session(&self, id: &str) -> Option<&SessionView> {
        self.sessions.iter().find(|s| s.id == id)
    }
}

/// How groups appear. Mirrors `csm_engine::GroupView`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
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

/// One session's status. Mirrors `csm_engine::Status`; the serde names are the engine's ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Launching: waiting for the first measured size, the Claude stagger, or the process.
    Starting,
    /// The program reports work in progress (see `progress`).
    Working,
    /// Waiting for the person to approve something.
    NeedsYouApproval,
    /// Waiting for an answer, input or sign-in.
    NeedsYouQuestion,
    /// Finished a turn the person has not looked at yet.
    DoneUnseen,
    /// Failed a turn (or reported an error) the person has not looked at yet.
    Error,
    #[default]
    Idle,
    /// No process: kept in the sidebar, woken on demand.
    Asleep,
    /// The process ended by itself; the pane stays until closed or restarted.
    Exited,
}

/// One session, as every surface shows it. Mirrors `csm_engine::SessionView` minus the look and
/// the subagent report (see the crate docs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub id: String,
    pub name: String,
    /// `local`, `ssh` or `wsl`.
    pub kind: String,
    /// Local folder (WSL: the `\\wsl$` mirror; SSH: empty).
    pub folder: String,
    /// The sidebar subtitle (leaf folder, host, `distro: leaf`).
    pub folder_short: String,
    pub annotation: Option<String>,
    pub owner_addon: Option<String>,
    pub command: String,
    /// Empty when ungrouped.
    pub group_id: String,
    /// `#rrggbb` (or an OSC 9001 override).
    pub accent: String,
    pub accent_override: bool,
    pub status: Status,
    /// One word for `status`.
    pub status_label: String,
    /// 0..=100 while working, when the program reports it.
    pub progress: Option<u8>,
    pub status_message: Option<String>,
    /// Has a PTY (or is starting one). False when asleep.
    pub live: bool,
    pub asleep: bool,
    pub exit_code: Option<i32>,
    pub branch: Option<String>,
    pub git_dirty: bool,
    pub git_upstream: Option<String>,
    pub git_ahead: Option<i32>,
    pub git_behind: Option<i32>,
    pub git_changes: usize,
    pub git_summary: Option<String>,
    pub worktree_of: Option<String>,
    pub repo_root: Option<String>,
    pub git_busy: bool,
    pub title: Option<String>,
    pub is_claude: bool,
    pub claude_session_id: Option<String>,
    pub reports_status: bool,
    pub remote_control: bool,
    pub remote_control_url: Option<String>,
    pub last_activity_ms: Option<i64>,
    pub inbox_since_ms: Option<i64>,
    pub status_since_ms: Option<i64>,
    pub project: String,
    pub project_name: String,
    /// "bundled ConPTY" / "inbox ConPTY" / "openpty", once started.
    pub backend: Option<String>,
    /// Its run commands and each one's last run.
    pub runs: Vec<RunView>,
    /// The Open with entries that apply to it, with the exact command each would run.
    pub open_with: Vec<OpenWithTarget>,
    pub subagents_running: usize,
    pub subagents_total: usize,
}

/// A group and its counts. Mirrors `csm_engine::GroupInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClosedView {
    pub name: String,
    /// The folder, `user@host` or `distro: folder`.
    pub subtitle: String,
    pub command: String,
}

/// One run command of a session and its last run. Mirrors `csm_engine::RunView`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunView {
    pub id: String,
    pub label: String,
    pub command_line: String,
    pub is_default: bool,
    /// `process` or `powershell`.
    pub mode: String,
    pub post_run_url: Option<String>,
    pub phase: RunPhase,
    pub exit_code: Option<i32>,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
}

/// Where a run is. Mirrors `csm_engine::RunPhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Never started in this session's life (or its output was dismissed).
    #[default]
    Idle,
    /// The worker is starting the process.
    Starting,
    Running,
    /// Exit code 0.
    Ok,
    /// Non-zero exit, or it could not start.
    Failed,
    /// Stopped by the person (or its session closed).
    Stopped,
}

/// One Open with entry as it applies to a session. Mirrors `csm_engine::OpenWithTarget`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenWithTarget {
    pub entry_id: String,
    pub label: String,
    /// The program's file name.
    pub program: String,
    pub is_default: bool,
    /// The exact command this session would run (or the program and why it can't).
    pub command: String,
}

/// One Open with entry in the editor's list. Mirrors `csm_engine::OpenWithView`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenWithEntry {
    pub id: String,
    pub label: String,
    pub executable: String,
    pub program: String,
    pub local_args: Option<String>,
    pub wsl_args: Option<String>,
    pub ssh_args: Option<String>,
    pub is_default: bool,
}

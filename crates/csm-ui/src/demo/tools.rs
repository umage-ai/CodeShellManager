//! Demo `ToolSpec`s for the ids in `docs/tools.md` the engine doesn't register yet. Each one
//! validates its params (the same `deny_unknown_fields` schema the real tool will take) and
//! echoes them; [`super::DemoBackend::on_reply`] applies the effect to the demo model. Effects
//! are declared honestly, as the real tools will, so consent and MCP hints already work.
//!
//! When the engine registers a real tool with one of these ids, delete the demo one (the
//! registry refuses duplicates, so a test that starts both catches it).

use csm_engine::{Ctx, Effects, Reply, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{FloatPane, GridPreset, GroupMode, Kind, LayoutMode, SplitNode};

/// Validate params as `P`, then echo them back as the result's data.
fn echo<P: DeserializeOwned + Serialize>(_: &mut Ctx<'_>, v: Value) -> Reply {
    let p: P = parse_params(v)?;
    Ok(ToolOutput::new("", serde_json::to_value(&p).unwrap_or(Value::Null)))
}

fn none(_: &mut Ctx<'_>, v: Value) -> Reply {
    if v.as_object().is_some_and(|o| !o.is_empty()) {
        return Err(csm_engine::ToolError::InvalidParams("this tool takes no params".into()));
    }
    Ok(ToolOutput::message(""))
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionIdParams {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SshParams {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub remote_folder: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WslParams {
    pub distro: String,
    pub user: Option<String>,
    pub folder: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KindParam {
    #[default]
    Local,
    Ssh,
    Wsl,
}

impl From<KindParam> for Kind {
    fn from(k: KindParam) -> Kind {
        match k {
            KindParam::Local => Kind::Local,
            KindParam::Ssh => Kind::Ssh,
            KindParam::Wsl => Kind::Wsl,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionNewParams {
    /// The folder to start in (local), or a display label for remote sessions.
    pub folder: String,
    /// `claude`, `pwsh`, a preset. Default: `claude`.
    pub command: Option<String>,
    pub name: Option<String>,
    pub group: Option<String>,
    pub kind: Option<KindParam>,
    pub ssh: Option<SshParams>,
    pub wsl: Option<WslParams>,
    /// Focus the new session (default true).
    pub focus: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameParams {
    pub session_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionEditParams {
    pub session_id: String,
    pub name: Option<String>,
    pub command: Option<String>,
    pub folder: Option<String>,
    /// Restart now when the change needs one.
    pub restart: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloseParams {
    pub session_id: String,
    /// Close even when busy. Without it a busy session is refused.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReopenParams {
    pub index: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendTextParams {
    pub session_id: String,
    pub text: String,
    #[serde(default)]
    pub submit: bool,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadOutputParams {
    pub session_id: String,
    pub lines: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitParams {
    pub session_id: String,
    /// `idle`, `done`, `needs_you`, `exit` or `any_change`.
    pub until: String,
    pub timeout_s: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionListParams {
    pub group: Option<String>,
    pub status: Option<String>,
    pub include_asleep: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupNewParams {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupIdParams {
    pub group_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupRenameParams {
    pub group_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupForceParams {
    pub group_id: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveToGroupParams {
    pub session_id: String,
    /// `null` moves it to Ungrouped.
    pub group_id: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewSetParams {
    /// `rail`, `headers` or `flat`.
    pub mode: Option<GroupModeParam>,
    /// The rail tab: a group id or `all`.
    pub rail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupModeParam {
    Rail,
    Headers,
    Flat,
}

impl From<GroupModeParam> for GroupMode {
    fn from(m: GroupModeParam) -> GroupMode {
        match m {
            GroupModeParam::Rail => GroupMode::Rail,
            GroupModeParam::Headers => GroupMode::Headers,
            GroupModeParam::Flat => GroupMode::Flat,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutGetParams {
    /// A group id or `all`.
    pub group: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LayoutModeParam {
    Focus,
    Side,
    Overview,
    Grid,
    Free,
}

impl From<LayoutModeParam> for LayoutMode {
    fn from(m: LayoutModeParam) -> LayoutMode {
        match m {
            LayoutModeParam::Focus => LayoutMode::Focus,
            LayoutModeParam::Side => LayoutMode::Side,
            LayoutModeParam::Overview => LayoutMode::Overview,
            LayoutModeParam::Grid => LayoutMode::Grid,
            LayoutModeParam::Free => LayoutMode::Free,
        }
    }
}

impl From<LayoutMode> for LayoutModeParam {
    fn from(m: LayoutMode) -> LayoutModeParam {
        match m {
            LayoutMode::Focus => LayoutModeParam::Focus,
            LayoutMode::Side => LayoutModeParam::Side,
            LayoutMode::Overview => LayoutModeParam::Overview,
            LayoutMode::Grid => LayoutModeParam::Grid,
            LayoutMode::Free => LayoutModeParam::Free,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GridParam {
    pub cols: u8,
    pub rows: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutSetParams {
    /// A group id or `all`.
    pub group: String,
    pub mode: Option<LayoutModeParam>,
    /// Grid preset, or a custom cols × rows.
    pub grid: Option<GridParam>,
    /// Free mode's split tree (persisted ratios).
    #[schemars(with = "Option<Value>")]
    pub tree: Option<SplitNode>,
    /// Free mode's floating panes.
    #[schemars(with = "Option<Value>")]
    pub floats: Option<Vec<FloatPane>>,
}

impl GridParam {
    pub fn preset(self) -> GridPreset {
        GridPreset::new(self.cols, self.rows)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinParams {
    pub session_id: String,
    /// The group whose Side by side set changes (default: the session's own, or `all`).
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaneParams {
    pub session_id: String,
    pub group: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InspectorParam {
    Notes,
    Runs,
    Git,
    Session,
    Hidden,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InspectorShowParams {
    pub tab: InspectorParam,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxPanelParams {
    pub open: Option<bool>,
    /// Height in points.
    pub height: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunParams {
    pub session_id: String,
    /// Default: the session's default run command.
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunEditParams {
    pub session_id: String,
    pub runs: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwitchBranchParams {
    pub session_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorktreeNewParams {
    pub session_id: String,
    pub branch: String,
    pub open_session: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRemoveParams {
    pub session_id: String,
    #[serde(default)]
    pub delete_branch: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteGetParams {
    pub folder: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoteSetParams {
    pub folder: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchParams {
    pub query: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsGetParams {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsSetParams {
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenWithParams {
    pub session_id: String,
    /// The entry's label; default: the default entry.
    pub entry: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantRevokeParams {
    pub client: String,
    pub tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportParams {
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QuitParams {
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DemoStatusParams {
    pub session_id: String,
    /// `working`, `approval`, `question`, `done`, `idle`, `asleep`, `error`, `starting`.
    pub status: String,
}

const READ: Effects = Effects::READ_ONLY;
const UI: Effects = Effects::UI;
const STATE: Effects = Effects::MUTATES_STATE;
const CONTENT: Effects = Effects::from_bits_truncate(Effects::IDEMPOTENT.bits() | Effects::READS_SESSION_CONTENT.bits());
const LAUNCH: Effects = Effects::from_bits_truncate(Effects::LAUNCHES_PROCESS.bits() | Effects::MUTATES_STATE.bits());
const DISCARD: Effects = Effects::from_bits_truncate(Effects::MAY_DISCARD_DATA.bits() | Effects::MUTATES_STATE.bits());
const RESTART: Effects = Effects::from_bits_truncate(
    Effects::MAY_DISCARD_DATA.bits() | Effects::LAUNCHES_PROCESS.bits() | Effects::MUTATES_STATE.bits(),
);
const NET: Effects = Effects::from_bits_truncate(
    Effects::LAUNCHES_PROCESS.bits() | Effects::ACCESSES_NETWORK.bits() | Effects::WRITES_FILESYSTEM.bits(),
);
const GIT_WRITE: Effects = Effects::from_bits_truncate(Effects::LAUNCHES_PROCESS.bits() | Effects::WRITES_FILESYSTEM.bits());

#[allow(clippy::too_many_arguments)]
fn spec(
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    toolset: Toolset,
    effects: Effects,
    input_schema: fn() -> Value,
    run: csm_engine::ToolFn,
) -> ToolSpec {
    ToolSpec { id, title, summary, toolset, effects, input_schema, run }
}

/// Every demo tool. Ids are exactly those of `docs/tools.md`.
pub fn tools() -> Vec<ToolSpec> {
    use Toolset::*;
    vec![
        // core
        spec("session_list", "List sessions", "Sessions with name, kind, folder, group, status and branch.", Core, READ, schema_of::<SessionListParams>, echo::<SessionListParams>),
        spec("session_get", "Show a session", "One session in full, including its program-status records.", Core, READ, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("session_new", "New session…", "Open a session in a folder: claude by default, or another command; local, SSH or WSL.", Core, LAUNCH, schema_of::<SessionNewParams>, echo::<SessionNewParams>),
        spec("session_focus", "Focus session", "Bring a session into view; wakes it if it is asleep.", Core, UI, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("session_rename", "Rename session", "Give a session a new name.", Core, STATE, schema_of::<RenameParams>, echo::<RenameParams>),
        spec("session_edit", "Edit session…", "Change a session's command, folder or name; restart when the change needs it.", Core, STATE, schema_of::<SessionEditParams>, echo::<SessionEditParams>),
        spec("session_close", "Close session", "End a session's process and remove it. Refuses a busy session without force.", Core, DISCARD, schema_of::<CloseParams>, echo::<CloseParams>),
        spec("session_sleep", "Sleep session", "Stop the process but keep the session, its group, notes and run commands.", Core, DISCARD, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("session_wake", "Wake session", "Start a sleeping session again; a Claude session resumes its conversation.", Core, LAUNCH, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("session_restart", "Restart session", "Close and reopen a session in place, waiting for the old process to exit.", Core, RESTART, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("session_reopen_closed", "Reopen the last closed session", "Reopen a recently closed session (Ctrl+Shift+T).", Core, LAUNCH, schema_of::<ReopenParams>, echo::<ReopenParams>),
        spec("session_send_text", "Send text to a session", "Paste text into a session (bracketed paste); submit adds Enter. Refuses itself and mid-turn sessions.", Core, Effects::SENDS_INPUT, schema_of::<SendTextParams>, echo::<SendTextParams>),
        spec("session_read_screen", "Read a session's screen", "The visible screen as text.", Core, CONTENT, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("session_read_output", "Read a session's output", "The tail of the scrollback, ANSI-stripped.", Core, CONTENT, schema_of::<ReadOutputParams>, echo::<ReadOutputParams>),
        spec("session_wait", "Wait for a session", "Completes when the session reaches a status.", Core, READ, schema_of::<WaitParams>, echo::<WaitParams>),
        spec("inbox_list", "Needs you", "Sessions waiting on you, oldest first, then unseen done and error.", Core, READ, empty_schema, none),
        spec("inbox_next", "Jump to the next session that needs you", "Focus the next session in the Needs you inbox (Ctrl+J).", Core, UI, empty_schema, none),
        spec("session_status_clear", "Clear status", "Forget a stuck program status; the heuristic watches the session again.", Core, STATE, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        // groups
        spec("group_list", "List groups", "Groups and their sessions.", Groups, READ, empty_schema, none),
        spec("group_new", "New group…", "Create a group.", Groups, STATE, schema_of::<GroupNewParams>, echo::<GroupNewParams>),
        spec("group_rename", "Rename group", "Give a group a new name.", Groups, STATE, schema_of::<GroupRenameParams>, echo::<GroupRenameParams>),
        spec("group_delete", "Delete group", "Delete a group; its sessions move to Ungrouped. Nothing is closed.", Groups, STATE, schema_of::<GroupIdParams>, echo::<GroupIdParams>),
        spec("session_move_to_group", "Move to group", "Move a session to a group (or Ungrouped), optionally before or after another.", Groups, STATE, schema_of::<MoveToGroupParams>, echo::<MoveToGroupParams>),
        spec("group_restart_all", "Restart all in group…", "Restart every live session in a group, one at a time.", Groups, RESTART, schema_of::<GroupForceParams>, echo::<GroupForceParams>),
        spec("group_sleep_all", "Sleep all in group", "Put every live session in a group to sleep.", Groups, DISCARD, schema_of::<GroupIdParams>, echo::<GroupIdParams>),
        spec("group_close_all", "Close all in group…", "Close every session in a group. Refuses busy ones without force.", Groups, DISCARD, schema_of::<GroupForceParams>, echo::<GroupForceParams>),
        spec("group_tear_off", "Move group to its own window", "Show a group in its own OS window with its own layout.", Groups, UI, schema_of::<GroupIdParams>, echo::<GroupIdParams>),
        spec("group_dock", "Dock group back", "Bring a torn-off group back into the main window.", Groups, UI, schema_of::<GroupIdParams>, echo::<GroupIdParams>),
        // layout
        spec("view_get", "Group view", "The sidebar's group view (rail, headers, flat) and the rail tab.", Layout, READ, empty_schema, none),
        spec("view_set", "Set group view", "Switch the group view (rail, headers, flat) or the rail tab.", Layout, UI, schema_of::<ViewSetParams>, echo::<ViewSetParams>),
        spec("layout_get", "Layout", "A group's layout: mode, pinned sessions, grid preset, split tree.", Layout, READ, schema_of::<LayoutGetParams>, echo::<LayoutGetParams>),
        spec("layout_set", "Set layout", "Set a group's layout mode, grid preset or split tree.", Layout, UI, schema_of::<LayoutSetParams>, echo::<LayoutSetParams>),
        spec("layout_pin", "Pin to Side by side", "Keep a session in its group's Side by side set.", Layout, UI, schema_of::<PinParams>, echo::<PinParams>),
        spec("layout_unpin", "Unpin from Side by side", "Remove a session from its group's Side by side set.", Layout, UI, schema_of::<PinParams>, echo::<PinParams>),
        spec("pane_float", "Float in this window", "Float a pane over the others (Free layout).", Layout, UI, schema_of::<PaneParams>, echo::<PaneParams>),
        spec("pane_dock", "Dock into the split tree", "Dock a floating or torn-off pane back into the layout.", Layout, UI, schema_of::<PaneParams>, echo::<PaneParams>),
        spec("pane_tear_off", "Move to a new window", "Show a pane in its own OS window.", Layout, UI, schema_of::<PaneParams>, echo::<PaneParams>),
        spec("inspector_show", "Inspector", "Show the inspector on a tab (notes, runs, git, session) or hide it.", Layout, UI, schema_of::<InspectorShowParams>, echo::<InspectorShowParams>),
        spec("inbox_panel_set", "Needs you panel", "Open or close the Needs you inbox, or set its height.", Layout, UI, schema_of::<InboxPanelParams>, echo::<InboxPanelParams>),
        // runs
        spec("run_list", "Run commands", "A session's run commands and their last result.", Runs, READ, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("run_start", "Run", "Start a run command (default: the session's default) in its own background terminal.", Runs, Effects::LAUNCHES_PROCESS, schema_of::<RunParams>, echo::<RunParams>),
        spec("run_stop", "Stop run", "Stop a running run command.", Runs, Effects::MAY_DISCARD_DATA, schema_of::<RunParams>, echo::<RunParams>),
        spec("run_output", "Run output", "The output of a run command.", Runs, CONTENT, schema_of::<RunParams>, echo::<RunParams>),
        spec("run_edit", "Edit run commands…", "Replace a session's list of run commands.", Runs, STATE, schema_of::<RunEditParams>, echo::<RunEditParams>),
        // git
        spec("git_status", "Git status", "Branch, upstream, ahead/behind and changed files.", Git, Effects::from_bits_truncate(Effects::IDEMPOTENT.bits() | Effects::LAUNCHES_PROCESS.bits()), schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("git_fetch", "Fetch", "git fetch for the session's repo.", Git, NET, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("git_pull", "Pull (fast-forward only)", "git pull --ff-only; refuses a diverged branch.", Git, NET, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("git_switch_branch", "Switch branch", "git switch to a local branch.", Git, GIT_WRITE, schema_of::<SwitchBranchParams>, echo::<SwitchBranchParams>),
        spec("git_branches", "Branches", "Local branches and which worktree holds each.", Git, Effects::LAUNCHES_PROCESS, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("worktree_list", "Worktrees", "The repo's worktrees with branch and status.", Git, Effects::LAUNCHES_PROCESS, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("worktree_new", "New worktree…", "Create a worktree from a branch and optionally open a session in it.", Git, GIT_WRITE, schema_of::<WorktreeNewParams>, echo::<WorktreeNewParams>),
        spec("worktree_remove", "Remove worktree…", "Remove a clean worktree (refuses dirty or nested ones).", Git, Effects::from_bits_truncate(GIT_WRITE.bits() | Effects::MAY_DISCARD_DATA.bits()), schema_of::<WorktreeRemoveParams>, echo::<WorktreeRemoveParams>),
        // notes, search
        spec("note_get", "Notes", "The note kept for a folder.", Notes, CONTENT, schema_of::<NoteGetParams>, echo::<NoteGetParams>),
        spec("note_set", "Save note", "Replace the note kept for a folder.", Notes, STATE, schema_of::<NoteSetParams>, echo::<NoteSetParams>),
        spec("search_output", "Search all output", "Full-text search of every session's output.", Search, CONTENT, schema_of::<SearchParams>, echo::<SearchParams>),
        spec("search_notes", "Search notes", "Search every folder's notes.", Search, CONTENT, schema_of::<SearchParams>, echo::<SearchParams>),
        // app
        spec("app_info", "About", "Version, the ConPTY in use and the app's paths.", App, READ, empty_schema, none),
        spec("settings_get", "Setting", "Read a setting.", App, READ, schema_of::<SettingsGetParams>, echo::<SettingsGetParams>),
        spec("settings_set", "Change a setting", "Change a setting (e.g. `theme`: `mocha` or `latte`).", App, STATE, schema_of::<SettingsSetParams>, echo::<SettingsSetParams>),
        spec("recently_closed_list", "Recently closed", "The last closed sessions, newest first.", App, READ, empty_schema, none),
        spec("open_folder", "Open folder", "Open the session's folder in the file manager.", App, Effects::LAUNCHES_PROCESS, schema_of::<SessionIdParams>, echo::<SessionIdParams>),
        spec("open_with", "Open with…", "Open the session's folder in an editor from the Open with list.", App, Effects::LAUNCHES_PROCESS, schema_of::<OpenWithParams>, echo::<OpenWithParams>),
        spec("grant_list", "MCP grants", "Clients allowed to call tools without asking.", App, READ, empty_schema, none),
        spec("grant_revoke", "Revoke an MCP grant", "Make a client ask again before calling a tool.", App, STATE, schema_of::<GrantRevokeParams>, echo::<GrantRevokeParams>),
        spec("state_import_wpf", "Import from the WPF app", "Import sessions, groups and settings from the old state.json.", App, STATE, schema_of::<ImportParams>, echo::<ImportParams>),
        spec("app_quit", "Quit", "Close the app; sessions get time to exit (the shutdown board).", App, Effects::MAY_DISCARD_DATA, schema_of::<QuitParams>, echo::<QuitParams>),
        // demo only: the review's status switcher. Not in docs/tools.md; removed at integration.
        spec("demo_set_status", "Demo: set status", "Demo only: put a session in a status, to review how each one looks.", App, UI, schema_of::<DemoStatusParams>, echo::<DemoStatusParams>),
    ]
}

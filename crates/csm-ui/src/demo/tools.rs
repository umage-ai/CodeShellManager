//! Tool params the screens send, and stand-ins for the ids in `docs/tools.md` the engine doesn't
//! register yet (git, notes, search, grants).
//!
//! - [`stand_ins`] with [`StandIn::NotBuilt`] is what the real app registers: each validates its
//!   params against the schema the real tool will take and then refuses with "not built yet", so
//!   a button that reaches one says so instead of pretending. Effects are declared honestly, as
//!   the real tools will, so consent and MCP hints already work.
//! - [`StandIn::Echo`] is the demo's: it echoes the params and [`super::DemoBackend`] applies the
//!   effect to its own model.
//! - [`demo_only`] is `demo_set_status`, the review's status switcher. Never in the real app.
//!
//! When the engine registers a real tool with one of these ids, delete the stand-in (the
//! registry refuses duplicates; `stand_ins_do_not_collide_with_engine_tools` catches it).
//! The param structs for engine-owned ids stay: the demo parses the calls with them, so they
//! must match the engine's schemas (same names, same `deny_unknown_fields`).

use csm_engine::{Ctx, Effects, Reply, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{GridPreset, GroupMode, Kind, LayoutMode};

/// Validate params as `P`, then echo them back as the result's data.
fn echo<P: DeserializeOwned + Serialize>(_: &mut Ctx<'_>, v: Value) -> Reply {
    let p: P = parse_params(v)?;
    Ok(ToolOutput::new("", serde_json::to_value(&p).unwrap_or(Value::Null)))
}

/// The real app's stand-in: params checked, then an honest refusal.
#[allow(dead_code)] // the stand-in seam: unused while every catalogue id is built
fn not_built<P: DeserializeOwned>(_: &mut Ctx<'_>, v: Value) -> Reply {
    let _: P = parse_params(v)?;
    Err(csm_engine::ToolError::Failed(NOT_BUILT.into()))
}

#[allow(dead_code)] // for no-param stand-ins
fn not_built_none(_: &mut Ctx<'_>, _: Value) -> Reply {
    Err(csm_engine::ToolError::Failed(NOT_BUILT.into()))
}

/// What a stand-in answers in the real app.
pub const NOT_BUILT: &str = "This isn't built yet in this version of CodeShellManager.";

#[allow(dead_code)] // for no-param stand-ins
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
    /// The folder to start in (local).
    #[serde(default)]
    pub folder: String,
    /// `claude`, `pwsh`, a preset's program. Default: `claude`.
    pub command: Option<String>,
    /// Arguments, as typed on a command line (`--continue`).
    pub args: Option<String>,
    pub name: Option<String>,
    pub group: Option<String>,
    pub kind: Option<KindParam>,
    pub ssh: Option<SshParams>,
    pub wsl: Option<WslParams>,
    /// Focus the new session (default true).
    pub focus: Option<bool>,
    pub after: Option<String>,
    /// Its own terminal look (a Windows Terminal profile's).
    pub look: Option<csm_core::look::Look>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LookSetParams {
    pub session_id: String,
    pub profile: Option<String>,
    pub look: Option<csm_core::look::Look>,
    #[serde(default)]
    pub clear: bool,
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
    pub args: Option<String>,
    pub folder: Option<String>,
    pub kind: Option<KindParam>,
    pub ssh: Option<SshParams>,
    pub wsl: Option<WslParams>,
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

/// `session_sleep_many`, `session_wake_many`, `session_restart_many`, `session_close_many`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionIdsParams {
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveManyParams {
    pub session_ids: Vec<String>,
    pub group_id: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SortParams {
    /// `name`, `folder`, `last_active`, `branch`, `dirty` or `repo`.
    pub by: String,
    pub descending: Option<bool>,
    /// A group id, `""` (ungrouped) or `all`.
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DuplicateParams {
    pub session_id: String,
    pub focus: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ViewSetParams {
    /// `rail`, `headers` or `flat`.
    pub view: Option<GroupModeParam>,
    /// The rail tab: a group id or `all`.
    pub rail_tab: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CollapseParams {
    pub group_id: String,
    pub collapsed: bool,
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
    Board,
}

impl From<LayoutModeParam> for LayoutMode {
    fn from(m: LayoutModeParam) -> LayoutMode {
        match m {
            LayoutModeParam::Focus => LayoutMode::Focus,
            LayoutModeParam::Side => LayoutMode::Side,
            LayoutModeParam::Overview => LayoutMode::Overview,
            LayoutModeParam::Grid => LayoutMode::Grid,
            LayoutModeParam::Free => LayoutMode::Free,
            LayoutModeParam::Board => LayoutMode::Board,
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
            LayoutMode::Board => LayoutModeParam::Board,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GridParam {
    pub cols: u8,
    pub rows: u8,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LayoutSetParams {
    /// A group id or `all`.
    pub group: Option<String>,
    pub mode: Option<LayoutModeParam>,
    /// Grid preset (`2x2`).
    pub preset: Option<String>,
    pub cols: Option<u8>,
    pub rows: Option<u8>,
    /// Free mode's split tree, floating and torn-off panes ([`crate::model::FreeLayout`]).
    pub free: Option<Value>,
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
    Usage,
    Docs,
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
    #[serde(default)]
    pub runs: Vec<Value>,
    /// Replace the list with the folder's project-type template.
    #[serde(default)]
    pub detect: bool,
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
    pub base: Option<String>,
    pub path: Option<String>,
    pub open_session: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRemoveParams {
    pub session_id: String,
    pub path: Option<String>,
    #[serde(default)]
    pub delete_branch: bool,
    #[serde(default)]
    pub force: bool,
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
    pub folder: Option<String>,
    pub session_id: Option<String>,
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

#[allow(dead_code)] // the stand-in seam: unused while every catalogue id is built
const READ: Effects = Effects::READ_ONLY;
const UI: Effects = Effects::UI;
#[allow(dead_code)] // the stand-in seam: unused while every catalogue id is built
const STATE: Effects = Effects::MUTATES_STATE;

/// How a stand-in answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandIn {
    /// The demo: echo the params (the demo backend applies the effect to its model).
    Echo,
    /// The real app: validate the params, then refuse with "not built yet".
    NotBuilt,
}

type Pair = (fn() -> Value, csm_engine::ToolFn);

/// `(schema, run)` for a stand-in taking `P`.
#[allow(dead_code)] // the stand-in seam: unused while every catalogue id is built
fn with<P: DeserializeOwned + Serialize + JsonSchema>(how: StandIn) -> Pair {
    (schema_of::<P>, if how == StandIn::Echo { echo::<P> } else { not_built::<P> })
}

#[allow(dead_code)] // for no-param stand-ins
fn no_params(how: StandIn) -> Pair {
    (empty_schema, if how == StandIn::Echo { none } else { not_built_none })
}

fn spec(id: &'static str, title: &'static str, summary: &'static str, toolset: Toolset, effects: Effects, (input_schema, run): Pair) -> ToolSpec {
    ToolSpec { id, title, summary, toolset, effects, input_schema, run }
}

/// The ids of `docs/tools.md` the engine doesn't have yet. Ids, titles, params and effects are
/// what the real tools will use.
pub fn stand_ins(_how: StandIn) -> Vec<ToolSpec> {
    // Every catalogue id is built now (engine tools, or the app's own `pane_*`). Kept as the seam
    // for the next tool that lands in the UI before the engine.
    Vec::new()
}

/// Tools only the demo has: the review's status switcher. Not in docs/tools.md.
pub fn demo_only() -> Vec<ToolSpec> {
    vec![spec(
        "demo_set_status",
        "Demo: set status",
        "Demo only: put a session in a status, to review how each one looks.",
        Toolset::App,
        UI,
        (schema_of::<DemoStatusParams>, echo::<DemoStatusParams>),
    )]
}

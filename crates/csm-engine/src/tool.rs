//! The tool contract: every user-visible action is a [`ToolSpec`].
//!
//! The desktop UI, the command palette, the keymap, the CLI and the MCP server all dispatch by
//! tool id through [`crate::Engine::call`]. Nothing the UI can do is unreachable from MCP, and
//! nothing MCP can do bypasses the engine's rules (consent, lifecycle guards, quoting).
//!
//! Naming (`docs/tools.md`): `snake_case`, `<area>_<verb>[_<object>]`, e.g. `session_new`,
//! `group_sleep_all`. The id is the MCP tool name; the CLI uses the kebab-case form.

use bitflags::bitflags;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

bitflags! {
    /// What a tool does beyond returning data. Declared honestly: the consent policy, the MCP
    /// annotations (`readOnlyHint`, `destructiveHint`, `openWorldHint`) and the palette's
    /// confirmation prompts are all derived from it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Effects: u16 {
        /// Returns the same result for the same state and changes nothing.
        const IDEMPOTENT = 1 << 0;
        /// Changes what the app shows (focus, layout, panels) but no session or file.
        const CHANGES_UI_STATE = 1 << 1;
        /// Changes persisted app state (sessions, groups, settings, notes).
        const MUTATES_STATE = 1 << 2;
        /// Starts a process (a session, a run command, git, wsl.exe, an editor).
        const LAUNCHES_PROCESS = 1 << 3;
        /// Writes keystrokes or text into a session. Equivalent to running code as the user.
        const SENDS_INPUT = 1 << 4;
        /// Can lose work: ends a process (close, sleep, restart, stop) or deletes something.
        const MAY_DISCARD_DATA = 1 << 5;
        /// Writes files outside the app's own state (git pull/switch, worktree add/remove).
        const WRITES_FILESYSTEM = 1 << 6;
        /// Talks to the network (git fetch/pull, update check).
        const ACCESSES_NETWORK = 1 << 7;
        /// Returns what a session printed (output, screen, notes). May contain secrets.
        const READS_SESSION_CONTENT = 1 << 8;
    }
}

impl Effects {
    pub const READ_ONLY: Effects = Effects::IDEMPOTENT;
    pub const UI: Effects = Effects::CHANGES_UI_STATE;

    /// MCP `readOnlyHint`.
    pub fn read_only_hint(self) -> bool {
        (self - (Effects::IDEMPOTENT | Effects::READS_SESSION_CONTENT)).is_empty()
    }

    /// MCP `destructiveHint`.
    pub fn destructive_hint(self) -> bool {
        self.intersects(Effects::MAY_DISCARD_DATA | Effects::SENDS_INPUT | Effects::WRITES_FILESYSTEM)
    }

    /// MCP `openWorldHint`.
    pub fn open_world_hint(self) -> bool {
        self.intersects(Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK)
    }

    /// Whether an [`Caller::External`] call needs the person's consent before it runs.
    /// UI calls never ask here; the UI asks its own questions (e.g. "Close all…?").
    pub fn needs_consent(self) -> bool {
        self.intersects(
            Effects::SENDS_INPUT
                | Effects::MAY_DISCARD_DATA
                | Effects::LAUNCHES_PROCESS
                | Effects::WRITES_FILESYSTEM
                | Effects::READS_SESSION_CONTENT,
        )
    }
}

/// Groups of tools. MCP clients start with [`Toolset::Core`] and enable others with the
/// `toolset_enable` tool, which keeps the definitions a client loads small.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Toolset {
    /// Sessions and the inbox: list, new, focus, status, read, send, wait.
    Core,
    Groups,
    Layout,
    Runs,
    Git,
    Notes,
    Search,
    App,
}

impl Toolset {
    pub const ALL: [Toolset; 8] =
        [Toolset::Core, Toolset::Groups, Toolset::Layout, Toolset::Runs, Toolset::Git, Toolset::Notes, Toolset::Search, Toolset::App];

    pub fn id(self) -> &'static str {
        match self {
            Toolset::Core => "core",
            Toolset::Groups => "groups",
            Toolset::Layout => "layout",
            Toolset::Runs => "runs",
            Toolset::Git => "git",
            Toolset::Notes => "notes",
            Toolset::Search => "search",
            Toolset::App => "app",
        }
    }

    pub fn from_id(s: &str) -> Option<Toolset> {
        Toolset::ALL.into_iter().find(|t| t.id() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            Toolset::Core => "Sessions",
            Toolset::Groups => "Groups",
            Toolset::Layout => "Layout and view",
            Toolset::Runs => "Run commands",
            Toolset::Git => "Git",
            Toolset::Notes => "Notes",
            Toolset::Search => "Search",
            Toolset::App => "App and settings",
        }
    }
}

/// Who is calling. Consent and some guards depend on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Caller {
    /// The person, through the app's own UI, palette or keymap.
    Ui,
    /// The person, through `csm <tool>` in a terminal.
    Cli,
    /// An MCP client. `client` is what it called itself at `initialize`; `session_id` is set when
    /// the client runs inside one of our sessions (so "Claude in Odulodo" can be named, and so a
    /// session can never be told to type into itself without that being visible).
    External { client: String, session_id: Option<String> },
}

impl Caller {
    pub fn is_external(&self) -> bool {
        matches!(self, Caller::External { .. })
    }
}

/// The error contract. Every message is plain English and says what to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "code", content = "message", rename_all = "snake_case")]
pub enum ToolError {
    #[error("no tool called `{0}`")]
    UnknownTool(String),
    #[error("{0}")]
    InvalidParams(String),
    #[error("{0}")]
    NotFound(String),
    /// The call is valid but the state forbids it now (a restart is running, the session is asleep…).
    #[error("{0}")]
    Refused(String),
    #[error("the person declined: {0}")]
    Declined(String),
    #[error("{0}")]
    Failed(String),
    #[error("timed out: {0}")]
    TimedOut(String),
}

/// What a tool returns. JSON for every caller; the UI may also read [`ToolOutput::message`] as a
/// toast, and MCP returns both (`structuredContent` + a text block).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// One short plain-English sentence about what happened ("Started claude in Odulodo.").
    pub message: String,
    pub data: Value,
}

impl ToolOutput {
    pub fn new(message: impl Into<String>, data: Value) -> Self {
        ToolOutput { message: message.into(), data }
    }

    pub fn message(message: impl Into<String>) -> Self {
        ToolOutput { message: message.into(), data: Value::Null }
    }
}

/// A tool's implementation. Runs on the engine thread with exclusive access to the state; it must
/// not block on slow work (spawn it and return, reporting progress through events).
pub type ToolFn = fn(&mut crate::Ctx<'_>, Value) -> Result<ToolOutput, ToolError>;

/// One registered tool.
#[derive(Clone, Copy)]
pub struct ToolSpec {
    /// `snake_case`; also the MCP tool name. The CLI uses [`ToolSpec::cli_name`].
    pub id: &'static str,
    /// Shown in the palette and menus ("New session…").
    pub title: &'static str,
    /// One or two sentences for agents and the palette's detail line.
    pub summary: &'static str,
    pub toolset: Toolset,
    pub effects: Effects,
    pub input_schema: fn() -> Value,
    pub run: ToolFn,
}

impl std::fmt::Debug for ToolSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSpec").field("id", &self.id).field("toolset", &self.toolset).finish()
    }
}

impl ToolSpec {
    pub fn cli_name(&self) -> String {
        self.id.replace('_', "-")
    }
}

/// JSON Schema for a params type.
pub fn schema_of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null)
}

/// The schema of a tool that takes no params.
pub fn empty_schema() -> Value {
    serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
}

/// Parse tool params; `null` is treated as `{}` so callers can omit them.
pub fn parse_params<T: DeserializeOwned>(v: Value) -> Result<T, ToolError> {
    let v = if v.is_null() { Value::Object(Default::default()) } else { v };
    serde_path_to_error::deserialize(v).map_err(|e| ToolError::InvalidParams(format!("{} (at `{}`)", e.inner(), e.path())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_follow_effects() {
        assert!(Effects::READ_ONLY.read_only_hint());
        assert!((Effects::IDEMPOTENT | Effects::READS_SESSION_CONTENT).read_only_hint());
        assert!(!Effects::UI.read_only_hint());
        assert!(Effects::SENDS_INPUT.destructive_hint());
        assert!(Effects::LAUNCHES_PROCESS.open_world_hint());
        assert!(!Effects::UI.needs_consent());
        assert!(Effects::SENDS_INPUT.needs_consent());
        assert!(Effects::READS_SESSION_CONTENT.needs_consent());
    }

    #[test]
    fn tool_error_serializes_and_round_trips() {
        // A tagged enum of newtype string variants needs `content`, or serde_json fails at runtime.
        let e = ToolError::Refused("busy".into());
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v, serde_json::json!({"code": "refused", "message": "busy"}));
        assert_eq!(serde_json::from_value::<ToolError>(v).unwrap(), e);
    }

    #[test]
    fn null_params_are_an_empty_object() {
        #[derive(Debug, Deserialize)]
        struct P {
            #[serde(default)]
            n: u32,
        }
        let p: P = parse_params(Value::Null).unwrap();
        assert_eq!(p.n, 0);
        let e = parse_params::<P>(serde_json::json!({"n": "x"})).unwrap_err();
        assert!(matches!(e, ToolError::InvalidParams(m) if m.contains("`n`")));
    }
}

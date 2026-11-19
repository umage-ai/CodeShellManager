//! What a front end needs from "the engine", wherever it lives.
//!
//! The MCP server and the CLI never touch the engine directly. They go through a [`Backend`],
//! which is either an engine in this process ([`LocalBackend`]) or the engine inside the running
//! desktop app, reached over the local socket ([`crate::RemoteBackend`]). Both answer the same two
//! questions: which tools exist (the registry is the only source of truth: nothing here knows a
//! tool by name), and what happens when one is called.

use csm_engine::{Caller, Effects, Engine, ToolError, ToolOutput, ToolSpec, Toolset};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One tool as a front end sees it: everything in [`ToolSpec`] except the function.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub toolset: Toolset,
    pub effects: Effects,
    pub input_schema: Value,
}

impl ToolInfo {
    pub fn from_spec(spec: &ToolSpec) -> ToolInfo {
        ToolInfo {
            id: spec.id.to_string(),
            title: spec.title.to_string(),
            summary: spec.summary.to_string(),
            toolset: spec.toolset,
            effects: spec.effects,
            input_schema: (spec.input_schema)(),
        }
    }

    /// The command name on the CLI (`session_new` is `session-new`).
    pub fn cli_name(&self) -> String {
        self.id.replace('_', "-")
    }
}

/// Why a call did not produce a tool result.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeError {
    /// The app could not be reached (not running, refused the token, went away). The message says
    /// what to do. The CLI exits with code 3 for this.
    Unreachable(String),
    /// The tool ran (or was refused by the engine) and failed.
    Tool(ToolError),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::Unreachable(m) => f.write_str(m),
            BridgeError::Tool(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BridgeError {}

impl From<ToolError> for BridgeError {
    fn from(e: ToolError) -> Self {
        BridgeError::Tool(e)
    }
}

/// Where tools run. `&mut self` because a remote backend owns a connection.
pub trait Backend: Send {
    /// Every registered tool, in registration order.
    fn tools(&mut self) -> Result<Vec<ToolInfo>, BridgeError>;

    /// Run a tool and wait for its result. A call that needs the person's consent waits for the
    /// answer (the engine gives up after [`csm_engine::CONSENT_TIMEOUT`]); a backend must not
    /// time out sooner than that.
    fn call(&mut self, caller: &Caller, tool: &str, params: Value) -> Result<ToolOutput, BridgeError>;
}

/// An engine in this process.
#[derive(Clone)]
pub struct LocalBackend {
    engine: Engine,
}

impl LocalBackend {
    pub fn new(engine: Engine) -> LocalBackend {
        LocalBackend { engine }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

impl Backend for LocalBackend {
    fn tools(&mut self) -> Result<Vec<ToolInfo>, BridgeError> {
        Ok(self.engine.registry().all().iter().map(ToolInfo::from_spec).collect())
    }

    fn call(&mut self, caller: &Caller, tool: &str, params: Value) -> Result<ToolOutput, BridgeError> {
        self.engine.call(caller.clone(), tool, params).map_err(BridgeError::Tool)
    }
}

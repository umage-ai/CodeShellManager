//! The MCP server: the registry's tools over stdio, behind progressive toolsets.
//!
//! Nothing here names a tool except the three MCP-only toolset tools and `consent_answer`, which
//! is hidden because it can never succeed for an MCP client. Everything else is whatever the
//! [`Backend`]'s registry holds, filtered by the toolsets the client has enabled (`core` to start,
//! as the Craft apps do, so a client pays context only for what it uses).

use std::collections::BTreeSet;
use std::io::{BufRead, Write};

use craft_mcp_server::{Effect, Incoming, Info, Server, Tools, object, read_message, response, text_result, tool};
use csm_engine::{Caller, ToolOutput, Toolset};
use serde_json::{Value, json};

use crate::catalog::{Backend, BridgeError, ToolInfo};
use crate::wire::clean_label;

const TOOLSET_LIST: &str = "toolset_list";
const TOOLSET_ENABLE: &str = "toolset_enable";
const TOOLSET_DISABLE: &str = "toolset_disable";

/// Tools an MCP client never sees. `consent_answer` is refused for every caller but the app's own
/// window, so listing it would only invite a client to try to approve itself.
const HIDDEN: [&str; 1] = ["consent_answer"];

const MAX_LINE: usize = 8 * 1024 * 1024;

const INSTRUCTIONS: &str = "CodeShellManager hosts many terminal sessions (Claude Code and other CLIs) in one desktop app; these tools drive it. \
Only the `core` toolset is listed at first: call toolset_list to see the others (groups, layout, runs, git, notes, search, app) and toolset_enable to add one. \
Tools that send input to a session, read what a session printed, start processes, write files or can lose work wait for the person to approve them in the app \
(Allow once, Always for this client, or Don't allow). Such a call can take up to two minutes to return while the card is open, and a refusal comes back as an error that says so. \
You cannot approve your own request. A session cannot send input to itself, and typing into a session that is mid-turn is refused unless forced. \
Start with the session tools in `core`.";

/// Options for [`McpService`].
#[derive(Debug, Clone, Default)]
pub struct McpOptions {
    /// The CodeShellManager session this server runs inside (`csm mcp --session <id>`). Sent with
    /// every call so the consent card can say "Claude in <session> wants to…", and so the engine
    /// can refuse a session typing into itself.
    pub session_id: Option<String>,
    /// List every tool, ignoring toolsets (tests, and clients that want everything).
    pub all_tools: bool,
}

/// The [`Tools`] implementation: toolset state plus a backend.
pub struct McpTools<B: Backend> {
    backend: B,
    catalog: Option<Vec<ToolInfo>>,
    last_error: Option<String>,
    enabled: BTreeSet<Toolset>,
    client: String,
    session_id: Option<String>,
    all_tools: bool,
    list_changed: bool,
}

impl<B: Backend> McpTools<B> {
    pub fn new(backend: B, options: McpOptions) -> Self {
        McpTools {
            backend,
            catalog: None,
            last_error: None,
            enabled: BTreeSet::from([Toolset::Core]),
            client: "mcp-client".to_string(),
            session_id: options.session_id.map(|s| clean_label(&s, 128)).filter(|s| !s.is_empty()),
            all_tools: options.all_tools,
            list_changed: false,
        }
    }

    /// The registry, fetched once it is reachable. A failure is remembered, not cached, so the next
    /// request tries again (the app may have been started in the meantime).
    fn catalog(&mut self) -> &[ToolInfo] {
        if self.catalog.is_none() {
            match self.backend.tools() {
                Ok(list) => {
                    self.catalog = Some(list);
                    self.last_error = None;
                }
                Err(e) => self.last_error = Some(e.to_string()),
            }
        }
        self.catalog.as_deref().unwrap_or_default()
    }

    fn is_meta(id: &str) -> bool {
        matches!(id, TOOLSET_LIST | TOOLSET_ENABLE | TOOLSET_DISABLE)
    }

    fn is_exposed(&self, info: &ToolInfo) -> bool {
        !HIDDEN.contains(&info.id.as_str()) && !Self::is_meta(&info.id)
    }

    fn is_listed(&self, info: &ToolInfo) -> bool {
        self.is_exposed(info) && (self.all_tools || self.enabled.contains(&info.toolset))
    }

    /// Why a known tool cannot be called yet, so the model is told the fix rather than "unknown".
    pub(crate) fn disabled_hint(&mut self, name: &str) -> Option<String> {
        if Self::is_meta(name) {
            return None;
        }
        let info = self.catalog().iter().find(|t| t.id == name)?.clone();
        if !self.is_exposed(&info) || self.is_listed(&info) {
            return None;
        }
        let set = info.toolset.id();
        Some(format!("`{name}` is in the `{set}` toolset, which is not enabled. Call toolset_enable with toolset \"{set}\" first, then call it again."))
    }

    pub(crate) fn take_list_changed(&mut self) -> bool {
        std::mem::take(&mut self.list_changed)
    }

    pub(crate) fn set_client(&mut self, name: &str) {
        let name = clean_label(name, 100);
        if !name.is_empty() {
            self.client = name;
        }
    }

    fn meta_tools() -> Vec<Value> {
        let sets: Vec<&str> = Toolset::ALL.iter().map(|t| t.id()).chain(std::iter::once("all")).collect();
        let pick = json!({"toolset": {"type": "string", "enum": sets, "description": "A toolset id, or `all`."}});
        vec![
            tool(TOOLSET_LIST, "List toolsets", "The toolsets of tools, with how many tools each holds and which are enabled. Only enabled toolsets are listed as tools.", object(json!({}), &[]), Effect::ReadOnly),
            tool(TOOLSET_ENABLE, "Enable a toolset", "Make a toolset's tools available to call. `core` is on already.", object(pick.clone(), &["toolset"]), Effect::Edits),
            tool(TOOLSET_DISABLE, "Disable a toolset", "Hide a toolset's tools again to save context.", object(pick, &["toolset"]), Effect::Edits),
        ]
    }

    fn definition(info: &ToolInfo) -> Value {
        let mut schema = info.input_schema.clone();
        if let Some(o) = schema.as_object_mut() {
            // Context is the scarce thing here: the dialect marker and the root title say nothing.
            o.remove("$schema");
            o.remove("title");
            o.entry("type").or_insert_with(|| json!("object"));
        } else {
            schema = object(json!({}), &[]);
        }
        let mut description = info.summary.clone();
        if info.effects.needs_consent() {
            description.push_str(" Asks the person for approval in the app first.");
        }
        json!({
            "name": info.id,
            "title": info.title,
            "description": description,
            "inputSchema": schema,
            "annotations": {
                "title": info.title,
                "readOnlyHint": info.effects.read_only_hint(),
                "destructiveHint": info.effects.destructive_hint(),
                "idempotentHint": info.effects.contains(csm_engine::Effects::IDEMPOTENT),
                "openWorldHint": info.effects.open_world_hint(),
            },
        })
    }

    fn toolset_counts(&mut self) -> Vec<(Toolset, usize)> {
        let catalog: Vec<ToolInfo> = self.catalog().to_vec();
        Toolset::ALL
            .iter()
            .map(|set| (*set, catalog.iter().filter(|t| t.toolset == *set && self.is_exposed(t)).count()))
            .filter(|(_, n)| *n > 0)
            .collect()
    }

    fn run_meta(&mut self, name: &str, args: &Value) -> Value {
        if name == TOOLSET_LIST {
            let counts = self.toolset_counts();
            let sets: Vec<Value> = counts
                .iter()
                .map(|(set, n)| json!({"id": set.id(), "title": set.title(), "tools": n, "enabled": self.all_tools || self.enabled.contains(set)}))
                .collect();
            let mut message = format!("{} toolsets.", sets.len());
            if let Some(e) = &self.last_error {
                message = format!("The app is not reachable right now ({e}).");
            }
            return output_result(&ToolOutput::new(message, json!({"toolsets": sets})));
        }
        let wanted = args.get("toolset").and_then(Value::as_str).unwrap_or("");
        let chosen: Vec<Toolset> = if wanted == "all" { Toolset::ALL.to_vec() } else { Toolset::from_id(wanted).into_iter().collect() };
        if chosen.is_empty() {
            return text_result(format!("There is no toolset `{wanted}`. Call toolset_list to see them."), true);
        }
        let enable = name == TOOLSET_ENABLE;
        let before = self.enabled.clone();
        for set in &chosen {
            if enable {
                self.enabled.insert(*set);
            } else if *set != Toolset::Core || wanted == "all" {
                // `core` stays unless the client says `all`: without it there is nothing to call.
                self.enabled.remove(set);
            }
        }
        if self.enabled != before {
            self.list_changed = true;
        }
        let catalog: Vec<ToolInfo> = self.catalog().to_vec();
        let ids: Vec<&str> = catalog.iter().filter(|t| chosen.contains(&t.toolset) && self.is_exposed(t)).map(|t| t.id.as_str()).collect();
        let verb = if enable { "Enabled" } else { "Disabled" };
        output_result(&ToolOutput::new(format!("{verb} `{wanted}` ({} tools).", ids.len()), json!({"toolset": wanted, "enabled": enable, "tools": ids})))
    }
}

/// A tool result: the message, then the data as JSON text (clients that read only text blocks still
/// get the data), and the data as `structuredContent` (an object, as MCP requires).
pub(crate) fn output_result(out: &ToolOutput) -> Value {
    let message = if out.message.is_empty() { "Done.".to_string() } else { out.message.clone() };
    let mut content = vec![json!({"type": "text", "text": message})];
    let mut result = json!({});
    if !out.data.is_null() {
        content.push(json!({"type": "text", "text": out.data.to_string()}));
        result["structuredContent"] = if out.data.is_object() { out.data.clone() } else { json!({"result": out.data}) };
    }
    result["content"] = Value::Array(content);
    result
}

impl<B: Backend> Tools for McpTools<B> {
    fn instructions(&self) -> String {
        INSTRUCTIONS.to_string()
    }

    fn list(&mut self) -> Vec<Value> {
        let mut defs = Self::meta_tools();
        let listed: Vec<Value> = {
            let catalog = self.catalog().to_vec();
            catalog.iter().filter(|t| self.is_listed(t)).map(Self::definition).collect()
        };
        defs.extend(listed);
        defs
    }

    fn call(&mut self, name: &str, args: &Value) -> Value {
        if Self::is_meta(name) {
            return self.run_meta(name, args);
        }
        let caller = Caller::External { client: self.client.clone(), session_id: self.session_id.clone() };
        match self.backend.call(&caller, name, args.clone()) {
            Ok(out) => output_result(&out),
            Err(BridgeError::Tool(e)) => text_result(e.to_string(), true),
            Err(BridgeError::Unreachable(m)) => text_result(m, true),
        }
    }
}

/// The MCP server for one client: framing, lifecycle and tool methods from `craft-mcp-server`,
/// plus what it cannot know by itself: the client's name (for the consent card), a hint when a
/// tool exists but its toolset is off, and `tools/list_changed` after a toolset changes.
pub struct McpService<B: Backend> {
    server: Server<McpTools<B>>,
}

impl<B: Backend> McpService<B> {
    pub fn new(backend: B, options: McpOptions) -> Self {
        let info = Info { name: "codeshell", title: "CodeShellManager", version: env!("CARGO_PKG_VERSION"), max_line: MAX_LINE };
        McpService { server: Server::new(info, McpTools::new(backend, options)) }
    }

    /// Serve newline-delimited JSON-RPC until `input` closes. Calls run one at a time: while a call
    /// waits for the person's approval (up to two minutes) this server answers nothing else.
    pub fn serve(&mut self, mut input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        while let Some(incoming) = read_message(&mut input, MAX_LINE)? {
            let lines = match incoming {
                Incoming::Line(line) => self.handle_line(&line),
                Incoming::Refused(reply) => vec![reply],
            };
            for line in lines {
                output.write_all(line.as_bytes())?;
                output.write_all(b"\n")?;
            }
            output.flush()?;
        }
        Ok(())
    }

    /// One line in, zero or more lines out (the reply, then any notification it caused).
    pub fn handle_line(&mut self, line: &str) -> Vec<String> {
        let parsed = serde_json::from_str::<Value>(line);
        let Ok(msg @ Value::Object(_)) = parsed else {
            // Parse errors, batches and junk: the library's own handling.
            return self.server.handle_line(line).into_iter().collect();
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let id = msg.get("id").cloned();

        if method == "initialize"
            && let Some(name) = params.pointer("/clientInfo/name").and_then(Value::as_str)
        {
            self.server.tools().set_client(name);
        }
        if method == "tools/call"
            && let (Some(id), Some(name)) = (id.clone(), params.get("name").and_then(Value::as_str))
            && let Some(hint) = self.server.tools().disabled_hint(name)
        {
            return vec![response(id, text_result(hint, true)).to_string()];
        }

        let mut reply = self.server.handle(msg);
        if method == "initialize"
            && let Some(r) = reply.as_mut()
        {
            // We do send `notifications/tools/list_changed` after a toolset changes.
            if let Some(tools) = r.pointer_mut("/result/capabilities/tools") {
                tools["listChanged"] = json!(true);
            }
        }
        let mut lines: Vec<String> = reply.into_iter().map(|r| r.to_string()).collect();
        if self.server.tools().take_list_changed() {
            lines.push(json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}).to_string());
        }
        lines
    }

    pub fn tools(&mut self) -> &mut McpTools<B> {
        self.server.tools()
    }
}

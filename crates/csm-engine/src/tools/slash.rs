//! `session_send_command`: the WPF ⚡ Commands panel. Types one of the configured Claude slash
//! commands (`ClaudeCommands`: /help, /clear, /compact, …) into a session and presses Enter.
//!
//! The text is app-authored (it comes from the settings, cleaned to one line with no control
//! characters, and the tool refuses anything not in that list), so it is typed, not pasted:
//! a slash command has to arrive as keys for Claude's prompt to treat it as one. Enter is its
//! own write, after the text. Arbitrary text goes through `session_send_text` (pasted) instead.

use std::time::Duration;

use csm_core::models::SlashCommand;
use csm_term::input;
use serde::Deserialize;
use serde_json::{Value, json};

use super::find;
use crate::engine::{Ctx, Reply};
use crate::session::Phase;
use crate::snapshot;
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        id: "session_send_command",
        title: "Send a Claude command",
        summary: "Type one of the configured Claude slash commands (settings ClaudeCommands: /help, /clear, /compact, /status, /memory, /init, exit) into a session and press Enter. Refused into the caller's own session, and while the session is working unless force is true.",
        toolset: Toolset::Core,
        effects: Effects::SENDS_INPUT,
        input_schema: schema_of::<SendCommandParams>,
        run: session_send_command,
    }]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendCommandParams {
    session_id: String,
    /// The command, exactly as listed in the `ClaudeCommands` setting (`/compact`).
    command: String,
    /// Send even while the session is working mid-turn.
    #[serde(default)]
    force: bool,
}

fn session_send_command(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SendCommandParams = parse_params(params)?;
    if let Caller::External { session_id: Some(own), .. } = ctx.caller
        && *own == p.session_id
    {
        return Err(ToolError::Refused("a session cannot send input to itself".into()));
    }
    let presets = SlashCommand::clean_list(&ctx.state.app.settings.claude_commands);
    let wanted = p.command.trim();
    let Some(cmd) = presets.iter().find(|c| c.command == wanted).map(|c| c.command.clone()) else {
        let list: Vec<&str> = presets.iter().map(|c| c.command.as_str()).collect();
        return Err(ToolError::InvalidParams(format!(
            "`{wanted}` is not one of the configured Claude commands ({}); send other text with session_send_text",
            if list.is_empty() { "none are configured".to_string() } else { list.join(", ") }
        )));
    };
    let i = find(ctx.state, &p.session_id)?;
    let asleep = ctx.state.app.sessions.get(i).is_some_and(|s| s.is_dormant);
    let Some(rt) = ctx.state.rt.sessions.get(&p.session_id).filter(|_| !asleep) else {
        return Err(ToolError::Refused("the session is asleep; wake it first".into()));
    };
    match rt.phase {
        Phase::Starting => return Err(ToolError::Refused("the session is still starting; wait for it and try again".into())),
        Phase::Exited { .. } => return Err(ToolError::Refused("the session's process has exited; restart it first".into())),
        Phase::Live => {}
    }
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound("gone".into()))?;
    let view = snapshot::session_view(ctx.state, s);
    if view.status.is_mid_turn() && !p.force {
        return Err(ToolError::Refused(format!("{} is working mid-turn; wait for it (session_wait until idle) or pass force: true.", view.name)));
    }
    let io = rt.io.clone();
    if !io.write(&input::typed(&cmd, false).0) {
        return Err(ToolError::Failed("the session is not accepting input".into()));
    }
    // Enter on its own write, after the text, so it submits rather than joining a burst.
    let spawned = std::thread::Builder::new().name("csm-submit".into()).spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        io.write(b"\r");
    });
    if spawned.is_err() {
        return Err(ToolError::Failed("could not send Enter".into()));
    }
    Ok(ToolOutput::new(format!("Sent {cmd} to {}.", view.name), json!({"session_id": p.session_id, "command": cmd})))
}

//! What `csm mcp` costs an MCP client in context: the bytes (and an estimated token count) of the
//! `tools/list` reply, for the always-on base and for each toolset when it is enabled.
//!
//! Measured through the real `McpService` over an in-process engine, so it follows whatever the
//! registry holds. Tokens are estimated as bytes / 4, which is close enough for JSON schemas to
//! compare toolsets; use the client's own counter when it matters.

use csm_engine::{Engine, State, Toolset};
use csm_mcp::{LocalBackend, McpOptions, McpService};
use serde_json::{Value, json};

fn call(service: &mut McpService<LocalBackend>, id: u64, method: &str, params: Value) -> Result<Value, String> {
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    let replies = service.handle_line(&line);
    let reply = replies.first().ok_or("no reply")?;
    serde_json::from_str(reply).map_err(|e| format!("bad reply: {e}"))
}

fn list_size(service: &mut McpService<LocalBackend>, id: u64) -> Result<(usize, usize), String> {
    let reply = call(service, id, "tools/list", json!({}))?;
    let tools = reply["result"]["tools"].as_array().ok_or("no tools array")?;
    Ok((tools.len(), serde_json::to_string(tools).map_err(|e| e.to_string())?.len()))
}

pub fn run(markdown: bool) -> Result<(), String> {
    let engine = Engine::start(State::default(), Vec::new()).map_err(|e| format!("could not start an engine: {e}"))?;
    let mut service = McpService::new(LocalBackend::new(engine.clone()), McpOptions::default());
    call(
        &mut service,
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "xtask", "version": "0"}}),
    )?;
    let mut id = 2;
    let (base_tools, base_bytes) = list_size(&mut service, id)?;
    let mut rows = vec![("base (core + toolset tools)".to_owned(), base_tools, base_bytes)];
    let (mut tools, mut bytes) = (base_tools, base_bytes);
    for set in Toolset::ALL {
        if set == Toolset::Core {
            continue;
        }
        id += 1;
        call(&mut service, id, "tools/call", json!({"name": "toolset_enable", "arguments": {"toolset": set.id()}}))?;
        id += 1;
        let (t, b) = list_size(&mut service, id)?;
        rows.push((format!("+ {}", set.id()), t - tools, b.saturating_sub(bytes)));
        (tools, bytes) = (t, b);
    }
    rows.push(("everything enabled".to_owned(), tools, bytes));
    engine.shutdown();
    if markdown {
        println!("| toolset | tools | bytes | est. tokens |\n|---|---|---|---|");
    } else {
        println!("{:<30} {:>6} {:>9} {:>12}", "toolset", "tools", "bytes", "est. tokens");
    }
    for (name, t, b) in rows {
        if markdown {
            println!("| {name} | {t} | {b} | ~{} |", b / 4);
        } else {
            println!("{name:<30} {t:>6} {b:>9} {:>12}", format!("~{}", b / 4));
        }
    }
    Ok(())
}

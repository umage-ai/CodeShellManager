//! Small fake tools for the tests: the real tools belong to the engine work, and the registry is
//! all this crate may depend on.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use csm_engine::{Ctx, Effects, Engine, State, ToolError, ToolOutput, ToolSpec, Toolset, parse_params};
use serde::Deserialize;
use serde_json::{Value, json};

fn echo_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "EchoParams",
        "type": "object",
        "properties": {
            "text": {"type": "string", "description": "What to echo."},
            "times": {"type": ["integer", "null"], "description": "Repeat count."}
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoParams {
    text: String,
    times: Option<u32>,
}

fn echo(_: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: EchoParams = parse_params(params)?;
    let text = p.text.repeat(p.times.unwrap_or(1) as usize);
    Ok(ToolOutput::new(format!("Echoed {text}."), json!({"text": text})))
}

fn missing(_: &mut Ctx<'_>, _: Value) -> Result<ToolOutput, ToolError> {
    Err(ToolError::NotFound("No session called nothing. Call session_list to see them.".into()))
}

fn list(_: &mut Ctx<'_>, _: Value) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::new("2 things.", json!([{"id": "a"}, {"id": "b"}])))
}

fn whoami(ctx: &mut Ctx<'_>, _: Value) -> Result<ToolOutput, ToolError> {
    let caller = serde_json::to_value(ctx.caller).unwrap_or(Value::Null);
    Ok(ToolOutput::new("Here.", caller))
}

fn sent(_: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let _: EchoParams = parse_params(params)?;
    Ok(ToolOutput::message("Sent."))
}

fn branch(_: &mut Ctx<'_>, _: Value) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::message("On main."))
}

fn toast(ctx: &mut Ctx<'_>, _: Value) -> Result<ToolOutput, ToolError> {
    ctx.events.toast("hello from a tool");
    Ok(ToolOutput::message("Toasted."))
}

fn none() -> Value {
    csm_engine::empty_schema()
}

pub fn specs() -> Vec<ToolSpec> {
    let spec = |id, toolset, effects, input_schema: fn() -> Value, run| ToolSpec { id, title: "Fake", summary: "A fake tool.", toolset, effects, input_schema, run };
    vec![
        spec("fake_echo", Toolset::Core, Effects::READ_ONLY, echo_schema, echo),
        spec("fake_missing", Toolset::Core, Effects::READ_ONLY, none, missing),
        spec("fake_list", Toolset::Core, Effects::READ_ONLY, none, list),
        spec("fake_whoami", Toolset::Core, Effects::READ_ONLY, none, whoami),
        spec("fake_send", Toolset::Core, Effects::SENDS_INPUT, echo_schema, sent),
        spec("fake_toast", Toolset::Core, Effects::UI, none, toast),
        spec("fake_branch", Toolset::Git, Effects::READ_ONLY, none, branch),
        spec("fake_layout", Toolset::Layout, Effects::UI, none, branch),
    ]
}

pub fn engine() -> Engine {
    Engine::start(State::default(), specs()).unwrap()
}

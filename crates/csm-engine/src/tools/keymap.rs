//! The keyboard map (`keymap_list`, `keymap_set`, `keymap_reset`): the actions and their chords,
//! as the app uses them. The rules are in `csm_core::keymap`; this module reads and writes the
//! person's overrides in the settings (`AppSettings::keymap`), persists them and tells the UI.
//!
//! Changing a chord changes what a later keystroke starts, so the tools declare `LAUNCHES_PROCESS` as
//! well as `MUTATES_STATE`: an MCP caller asks the person first (`Effects::needs_consent`), like any
//! change that decides what runs. Nothing here starts a process or reaches the terminal, and
//! `csm_core::keymap::set` refuses the Ctrl+letter chords a terminal program needs.

use csm_core::keymap::{self, Keymap, Overrides, SetError};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

/// The settings key the UI follows (`SettingChanged { key, value }`).
pub const SETTING_KEY: &str = "Keymap";

pub(super) fn specs() -> Vec<ToolSpec> {
    let changes = Effects::MUTATES_STATE.union(Effects::LAUNCHES_PROCESS).union(Effects::IDEMPOTENT);
    vec![
        ToolSpec {
            id: "keymap_list",
            title: "Keyboard shortcuts",
            summary: "Every action with its chords (the defaults and the person's own), the chords two actions share (a conflict: the first claim is kept), override entries that were skipped as damaged, and ids this build does not know.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: keymap_list,
        },
        ToolSpec {
            id: "keymap_set",
            title: "Change a keyboard shortcut…",
            summary: "Set the chords of one action, e.g. [\"Ctrl+Shift+P\"]; an empty list removes its chords. Refused when a chord is used by another action, cannot be read, needs Ctrl or a function key, is Ctrl+Alt with a letter, digit or punctuation key (AltGr on many keyboards), or is a Ctrl+letter the terminal needs (Ctrl+A, C, D, E, L, R, U, W, Z). Setting the defaults removes the person's override.",
            toolset: Toolset::App,
            effects: changes,
            input_schema: schema_of::<SetParams>,
            run: keymap_set,
        },
        ToolSpec {
            id: "keymap_reset",
            title: "Reset keyboard shortcuts",
            summary: "Put one action's chords back to the defaults, or every action's (without `action`).",
            toolset: Toolset::App,
            effects: changes,
            input_schema: schema_of::<ResetParams>,
            run: keymap_reset,
        },
    ]
}

/// The current map: the settings' overrides, resolved.
fn current(ctx: &Ctx<'_>) -> Keymap {
    Keymap::new(&ctx.state.app.settings.keymap)
}

fn keymap_list(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let map = current(ctx);
    let actions: Vec<Value> = map
        .entries()
        .iter()
        .map(|e| {
            json!({
                "id": e.id(),
                "name": e.name(),
                "chords": e.chords().iter().map(ToString::to_string).collect::<Vec<_>>(),
                "default": e.default_chords().iter().map(ToString::to_string).collect::<Vec<_>>(),
                "customised": e.customised(),
            })
        })
        .collect();
    let conflicts: Vec<Value> = map
        .conflicts()
        .iter()
        .map(|c| json!({"chord": c.chord.to_string(), "kept": c.kept, "dropped": c.dropped}))
        .collect();
    let skipped: Vec<Value> = map.skipped().iter().map(|s| json!({"id": s.id, "chord": s.chord, "reason": s.reason})).collect();
    let summary = format!(
        "{} actions; {} customised; {} conflict(s); {} skipped; {} unknown.",
        actions.len(),
        map.entries().iter().filter(|e| e.customised()).count(),
        conflicts.len(),
        skipped.len(),
        map.unknown().len()
    );
    Ok(ToolOutput::new(
        summary,
        json!({"actions": actions, "conflicts": conflicts, "skipped": skipped, "unknown": map.unknown()}),
    ))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParams {
    /// The action's id, as `keymap_list` shows it (a tool id, or `ui.<name>`).
    action: String,
    /// The chords, e.g. `["Ctrl+Shift+P", "F3"]`. Modifiers are Ctrl, Alt and Shift; keys are
    /// letters, digits, F1-F12, arrows, Home, End, PgUp, PgDn, Tab, Space, Enter, Del, Ins, `,` `.` `/` `-` `=`.
    chords: Vec<String>,
}

fn keymap_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SetParams = parse_params(params)?;
    let updated = keymap::set(&ctx.state.app.settings.keymap, &p.action, &p.chords).map_err(|e| match e {
        SetError::UnknownAction(_) => ToolError::NotFound(e.to_string()),
        SetError::BadChord(_) => ToolError::InvalidParams(e.to_string()),
        SetError::InUse { .. } | SetError::Reserved(_) => ToolError::Refused(e.to_string()),
    })?;
    store(ctx, updated);
    let shown: Vec<String> = current(ctx).chords(&p.action).iter().map(ToString::to_string).collect();
    let msg = if shown.is_empty() { format!("{} has no shortcut now.", p.action) } else { format!("{} is {}.", p.action, shown.join(" or ")) };
    Ok(ToolOutput::new(msg, json!({"action": p.action, "chords": shown})))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ResetParams {
    /// One action's id; without it, every action goes back to its defaults.
    action: Option<String>,
}

fn keymap_reset(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ResetParams = parse_params(params)?;
    if let Some(id) = &p.action
        && !current(ctx).entries().iter().any(|e| e.id() == id.as_str())
    {
        return Err(ToolError::NotFound(format!("there is no action named `{id}`")));
    }
    let updated: Overrides = keymap::reset(&ctx.state.app.settings.keymap, p.action.as_deref());
    store(ctx, updated);
    let what = p.action.as_deref().map_or_else(|| "every shortcut".to_owned(), |id| format!("`{id}`"));
    Ok(ToolOutput::new(format!("{what} back to the default."), json!({"action": p.action})))
}

/// Keeps the overrides, saves them and tells the UI what they are now.
fn store(ctx: &mut Ctx<'_>, overrides: Overrides) {
    ctx.state.app.settings.keymap = overrides;
    ctx.state.persist();
    let value = json!(ctx.state.app.settings.keymap);
    ctx.events.emit(crate::Event::SettingChanged { key: SETTING_KEY.to_owned(), value });
}

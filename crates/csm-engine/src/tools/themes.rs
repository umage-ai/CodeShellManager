//! Themes (docs/addons.md, "Themes"): `theme_list`, `theme_set` (app toolset).
//!
//! The built-in Mocha and Latte, plus the `[[theme]]` palettes of enabled add-ons
//! (`addon-id.theme-id`). The app draws `ThemeDark` in dark mode and `ThemeLight` in light mode;
//! `Theme` (dark, light or system) picks between them. A chosen theme that is not available any
//! more (its add-on was disabled or removed) falls back to the built-in, and these tools say so.

use csm_core::addons::{BUILTIN_THEMES, ThemeBase};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::addons;
use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "theme_list",
            title: "Themes",
            summary: "The themes to choose from: the built-in Mocha (dark) and Latte (light), and the themes of enabled add-ons (`addon-id.theme-id`), with the mode and the dark and light choices in force. A choice that is no longer available says so (the app draws the built-in instead).",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: theme_list,
        },
        ToolSpec {
            id: "theme_set",
            title: "Theme…",
            summary: "Choose the theme: `mode` (dark, light or system, which follows the desktop), the `dark` theme and the `light` theme, by the ids `theme_list` gives. Give any of them. A dark theme only goes in `dark`, a light one only in `light`. The window changes at once.",
            toolset: Toolset::App,
            effects: Effects::UI.union(Effects::IDEMPOTENT),
            input_schema: schema_of::<SetParams>,
            run: theme_set,
        },
    ]
}

/// One theme as the tools show it.
struct Offered {
    id: String,
    name: String,
    base: ThemeBase,
    /// The add-on's name, `None` for a built-in.
    source: Option<String>,
    description: String,
}

fn offered(ctx: &Ctx<'_>) -> Vec<Offered> {
    let mut out: Vec<Offered> = BUILTIN_THEMES
        .iter()
        .map(|(id, name, base)| Offered { id: (*id).to_owned(), name: (*name).to_owned(), base: *base, source: None, description: String::new() })
        .collect();
    out.extend(addons::themes(&ctx.state.rt.addons).map(|(addon, addon_name, t)| Offered {
        id: format!("{addon}.{}", t.id),
        name: t.name.clone(),
        base: t.base,
        source: Some(addon_name.to_owned()),
        description: t.description.clone(),
    }));
    out
}

/// Why `id` cannot be drawn as the `want` theme, or `None` when it can.
fn unavailable(list: &[Offered], id: &str, want: ThemeBase) -> Option<String> {
    match list.iter().find(|t| t.id == id) {
        Some(t) if t.base == want => None,
        Some(t) => Some(format!("`{id}` is a {} theme, not a {} one", t.base.id(), want.id())),
        None if id.contains('.') => Some(format!("`{id}` is not available: its add-on is not enabled, or has no such theme")),
        None => Some(format!("`{id}` is not a theme")),
    }
}

fn theme_list(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let list = offered(ctx);
    let s = &ctx.state.app.settings;
    let slot = |id: &str, base: ThemeBase| {
        let why = unavailable(&list, id, base);
        json!({
            "id": id,
            "drawn": if why.is_some() { base.builtin_id() } else { id },
            "fallback": why.map(|w| format!("{w}; {} is drawn instead", base.builtin_id())),
        })
    };
    let data = json!({
        "mode": s.theme.to_ascii_lowercase(),
        "dark": slot(&s.theme_dark, ThemeBase::Dark),
        "light": slot(&s.theme_light, ThemeBase::Light),
        "themes": list.iter().map(|t| json!({"id": t.id, "name": t.name, "base": t.base.id(), "source": t.source, "description": t.description})).collect::<Vec<_>>(),
    });
    Ok(ToolOutput::new(format!("{} themes.", list.len()), data))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct SetParams {
    /// `dark`, `light` or `system` (follows the desktop).
    mode: Option<String>,
    /// The theme drawn in dark mode: `mocha` or a dark add-on theme.
    dark: Option<String>,
    /// The theme drawn in light mode: `latte` or a light add-on theme.
    light: Option<String>,
}

fn theme_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SetParams = parse_params(params)?;
    if p.mode.is_none() && p.dark.is_none() && p.light.is_none() {
        return Err(ToolError::InvalidParams("give `mode`, `dark` or `light`".into()));
    }
    let mode = p.mode.map(|m| m.trim().to_ascii_lowercase());
    if let Some(m) = &mode
        && !matches!(m.as_str(), "dark" | "light" | "system")
    {
        return Err(ToolError::InvalidParams(format!("mode `{m}` is not dark, light or system")));
    }
    let list = offered(ctx);
    for (id, base) in [(&p.dark, ThemeBase::Dark), (&p.light, ThemeBase::Light)] {
        if let Some(id) = id
            && let Some(why) = unavailable(&list, id, base)
        {
            return Err(ToolError::InvalidParams(format!("{why}; theme_list shows the {} themes", base.id())));
        }
    }
    let s = &mut ctx.state.app.settings;
    let mut changed: Vec<(&str, String)> = Vec::new();
    if let Some(m) = mode
        && s.theme != m
    {
        s.theme.clone_from(&m);
        changed.push(("Theme", m));
    }
    if let Some(d) = p.dark
        && s.theme_dark != d
    {
        s.theme_dark.clone_from(&d);
        changed.push(("ThemeDark", d));
    }
    if let Some(l) = p.light
        && s.theme_light != l
    {
        s.theme_light.clone_from(&l);
        changed.push(("ThemeLight", l));
    }
    let data = json!({"mode": s.theme, "dark": s.theme_dark, "light": s.theme_light});
    if !changed.is_empty() {
        ctx.state.persist();
        for (key, value) in changed {
            ctx.events.emit(crate::Event::SettingChanged { key: key.to_owned(), value: Value::String(value) });
        }
    }
    let msg = format!("Theme: {} (dark {}, light {}).", data["mode"].as_str().unwrap_or_default(), data["dark"].as_str().unwrap_or_default(), data["light"].as_str().unwrap_or_default());
    Ok(ToolOutput::new(msg, data))
}

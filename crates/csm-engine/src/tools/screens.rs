//! Add-on screens as tools (docs/addons.md, "Screens"): list what running add-ons offer and what
//! is open, open one on a session or project, close it, and move it to its own window or back.
//!
//! All of them are about what the app shows, not about a session or a file, so they declare
//! `CHANGES_UI_STATE` at most: an MCP agent may say "I opened the review for you" without a card,
//! like `session_focus`. What a screen's buttons do is each button's own tool, through the normal
//! dispatch (see `csm_core::addons::screen::action_policy`).

use csm_core::addons::screen::{ScreenContext, ScreenScope};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::events::Event;
use crate::plugin_screens::{self, Opened, ScreenPhase};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "screen_list",
            title: "List add-on screens",
            summary: "The screens running add-ons offer (id, title, scope: project, session or global) and the screens that are open now (instance, its add-on, phase, whether it is in its own window, why the last view was refused).",
            toolset: Toolset::Addons,
            effects: Effects::IDEMPOTENT,
            input_schema: crate::tool::empty_schema,
            run: screen_list,
        },
        ToolSpec {
            id: "screen_open",
            title: "Open an add-on screen",
            summary: "Open one of a running add-on's screens (from screen_list), on a session, a project or a folder, as a tab above the panes. The same screen on the same context is focused instead of opened twice. Returns the instance id. An add-on has at most 8 screens open.",
            toolset: Toolset::Addons,
            effects: Effects::CHANGES_UI_STATE,
            input_schema: schema_of::<OpenParams>,
            run: screen_open,
        },
        ToolSpec {
            id: "screen_close",
            title: "Close an add-on screen",
            summary: "Close an open add-on screen (the add-on is told). Its drafts are lost; the add-on's own data is not touched.",
            toolset: Toolset::Addons,
            effects: Effects::CHANGES_UI_STATE,
            input_schema: schema_of::<InstanceParams>,
            run: screen_close,
        },
        ToolSpec {
            id: "screen_pop_out",
            title: "Pop an add-on screen out",
            summary: "Move an open add-on screen into its own window.",
            toolset: Toolset::Addons,
            effects: Effects::CHANGES_UI_STATE,
            input_schema: schema_of::<InstanceParams>,
            run: screen_pop_out,
        },
        ToolSpec {
            id: "screen_dock",
            title: "Dock an add-on screen",
            summary: "Move an add-on screen from its own window back into the tab strip.",
            toolset: Toolset::Addons,
            effects: Effects::CHANGES_UI_STATE,
            input_schema: schema_of::<InstanceParams>,
            run: screen_dock,
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenParams {
    /// The screen id, `<add-on>.<name>`, from screen_list.
    screen: String,
    /// What to open it on. A session id gives the screen its project and folder.
    #[serde(default)]
    context: Option<ContextParams>,
    /// Show it now (default true).
    #[serde(default)]
    focus: Option<bool>,
    /// `dark` or `light`, passed to the add-on.
    #[serde(default)]
    theme: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContextParams {
    session_id: Option<String>,
    /// A project id (what `task_*` tools call a project).
    project: Option<String>,
    /// A folder or file path the screen is about.
    path: Option<String>,
    /// Anything else the add-on wants back, at most 4 KiB as JSON.
    args: Option<Value>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct InstanceParams {
    /// The instance id from screen_open or screen_list.
    instance: String,
}

fn screen_list(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let store = ctx.engine.screens();
    let (declared, open) = (store.declared(), store.instances());
    Ok(ToolOutput::new(format!("{} screen(s) offered, {} open.", declared.len(), open.len()), json!({"screens": declared, "instances": open})))
}

fn screen_open(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: OpenParams = parse_params(params)?;
    let store = ctx.engine.screens().clone();
    let addon = store
        .declared()
        .into_iter()
        .find(|d| d.id == p.screen)
        .ok_or_else(|| ToolError::NotFound(format!("no running add-on offers a screen `{}`; `screen_list` shows them", p.screen)))?;
    let c = p.context.unwrap_or(ContextParams { session_id: None, project: None, path: None, args: None });
    let mut context = ScreenContext { session_id: None, project: c.project, folder: None, path: c.path, args: c.args };
    if let Some(sid) = c.session_id {
        let i = super::find(ctx.state, &sid)?;
        let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound(format!("no session `{sid}`")))?;
        context.project = context.project.or_else(|| Some(crate::snapshot::project_of(ctx.state, s)));
        context.folder = Some(s.working_folder.clone()).filter(|f| !f.is_empty());
        context.session_id = Some(sid);
    }
    match addon.scope {
        ScreenScope::Session if context.session_id.is_none() => {
            return Err(ToolError::InvalidParams(format!("`{}` is a session screen: give `context.session_id`", p.screen)));
        }
        ScreenScope::Project if context.project.is_none() => {
            return Err(ToolError::InvalidParams(format!("`{}` is a project screen: give `context.session_id` or `context.project`", p.screen)));
        }
        _ => {}
    }
    context.validate().map_err(|e| ToolError::InvalidParams(e.to_string()))?;
    let link = ctx.engine.host.link(&addon.addon);
    let focus = p.focus.unwrap_or(true);
    let theme = p.theme.filter(|t| t == "dark" || t == "light");
    let (instance, created) = match store.open(&addon.addon, &p.screen, context.clone())? {
        Opened::Created(i) => (i, true),
        // A screen that did not answer is opened again when the person asks again.
        Opened::Existing(i, ScreenPhase::NotAnswering) => {
            store.reopening(&i);
            (i, true)
        }
        Opened::Existing(i, _) => (i, false),
    };
    if created {
        match link {
            Some(l) => {
                let generation = store.instance_generation(&instance).unwrap_or(0);
                plugin_screens::request_open(store, l, addon.addon.clone(), generation, instance.clone(), p.screen.clone(), context, theme);
            }
            None => return Err(ToolError::Refused(format!("the add-on `{}` is not running", addon.addon))),
        }
    }
    if focus {
        ctx.events.emit(Event::ScreenFocusRequested { instance: instance.clone() });
    }
    let msg = if created { format!("Opening {}.", addon.title) } else { format!("{} is already open.", addon.title) };
    Ok(ToolOutput::new(msg, json!({"instance": instance, "created": created})))
}

fn screen_close(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: InstanceParams = parse_params(params)?;
    let addon = ctx.engine.screens().close(&p.instance).ok_or_else(|| ToolError::NotFound(format!("no open screen `{}`; `screen_list` shows them", p.instance)))?;
    if let Some(l) = ctx.engine.host.link(&addon) {
        l.notify("screen/closed", json!({"instance": p.instance}));
    }
    Ok(ToolOutput::new("Closed.", json!({"instance": p.instance})))
}

fn screen_pop_out(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: InstanceParams = parse_params(params)?;
    ctx.engine.screens().set_popped_out(&p.instance, true)?;
    Ok(ToolOutput::new("In its own window.", json!({"instance": p.instance})))
}

fn screen_dock(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: InstanceParams = parse_params(params)?;
    ctx.engine.screens().set_popped_out(&p.instance, false)?;
    ctx.events.emit(Event::ScreenFocusRequested { instance: p.instance.clone() });
    Ok(ToolOutput::new("Back in the tab strip.", json!({"instance": p.instance})))
}

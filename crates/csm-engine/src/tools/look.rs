//! Windows Terminal profiles and a session's own look: `profile_list`, `session_look_set`
//! (docs/tools.md, app toolset). `session_new` takes a `look` too.
//!
//! The look is stamped onto the session (the WPF app's `Profile*` fields, persisted with it) and
//! stays put if the profile later changes in Windows Terminal. Reading WT's `settings.json` is
//! file I/O: it runs on a worker thread, never on the engine thread.

use csm_core::look::{CursorShape, Look, LookColors};
use csm_services::wt_profiles::WtProfile;
use csm_term::alacritty_terminal::vte::ansi::{CursorShape as TermCursor, Rgb};
use serde::Deserialize;
use serde_json::{Value, json};

use super::find;
use crate::engine::{Ctx, Reply};
use crate::state::State;
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "profile_list",
            title: "Windows Terminal profiles",
            summary: "The profiles in Windows Terminal's settings (Stable, Preview and unpackaged installs): name, command line, starting folder and look, ready for `session_look_set` or `session_new`'s `look`. Needs the ImportWindowsTerminalProfiles setting.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: profile_list,
        },
        ToolSpec {
            id: "session_look_set",
            title: "Session look…",
            summary: "Give a session its own terminal look: a Windows Terminal profile's (by name), an explicit look (colours, font size, cursor shape, padding), or none (`clear`). A running session changes at once.",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE,
            input_schema: schema_of::<LookSetParams>,
            run: session_look_set,
        },
    ]
}

/// Off unless the person opted in (the WPF app's rule: another program's settings are read only
/// when asked to).
fn import_refusal(state: &State) -> Option<ToolError> {
    (!state.app.settings.import_windows_terminal_profiles).then(|| {
        ToolError::Refused("Windows Terminal profile import is off; turn on ImportWindowsTerminalProfiles in Settings › Appearance".into())
    })
}

/// A worker-thread loader for the profiles (the configured files in tests, the install
/// locations otherwise).
fn loader(state: &State) -> impl FnOnce() -> Vec<WtProfile> + Send + 'static {
    let files = state.options.terminal_profile_files.clone();
    move || match files {
        Some(f) => csm_services::wt_profiles::load_from(&f),
        None => csm_services::wt_profiles::load_profiles(),
    }
}

/// A profile's appearance as a [`Look`].
pub(crate) fn look_of(p: &WtProfile) -> Look {
    Look {
        colors: p.color_scheme_json.as_deref().and_then(LookColors::from_xterm_json).filter(|c| !c.is_empty()),
        font_family: p.font_family.as_deref().and_then(csm_core::look::unquote_font_family),
        font_size: p.font_size.filter(|s| (4..=72).contains(s)),
        font_weight: p.font_weight.clone(),
        ligatures: p.font_ligatures,
        cursor_shape: p.cursor_shape.as_deref().and_then(CursorShape::from_id),
        cursor_blink: p.cursor_blink,
        padding: p.padding.as_deref().and_then(csm_core::look::parse_css_padding).map(|e| e.to_vec()),
        opacity: p.background_opacity,
        retro: p.retro_effect,
    }
}

fn profile_json(p: &WtProfile) -> Value {
    json!({
        "name": p.name,
        "source": p.source,
        "guid": p.guid,
        "commandline": p.commandline,
        "starting_directory": p.starting_directory,
        "look": look_of(p),
    })
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), ToolError> {
    std::thread::Builder::new().name(name.into()).spawn(work).map(|_| ()).map_err(|e| ToolError::Failed(format!("could not start reading the profiles: {e}")))
}

fn profile_list(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    if let Some(e) = import_refusal(ctx.state) {
        return Err(e);
    }
    let load = loader(ctx.state);
    let Some(reply) = ctx.park() else { return Ok(ToolOutput::message("No answer channel.")) };
    spawn("csm-wt-profiles", move || {
        let list: Vec<Value> = load().iter().map(profile_json).collect();
        let message = match list.len() {
            0 => "No Windows Terminal profiles found.".to_owned(),
            1 => "1 Windows Terminal profile.".to_owned(),
            n => format!("{n} Windows Terminal profiles."),
        };
        reply.send(Ok(ToolOutput::new(message, Value::Array(list))));
    })?;
    Ok(ToolOutput::message("Reading Windows Terminal's settings."))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LookSetParams {
    session_id: String,
    /// A Windows Terminal profile's name, as `profile_list` gives it: its look is stamped on.
    #[serde(default)]
    profile: Option<String>,
    /// The look itself (replaces the session's whole look).
    #[serde(default)]
    look: Option<Look>,
    /// Remove the session's look: it follows the app's settings again.
    #[serde(default)]
    clear: bool,
}

fn session_look_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: LookSetParams = parse_params(params)?;
    find(ctx.state, &p.session_id)?;
    let given = usize::from(p.profile.is_some()) + usize::from(p.look.is_some()) + usize::from(p.clear);
    if given != 1 {
        return Err(ToolError::InvalidParams("give exactly one of `profile`, `look` or `clear: true`".into()));
    }
    if p.clear {
        return set_look(ctx.state, &p.session_id, &Look::default(), None);
    }
    if let Some(look) = p.look {
        return set_look(ctx.state, &p.session_id, &look, None);
    }
    let wanted = p.profile.unwrap_or_default();
    if let Some(e) = import_refusal(ctx.state) {
        return Err(e);
    }
    let load = loader(ctx.state);
    let reply = ctx.park();
    let engine = ctx.engine.clone();
    let id = p.session_id;
    spawn("csm-wt-profile", move || {
        let profiles = load();
        let found = profiles
            .iter()
            .find(|x| x.name == wanted)
            .or_else(|| profiles.iter().find(|x| x.name.eq_ignore_ascii_case(wanted.trim())))
            .map(|x| (x.name.clone(), look_of(x)));
        engine.post(Box::new(move |ctx| {
            let r = match found {
                None => Err(ToolError::NotFound(format!("no Windows Terminal profile named `{wanted}`; `profile_list` shows them"))),
                Some((name, look)) => set_look(ctx.state, &id, &look, Some(&name)),
            };
            crate::lifecycle::after_step(ctx.state, ctx.engine);
            if let Some(reply) = reply {
                reply.send(r);
            }
        }));
    })?;
    Ok(ToolOutput::message("Reading Windows Terminal's settings."))
}

/// Stamps `look` onto the session, saves, and shows it at once in a running terminal.
fn set_look(state: &mut State, id: &str, look: &Look, profile: Option<&str>) -> Reply {
    if let Some(msg) = look.validate() {
        return Err(ToolError::InvalidParams(msg));
    }
    let i = find(state, id)?;
    let Some(s) = state.app.sessions.get_mut(i) else { return Err(ToolError::NotFound("gone".into())) };
    let before = Look::from_session(s);
    look.apply_to(s);
    let after = Look::from_session(s);
    let name = s.name.clone();
    state.persist();
    push_to_terminal(state, id);
    let message = match (profile, after.is_empty()) {
        (Some(p), _) => format!("{name} now looks like the “{p}” profile."),
        (None, true) => format!("{name} follows the app's terminal settings."),
        (None, false) => format!("{name} has its own look."),
    };
    Ok(ToolOutput::new(message, json!({"session_id": id, "changed": before != after, "look": after})))
}

/// Gives a running session's terminal the stored look's colours (for OSC colour queries) and
/// default cursor. The renderer draws the rest from the snapshot's `look`.
pub(crate) fn push_to_terminal(state: &State, id: &str) {
    let (Some(rt), Some(s)) = (state.rt.sessions.get(id), state.app.sessions.iter().find(|s| s.id == id)) else { return };
    let look = Look::from_session(s);
    apply_to_terminal(&mut rt.io.lock_terminal(), &look);
    rt.io.bump();
}

/// The emulator side of a look: default palette and cursor.
pub(crate) fn apply_to_terminal(t: &mut csm_term::Terminal, look: &Look) {
    let mut palette = csm_term::Palette::default();
    if let Some(c) = look.colors.as_ref().map(LookColors::resolve) {
        let rgb = |[r, g, b]: [u8; 3]| Rgb { r, g, b };
        for (slot, c) in palette.ansi.iter_mut().zip(c.ansi) {
            if let Some(c) = c {
                *slot = rgb(c);
            }
        }
        if let Some(c) = c.foreground {
            palette.foreground = rgb(c);
        }
        if let Some(c) = c.background {
            palette.background = rgb(c);
        }
        if let Some(c) = c.cursor {
            palette.cursor = rgb(c);
        }
    }
    t.set_palette(palette);
    let shape = match look.cursor_shape {
        None | Some(CursorShape::Block) => TermCursor::Block,
        Some(CursorShape::Underline) => TermCursor::Underline,
        Some(CursorShape::Bar) => TermCursor::Beam,
    };
    t.set_default_cursor(shape, look.cursor_blink.unwrap_or(false));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_becomes_a_look() {
        let p = WtProfile {
            name: "PowerShell".into(),
            font_family: Some("'Cascadia Code', monospace".into()),
            font_size: Some(12),
            cursor_shape: Some("bar".into()),
            padding: Some("8px 8px 8px 8px".into()),
            color_scheme_json: Some(r##"{"background":"#0C0C0C","magenta":"#881798"}"##.into()),
            ..Default::default()
        };
        let l = look_of(&p);
        assert_eq!(l.font_family.as_deref(), Some("Cascadia Code"));
        assert_eq!(l.font_size, Some(12));
        assert_eq!(l.cursor_shape, Some(CursorShape::Bar));
        assert_eq!(l.padding, Some(vec![8, 8, 8, 8]));
        assert_eq!(l.colors.as_ref().and_then(|c| c.background.clone()).as_deref(), Some("#0c0c0c"));
        assert!(l.validate().is_none());
        // A size the renderer cannot draw is dropped, not stored.
        assert_eq!(look_of(&WtProfile { font_size: Some(300), ..Default::default() }).font_size, None);
    }
}

//! `state_export` / `state_import`: the `.csm` setup backup (WPF `ImportExportService`, the
//! title bar's Export / Import).
//!
//! - **Export** writes the whole app state (sessions, groups, layouts, settings) as JSON, the
//!   same shape as `state.json`, so a WPF backup imports here and back. The file is written on a
//!   worker. An MCP caller's export leaves out the API key and the MCP grants.
//! - **Import** follows the WPF app: the backup's sessions and groups **replace** ours (ours are
//!   closed, which keeps them in Recently closed), and a fixed list of settings is taken from
//!   it. Two deliberate differences, both because a backup is any file someone was handed:
//!   imported sessions arrive **asleep** (WPF restored them on the next start, running whatever
//!   command the file named), and the API key and the MCP grants are never replaced by a blank
//!   or by the file's grants.
//! - **The Open with list** in a backup is a list of programs to launch. It is sanitised
//!   (`sanitize_imported`: no batch files, each program checked at its real path on disk) and
//!   still only adopted when the caller says so (`adopt_open_with`), after showing the person
//!   the commands (`dry_run` returns them). Without it, ours is kept (the WPF default answer).
//! - Import is refused for MCP clients: it decides what runs. `dry_run` reads and changes nothing;
//!   replacing existing sessions needs `force` (the "Continue?" the UI asks).

use std::path::{Path, PathBuf};

use csm_core::models::{AppSettings, AppState, OpenWithEntry, SessionGroup, SessionStatus, ShellSession, SlashCommand};
use serde::Deserialize;
use serde_json::{Value, json};

use super::open;
use crate::engine::{Ctx, ParkedReply, Reply};
use crate::lifecycle;
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

/// A backup bigger than this is not a backup (state files are kilobytes).
const MAX_BACKUP_BYTES: u64 = 32 * 1024 * 1024;

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "state_export",
            title: "Export setup…",
            summary: "Write a backup of the whole setup (sessions, groups, layouts, settings) to a .csm file at `path` (absolute). The same JSON as state.json; the WPF app's Import reads it. An MCP client's export leaves out the API key and MCP grants.",
            toolset: Toolset::App,
            effects: Effects::WRITES_FILESYSTEM,
            input_schema: schema_of::<ExportParams>,
            run: state_export,
        },
        ToolSpec {
            id: "state_import",
            title: "Import setup…",
            summary: "Read a .csm backup and replace the sessions and groups with its own (ours are closed; Recently closed keeps the last ten; imported ones arrive asleep), taking its settings. dry_run: true only reports what it would do and lists the backup's Open with commands; replacing needs force: true; the backup's Open with list is adopted only with adopt_open_with: true. App and CLI only.",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE.union(Effects::MAY_DISCARD_DATA),
            input_schema: schema_of::<ImportParams>,
            run: state_import,
        },
    ]
}

/// An absolute path with nothing hostile in it.
fn backup_path(raw: &str) -> Result<PathBuf, ToolError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(ToolError::InvalidParams("give the backup file's path".into()));
    }
    if s.chars().any(char::is_control) {
        return Err(ToolError::InvalidParams("the path has control characters in it".into()));
    }
    let p = PathBuf::from(s);
    if !p.is_absolute() {
        return Err(ToolError::InvalidParams(format!("give an absolute path, not `{s}`")));
    }
    Ok(p)
}

fn park(ctx: &mut Ctx<'_>) -> Result<ParkedReply, ToolError> {
    ctx.park().ok_or_else(|| ToolError::Failed("this tool cannot run outside a call".into()))
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), ToolError> {
    std::thread::Builder::new().name(name.into()).spawn(f).map(|_| ()).map_err(|e| ToolError::Failed(format!("could not start a worker: {e}")))
}

// ── Export ───────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExportParams {
    /// Where to write the backup: an absolute path (`.csm` by convention). An existing file is
    /// replaced (the old one is kept beside it as `.bak`).
    path: String,
}

fn state_export(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ExportParams = parse_params(params)?;
    let path = backup_path(&p.path)?;
    let mut app = ctx.state.app.clone();
    app.ui.focused_session = ctx.state.rt.focused.clone();
    app.csm_rs_version = Some(crate::persist::STATE_FILE_VERSION);
    if ctx.caller.is_external() {
        app.settings.anthropic_api_key.clear();
        app.mcp_grants.clear();
    }
    let text = app.to_json().map_err(|e| ToolError::Failed(format!("could not write the state as JSON: {e}")))?;
    let (sessions, groups) = (app.sessions.len(), app.groups.len());
    let reply = park(ctx)?;
    spawn("csm-export", move || {
        let r = if path.is_dir() {
            Err(ToolError::InvalidParams(format!("{} is a folder; give a file name", path.display())))
        } else {
            match crate::persist::write_atomic(&path, &text) {
                Ok(()) => Ok(ToolOutput::new(
                    format!("Exported {sessions} sessions and {groups} groups to {}.", path.display()),
                    json!({"path": path.display().to_string(), "sessions": sessions, "groups": groups, "bytes": text.len()}),
                )),
                Err(e) => Err(ToolError::Failed(format!("could not write {}: {e}", path.display()))),
            }
        };
        reply.send(r);
    })?;
    Ok(ToolOutput::message("Exporting."))
}

// ── Import ───────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportParams {
    /// The backup (`.csm`, written by Export here or in the WPF app). Absolute path.
    path: String,
    /// Only read and check the file: say what an import would replace and list the backup's
    /// Open with commands. Nothing changes.
    #[serde(default)]
    dry_run: bool,
    /// Replace the current sessions and groups (closed even when busy; they stay in Recently
    /// closed). Needed whenever there are any.
    #[serde(default)]
    force: bool,
    /// Also take the backup's Open with list (programs that run on a click). Only after the
    /// person has seen it (`dry_run` lists it). Default: keep ours.
    #[serde(default)]
    adopt_open_with: bool,
}

/// What a worker read from a backup file.
struct Read {
    state: AppState,
    /// The backup's Open with list after `sanitize_imported`.
    open_with: Vec<OpenWithEntry>,
    /// Entries the sanitiser dropped.
    dropped: usize,
}

fn read_backup(path: &Path) -> Result<Read, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > MAX_BACKUP_BYTES {
        return Err(format!("{} is too big to be a CodeShellManager backup", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let state = csm_core::state::parse(&String::from_utf8_lossy(&bytes))
        .map_err(|e| format!("{} could not be read as a CodeShellManager backup: {e}", path.display()))?;
    let open_with = open::sanitize_imported(&state.settings.open_with);
    let dropped = state.settings.open_with.len().saturating_sub(open_with.len());
    Ok(Read { state, open_with, dropped })
}

fn refused_for_mcp() -> ToolError {
    ToolError::Refused("importing a setup decides what runs (commands, editors); do it in the app or with `csm state-import`".into())
}

/// Refused before any consent card: no answer would make it right for an MCP client.
pub(super) fn precheck(id: &str, caller: &crate::tool::Caller) -> Option<ToolError> {
    (id == "state_import" && caller.is_external()).then(refused_for_mcp)
}

fn state_import(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if ctx.caller.is_external() {
        return Err(refused_for_mcp());
    }
    let p: ImportParams = parse_params(params)?;
    let path = backup_path(&p.path)?;
    let reply = park(ctx)?;
    let engine = ctx.engine.clone();
    spawn("csm-import", move || {
        let read = read_backup(&path);
        engine.post(Box::new(move |ctx| {
            let r = match read {
                Err(m) => Err(ToolError::Failed(m)),
                Ok(read) => finish_import(ctx, &path, read, &p),
            };
            lifecycle::after_step(ctx.state, ctx.engine);
            reply.send(r);
        }));
    })?;
    Ok(ToolOutput::message("Reading the backup."))
}

fn finish_import(ctx: &mut Ctx<'_>, path: &Path, read: Read, p: &ImportParams) -> Reply {
    let theirs = &read.state.settings;
    let differs = theirs.open_with_seeded && !open::same_commands(&read.open_with, &ctx.state.app.settings.open_with);
    let (have_s, have_g) = (ctx.state.app.sessions.len(), ctx.state.app.groups.len());
    let (in_s, in_g) = (read.state.sessions.len(), read.state.groups.len());
    let summary = json!({
        "path": path.display().to_string(),
        "sessions": in_s,
        "groups": in_g,
        "replaces": {"sessions": have_s, "groups": have_g},
        "open_with": {
            "differs": differs,
            "entries": read.open_with.len(),
            "dropped": read.dropped,
            "review": if read.open_with.is_empty() { "(an empty list)\n".to_string() } else { open::describe_for_review(&read.open_with) },
        },
    });
    if p.dry_run {
        let msg = format!(
            "The backup has {in_s} sessions and {in_g} groups. Importing replaces your {have_s} sessions and {have_g} groups{}.",
            if differs { "; it also has its own Open with list" } else { "" }
        );
        return Ok(ToolOutput::new(msg, summary));
    }
    if (have_s > 0 || have_g > 0) && !p.force {
        return Err(ToolError::Refused(format!(
            "importing replaces your {have_s} sessions and {have_g} groups (they are closed and kept in Recently closed); pass force: true to go ahead, or dry_run: true to see what the backup holds"
        )));
    }
    let adopt = differs && p.adopt_open_with;
    let Read { state: mut theirs, open_with, .. } = read;
    // Ours go first, the way closing does it (Recently closed keeps the last ten).
    let ids: Vec<String> = ctx.state.app.sessions.iter().map(|s| s.id.clone()).collect();
    for id in &ids {
        if let Err(e) = lifecycle::close_session(ctx, id, true) {
            log::warn!("import: closing {id}: {e}");
        }
    }
    let (groups, sessions) = adopt_sessions(std::mem::take(&mut theirs.groups), std::mem::take(&mut theirs.sessions));
    let app = &mut ctx.state.app;
    app.groups = groups;
    app.sessions = sessions;
    app.group_layouts = theirs.group_layouts;
    app.last_layout = theirs.last_layout;
    app.ui = theirs.ui;
    app.ui.focused_session = None;
    adopt_settings(&mut app.settings, &theirs.settings, adopt.then_some(open_with));
    let (n_s, n_g) = (app.sessions.len(), app.groups.len());
    ctx.state.persist();
    let msg = format!(
        "Imported {n_s} sessions (asleep: wake them when you need them) and {n_g} groups{}.",
        if adopt {
            " and the backup's Open with list"
        } else if differs {
            "; your Open with list was kept"
        } else {
            ""
        }
    );
    let mut data = summary;
    data["imported"] = json!({
        "sessions": n_s,
        "groups": n_g,
        "open_with_adopted": adopt,
        "launch_commands": ctx.state.app.settings.launch_commands,
    });
    Ok(ToolOutput::new(msg, data))
}

/// The backup's groups and sessions, cleaned: blank and duplicate ids dropped, sessions asleep,
/// a session whose group is not in the backup is ungrouped, group order renumbered.
fn adopt_sessions(groups: Vec<SessionGroup>, sessions: Vec<ShellSession>) -> (Vec<SessionGroup>, Vec<ShellSession>) {
    let mut gs: Vec<SessionGroup> = Vec::new();
    for g in groups {
        if !g.id.trim().is_empty() && !gs.iter().any(|x| x.id == g.id) {
            gs.push(g);
        }
    }
    for (i, g) in gs.iter_mut().enumerate() {
        g.sort_order = i32::try_from(i).unwrap_or(i32::MAX);
    }
    let mut ss: Vec<ShellSession> = Vec::new();
    for mut s in sessions {
        if s.id.trim().is_empty() || ss.iter().any(|x| x.id == s.id) {
            continue;
        }
        s.is_dormant = true;
        s.status = SessionStatus::Idle;
        if !s.group_id.is_empty() && !gs.iter().any(|g| g.id == s.group_id) {
            s.group_id.clear();
        }
        ss.push(s);
    }
    (gs, ss)
}

/// The settings an import takes from a backup (the WPF `Import_Click` list, plus the Rust app's
/// own terminal and slash-command settings). Kept from ours: window and machine settings, the
/// MCP offer, the API key when the backup has none, and the Open with list unless `open_with`
/// carries the one the person agreed to.
fn adopt_settings(ours: &mut AppSettings, theirs: &AppSettings, open_with: Option<Vec<OpenWithEntry>>) {
    ours.auto_restore_sessions = theirs.auto_restore_sessions;
    ours.auto_resume_claude = theirs.auto_resume_claude;
    ours.show_toast_notifications = theirs.show_toast_notifications;
    ours.show_notification_sound = theirs.show_notification_sound;
    if !theirs.anthropic_api_key.trim().is_empty() {
        ours.anthropic_api_key = theirs.anthropic_api_key.clone();
    }
    if !theirs.default_command.trim().is_empty() && !theirs.default_command.chars().any(char::is_control) {
        ours.default_command = theirs.default_command.trim().to_owned();
    }
    if !theirs.default_working_folder.chars().any(char::is_control) {
        ours.default_working_folder = theirs.default_working_folder.trim().to_owned();
    }
    ours.show_git_branch = theirs.show_git_branch;
    ours.search_collapse_after_navigate = theirs.search_collapse_after_navigate;
    ours.max_search_results = theirs.max_search_results;
    ours.show_terminal_status_dot = theirs.show_terminal_status_dot;
    ours.launch_commands =
        theirs.launch_commands.iter().map(|c| c.trim().to_owned()).filter(|c| !c.is_empty() && !c.chars().any(char::is_control)).fold(
            Vec::new(),
            |mut v, c| {
                if !v.contains(&c) {
                    v.push(c);
                }
                v
            },
        );
    if let Some(list) = open_with {
        ours.open_with = list;
        ours.open_with_seeded = true;
    }
    ours.terminal_font_family = theirs.terminal_font_family.clone();
    ours.terminal_font_size = theirs.terminal_font_size;
    ours.terminal_font_ligatures = theirs.terminal_font_ligatures;
    ours.terminal_font_weight = theirs.terminal_font_weight.clone();
    ours.terminal_letter_spacing = theirs.terminal_letter_spacing;
    ours.terminal_line_height = theirs.terminal_line_height;
    ours.terminal_font_points = theirs.terminal_font_points;
    ours.terminal_padding = theirs.terminal_padding;
    ours.claude_commands = SlashCommand::clean_list(&theirs.claude_commands);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, exe: &str) -> OpenWithEntry {
        OpenWithEntry {
            id: label.into(),
            label: label.into(),
            executable: exe.into(),
            local_args: Some("{folder}".into()),
            ..OpenWithEntry::default()
        }
    }

    #[test]
    fn settings_follow_the_wpf_list_and_keep_ours_where_it_matters() {
        let mut ours = AppSettings { anthropic_api_key: "sk-ours".into(), offer_mcp_to_claude: true, ..AppSettings::default() };
        ours.open_with = vec![entry("Code", r"C:\Code.exe")];
        ours.open_with_seeded = true;
        let theirs = AppSettings {
            auto_restore_sessions: false,
            default_command: "codex".into(),
            launch_commands: vec!["codex".into(), "bad\ncmd".into(), " codex ".into(), "pwsh".into()],
            claude_commands: vec![SlashCommand::new("/review", "Review"), SlashCommand::new("/x\r\ny", "two lines")],
            terminal_padding: 3,
            offer_mcp_to_claude: false,
            open_with: vec![entry("Evil", r"C:\evil.exe")],
            open_with_seeded: true,
            ..AppSettings::default()
        };
        let mut a = ours.clone();
        adopt_settings(&mut a, &theirs, None);
        assert!(!a.auto_restore_sessions);
        assert_eq!(a.default_command, "codex");
        assert_eq!(a.launch_commands, ["codex", "pwsh"], "control characters and duplicates dropped");
        assert_eq!(a.claude_commands, vec![SlashCommand::new("/review", "Review")]);
        assert_eq!(a.terminal_padding, 3);
        assert_eq!(a.anthropic_api_key, "sk-ours", "a blank key in the backup never erases ours");
        assert!(a.offer_mcp_to_claude, "the MCP offer changes the claude command line: not imported");
        assert_eq!(a.open_with, ours.open_with, "the Open with list is kept unless the person agreed");
        let mut b = ours.clone();
        adopt_settings(&mut b, &theirs, Some(vec![entry("Agreed", r"C:\Agreed.exe")]));
        assert_eq!(b.open_with[0].label, "Agreed");
        assert!(b.open_with_seeded);
    }

    #[test]
    fn imported_sessions_arrive_asleep_and_ungrouped_when_their_group_is_missing() {
        let groups = vec![
            SessionGroup { id: "g1".into(), name: "One".into(), ..SessionGroup::default() },
            SessionGroup { id: "g1".into(), name: "Dup".into(), ..SessionGroup::default() },
            SessionGroup { id: " ".into(), name: "Blank".into(), ..SessionGroup::default() },
        ];
        let mk = |id: &str, g: &str| ShellSession { id: id.into(), group_id: g.into(), is_dormant: false, ..ShellSession::default() };
        let (g, s) = adopt_sessions(groups, vec![mk("a", "g1"), mk("b", "gone"), mk("a", "g1"), mk("", "")]);
        assert_eq!(g.len(), 1);
        assert_eq!(s.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert!(s.iter().all(|x| x.is_dormant));
        assert_eq!(s[0].group_id, "g1");
        assert_eq!(s[1].group_id, "", "its group isn't in the backup");
    }

    #[test]
    fn paths_must_be_absolute_and_plain() {
        assert!(backup_path("").is_err());
        assert!(backup_path("relative.csm").is_err());
        assert!(backup_path("C:\\x\n.csm").is_err());
        let abs = if cfg!(windows) { r"C:\backups\a.csm" } else { "/backups/a.csm" };
        assert_eq!(backup_path(abs).unwrap(), PathBuf::from(abs));
    }
}

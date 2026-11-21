//! Add-ons (docs/addons.md): discovery, the enable state, and the contributions the rest of the
//! engine reads (slash presets, profiles, run templates, palette commands).
//!
//! - Discovery runs on a worker (`csm-services::addons::scan` and `csm_core::addons::parse`); the
//!   result comes back as a job that installs the catalogue on the engine thread.
//! - An add-on is off until the person enables it. Enabling stores the digest of the `addon.toml`
//!   they approved in `AppSettings::addon_approvals`; a file that changes after that is off again
//!   until it is approved again. Disabling forgets the approval.
//! - Nothing is read under `--clean` or an isolated run (no state folder: `save_path` is `None`).
//! - Contributions are namespaced: a profile is `addon-id.name`, a run template is `addon-id.name`,
//!   a palette command is `addon-id.id`, and a slash preset is only added when the person does not
//!   already have that command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use csm_core::addons::{self as core, AddonKind, ParsedAddon, ProfileSpec, Problem, ProfileStatus, StatusRule};
use csm_core::models::{RunCommandItem, RunMode, SlashCommand};
use csm_services::addons as disk;
use serde::Serialize;
use serde_json::json;

use crate::engine::{Ctx, ParkedReply};
use crate::registry::Registry;
use crate::tool::{Toolset, ToolError, ToolOutput};

/// Where an add-on stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonState {
    /// Enabled, and its contributions are in use.
    Loaded,
    /// Found and valid, but not enabled (or changed since it was enabled).
    Disabled,
    /// Could not be read or parsed.
    Error,
}

/// One add-on as the catalogue holds it.
pub(crate) struct Entry {
    pub folder: String,
    pub path: PathBuf,
    pub parsed: Option<Arc<ParsedAddon>>,
    pub digest: Option<String>,
    pub state: AddonState,
    /// Why it is disabled or failed, in plain words.
    pub message: Option<String>,
    /// Entries the install check dropped (a palette command naming an unknown tool).
    pub problems: Vec<Problem>,
}

/// The catalogue: runtime state, never persisted.
#[derive(Default)]
pub(crate) struct Catalogue {
    pub entries: Vec<Entry>,
    /// The add-ons folder, `None` when there is no state folder (clean or isolated runs).
    pub root: Option<PathBuf>,
    /// A scan is on its way.
    pub scanning: bool,
    /// Replies of `addon_reload` calls waiting for the scan.
    pub waiters: Vec<ParkedReply>,
    /// Why the folder could not be read at all.
    pub scan_error: Option<String>,
}

/// What the UI and `addon_list` show for one add-on.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AddonSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub kind: &'static str,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub state: AddonState,
    /// Why it is disabled or failed.
    pub message: Option<String>,
    pub folder: String,
    pub path: String,
    /// `addon-id.name` ids.
    pub profiles: Vec<String>,
    pub run_templates: Vec<RunTemplateSummary>,
    pub slash: Vec<SlashCommand>,
    pub commands: Vec<CommandSummary>,
    /// For a process add-on: the effects it declares (shown in the consent dialog).
    pub process_effects: Vec<String>,
    /// Entries that were skipped, one line each.
    pub problems: Vec<String>,
}

/// A run template as the Runs panel offers it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunTemplateSummary {
    /// `addon-id.name`.
    pub id: String,
    pub name: String,
    pub command_line: String,
    pub cwd: Option<String>,
}

/// A palette command.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CommandSummary {
    /// `addon-id.id`.
    pub id: String,
    pub title: String,
    pub tool: String,
    pub params: serde_json::Value,
}

/// One folder, parsed on the worker and installed on the engine thread.
pub(crate) struct Scanned {
    pub folder: String,
    pub path: PathBuf,
    /// The parsed add-on and the digest of its text, or why it could not be read.
    pub parsed: Result<(ParsedAddon, String), String>,
}

/// The add-ons folder for a state file: beside it, as `addons/`. `None` when nothing is saved.
pub(crate) fn root_for(save_path: Option<&Path>, clean: bool) -> Option<PathBuf> {
    if clean {
        return None;
    }
    save_path.and_then(Path::parent).map(disk::addons_dir)
}

/// Starts a scan on a worker. The catalogue is installed by a job posted back to the engine.
pub(crate) fn reload(ctx: &mut Ctx<'_>) {
    let root = root_for(ctx.state.options.save_path.as_deref(), ctx.state.options.clean);
    ctx.state.rt.addons.root.clone_from(&root);
    ctx.state.rt.addons.scanning = true;
    ctx.state.rt.dirty = true;
    let Some(root) = root else {
        install(ctx, Vec::new(), None);
        return;
    };
    let engine = ctx.engine.clone();
    let spawned = std::thread::Builder::new().name("csm-addons-scan".into()).spawn(move || {
        let (scanned, err) = match disk::scan(&root) {
            Ok(found) => (found.into_iter().map(parse_found).collect::<Vec<_>>(), None),
            Err(e) => (Vec::new(), Some(e)),
        };
        engine.post(Box::new(move |ctx: &mut Ctx<'_>| install(ctx, scanned, err)));
    });
    if spawned.is_err() {
        ctx.state.rt.addons.scanning = false;
        ctx.state.rt.addons.scan_error = Some("could not start the add-on scan thread".into());
        ctx.state.rt.dirty = true;
    }
}

fn parse_found(f: disk::Found) -> Scanned {
    let parsed = match f.text {
        Err(e) => Err(e),
        Ok(text) => {
            let digest = disk::digest(&text);
            core::parse(&text, &f.folder).map(|p| (p, digest)).map_err(|e| e.to_string())
        }
    };
    Scanned { folder: f.folder, path: f.path, parsed }
}

/// Engine thread: replaces the catalogue, derives each add-on's state from the approvals, and
/// answers the waiting `addon_reload` calls.
fn install(ctx: &mut Ctx<'_>, scanned: Vec<Scanned>, scan_error: Option<String>) {
    let approvals = ctx.state.app.settings.addon_approvals.clone();
    let registry = ctx.registry;
    let entries: Vec<Entry> = scanned.into_iter().map(|s| build_entry(s, registry)).collect();
    let cat = &mut ctx.state.rt.addons;
    cat.entries = entries;
    cat.scanning = false;
    cat.scan_error = scan_error;
    for e in &mut cat.entries {
        refresh_state(e, &approvals);
    }
    let count = cat.entries.len();
    for w in std::mem::take(&mut cat.waiters) {
        w.send(Ok(ToolOutput::new(format!("Reloaded {count} add-on folder(s)."), json!({"count": count}))));
    }
    ctx.state.rt.dirty = true;
}

fn build_entry(s: Scanned, registry: &Registry) -> Entry {
    match s.parsed {
        Err(e) => Entry { folder: s.folder, path: s.path, parsed: None, digest: None, state: AddonState::Error, message: Some(e), problems: Vec::new() },
        Ok((mut p, digest)) => {
            let mut problems = Vec::new();
            if p.meta.kind == AddonKind::Process {
                if !(p.profiles.is_empty() && p.run_templates.is_empty() && p.slash.is_empty() && p.commands.is_empty()) {
                    problems.push(Problem { scope: "process add-on".into(), message: "its profiles, run commands, slash presets and palette commands are ignored; process add-ons run nothing yet".into() });
                }
                p.profiles.clear();
                p.run_templates.clear();
                p.slash.clear();
                p.commands.clear();
            }
            // A palette command must name a real tool, and never an add-on tool.
            p.commands.retain(|c| {
                let known = registry.get(&c.tool).is_some_and(|t| t.toolset != Toolset::Addons);
                if !known {
                    problems.push(Problem { scope: format!("command `{}`", c.id), message: format!("`{}` is not a tool this app has; ignored", c.tool) });
                }
                known
            });
            Entry { folder: s.folder, path: s.path, parsed: Some(Arc::new(p)), digest: Some(digest), state: AddonState::Disabled, message: None, problems }
        }
    }
}

/// The state of one entry from the approvals. Parse failures stay errors.
fn refresh_state(e: &mut Entry, approvals: &BTreeMap<String, String>) {
    let Some(p) = &e.parsed else {
        e.state = AddonState::Error;
        return;
    };
    match (approvals.get(&p.meta.id), &e.digest) {
        (Some(approved), Some(now)) if approved == now => {
            e.state = AddonState::Loaded;
            e.message = None;
        }
        (Some(_), _) => {
            e.state = AddonState::Disabled;
            e.message = Some("the file changed since you enabled it; review it and enable it again".into());
        }
        (None, _) => {
            e.state = AddonState::Disabled;
            e.message = Some("not enabled yet".into());
        }
    }
}

/// Re-derives every entry's state after the approvals changed (no disk access).
pub(crate) fn refresh_all(ctx: &mut Ctx<'_>) {
    let approvals = ctx.state.app.settings.addon_approvals.clone();
    for e in &mut ctx.state.rt.addons.entries {
        refresh_state(e, &approvals);
    }
    ctx.state.rt.dirty = true;
}

/// The parsed contents of an enabled add-on (a cheap clone).
pub(crate) fn enabled_addon(cat: &Catalogue, id: &str) -> Result<Arc<ParsedAddon>, ToolError> {
    let e = entry(cat, id).ok_or_else(|| ToolError::NotFound(format!("no add-on `{id}`; `addon_list` shows them")))?;
    if e.state != AddonState::Loaded {
        return Err(ToolError::Refused(format!("the add-on `{id}` is not enabled")));
    }
    e.parsed.clone().ok_or_else(|| ToolError::NotFound(format!("the add-on `{id}` did not load")))
}

/// The entry for this folder (= add-on id).
pub(crate) fn entry<'a>(cat: &'a Catalogue, id: &str) -> Option<&'a Entry> {
    cat.entries.iter().find(|e| e.folder == id)
}

/// Parsed contents of enabled add-ons.
pub(crate) fn loaded(cat: &Catalogue) -> impl Iterator<Item = &ParsedAddon> {
    cat.entries.iter().filter(|e| e.state == AddonState::Loaded).filter_map(|e| e.parsed.as_deref())
}

/// The enabled profile named `addon-id.name`.
pub(crate) fn profile<'a>(cat: &'a Catalogue, id: &str) -> Result<(&'a ParsedAddon, &'a ProfileSpec), ToolError> {
    let (addon, name) = id.split_once('.').ok_or_else(|| ToolError::InvalidParams(format!("`{id}` is not `addon-id.name`; `addon_list` shows the ids")))?;
    let e = entry(cat, addon).ok_or_else(|| ToolError::NotFound(format!("no add-on `{addon}`; `addon_list` shows them")))?;
    if e.state != AddonState::Loaded {
        return Err(ToolError::Refused(format!("the add-on `{addon}` is not enabled")));
    }
    let p = e.parsed.as_deref().ok_or_else(|| ToolError::NotFound(format!("the add-on `{addon}` did not load")))?;
    let spec = p.profiles.iter().find(|x| x.name == name).ok_or_else(|| ToolError::NotFound(format!("the add-on `{addon}` has no profile `{name}`")))?;
    Ok((p, spec))
}

/// The slash menu's list: the person's own presets first, then the enabled add-ons' presets the
/// person does not already have. Each add-on description says where it came from.
pub(crate) fn slash_presets(cat: &Catalogue, own: &[SlashCommand]) -> Vec<SlashCommand> {
    let mut out = SlashCommand::clean_list(own);
    for p in loaded(cat) {
        for s in &p.slash {
            if out.iter().any(|c| c.command == s.command) || out.len() >= SlashCommand::MAX * 2 {
                continue;
            }
            let desc = if s.description.is_empty() { p.meta.name.clone() } else { format!("{} ({})", s.description, p.meta.name) };
            out.push(SlashCommand { command: s.command.clone(), description: desc });
        }
    }
    out
}

/// The status rules of the enabled profile a session was launched from, if any.
pub(crate) fn status_rules(cat: &Catalogue, profile_id: &str) -> Option<Vec<StatusRule>> {
    if profile_id.is_empty() {
        return None;
    }
    profile(cat, profile_id).ok().map(|(_, spec)| spec.status.clone()).filter(|r| !r.is_empty())
}

/// The screen's status for a rule set, in the app's terms.
pub(crate) fn screen_status(rules: &[StatusRule], screen: &str) -> Option<ProfileStatus> {
    core::match_status(rules, screen)
}

/// Everything the UI and `addon_list` show, in folder order.
pub(crate) fn summaries(cat: &Catalogue) -> Vec<AddonSummary> {
    cat.entries.iter().map(summary).collect()
}

fn summary(e: &Entry) -> AddonSummary {
    let p = e.parsed.as_deref();
    let meta = p.map(|p| &p.meta);
    let id = meta.map_or_else(|| e.folder.clone(), |m| m.id.clone());
    let mut problems: Vec<String> = e.problems.iter().map(|x| format!("{}: {}", x.scope, x.message)).collect();
    if let Some(p) = p {
        problems.extend(p.problems.iter().map(|x| format!("{}: {}", x.scope, x.message)));
    }
    AddonSummary {
        id: id.clone(),
        name: meta.map_or_else(|| e.folder.clone(), |m| m.name.clone()),
        version: meta.map_or_else(String::new, |m| m.version.to_string()),
        description: meta.map_or_else(String::new, |m| m.description.clone()),
        kind: meta.map_or("unknown", |m| m.kind.id()),
        author: meta.and_then(|m| m.author.clone()),
        homepage: meta.and_then(|m| m.homepage.clone()),
        state: e.state,
        message: e.message.clone(),
        folder: e.folder.clone(),
        path: e.path.display().to_string(),
        profiles: p.map_or_else(Vec::new, |p| p.profiles.iter().map(|x| format!("{id}.{}", x.name)).collect()),
        run_templates: p.map_or_else(Vec::new, |p| {
            p.run_templates
                .iter()
                .map(|r| RunTemplateSummary { id: format!("{id}.{}", r.name), name: r.name.clone(), command_line: r.argv.join(" "), cwd: r.cwd.clone() })
                .collect()
        }),
        slash: p.map_or_else(Vec::new, |p| p.slash.iter().map(|s| SlashCommand { command: s.command.clone(), description: s.description.clone() }).collect()),
        commands: p.map_or_else(Vec::new, |p| {
            p.commands.iter().map(|c| CommandSummary { id: format!("{id}.{}", c.id), title: c.title.clone(), tool: c.tool.clone(), params: c.params.clone() }).collect()
        }),
        process_effects: p.and_then(|p| p.process.as_ref()).map_or_else(Vec::new, |s| s.effects.clone()),
        problems,
    }
}

/// The run command a template becomes on a session: argv joined by single spaces. Every token is
/// plain (see `csm_core::addons::plain_token`), so the joined line has no shell meaning. A
/// template with a `cwd` other than `{folder}` is refused: run commands cannot set a folder yet.
pub(crate) fn run_item_for(p: &ParsedAddon, template: &str, id: String, is_first: bool) -> Result<RunCommandItem, String> {
    let r = p.run_templates.iter().find(|r| r.name == template).ok_or_else(|| format!("the add-on has no run template `{template}`"))?;
    if r.cwd.as_deref().is_some_and(|c| c != "{folder}") {
        return Err("this run template sets a working folder, which run commands cannot take yet".into());
    }
    Ok(RunCommandItem { id, label: r.name.clone(), command_line: r.argv.join(" "), is_default: is_first, mode: RunMode::Process, post_run_url: None })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn root_is_beside_the_state_file_and_absent_when_clean() {
        let p = Path::new("/x/y/state.json");
        assert_eq!(root_for(Some(p), false), Some(PathBuf::from("/x/y").join("addons")));
        assert_eq!(root_for(Some(p), true), None);
        assert_eq!(root_for(None, false), None);
    }
}

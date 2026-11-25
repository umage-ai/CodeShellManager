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
use crate::plugins::{self, PluginRuntime};
use crate::registry::Registry;
use crate::tool::{Effects, Toolset, ToolError, ToolOutput};

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

/// A theme's font file, read and checked on the scan worker. Cheap to clone; equal only when it is
/// the same read (so a snapshot compares pointers, not megabytes).
#[derive(Clone)]
pub struct FontBytes(pub Arc<Vec<u8>>);

impl std::fmt::Debug for FontBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FontBytes({} bytes)", self.0.len())
    }
}

impl PartialEq for FontBytes {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// The font files an add-on's themes name that loaded: add-on-relative path, bytes.
pub type ThemeFonts = Vec<(String, FontBytes)>;

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
    /// The tools `[process] calls` lists that passed the install check, with their effects.
    pub standing: Vec<StandingCall>,
    /// The font files its themes name that loaded (read on the scan worker).
    pub fonts: ThemeFonts,
}

/// One tool of `[process] calls`, as the enable dialog and `addon_list` show it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StandingCall {
    pub tool: String,
    pub title: String,
    /// The tool's effects in words (`input`, `files`, `content`, ... or `read`).
    pub effects: String,
    /// Whether the tool would ask an MCP caller, so whether this list removes a card.
    pub asks: bool,
}

/// The catalogue: runtime state, never persisted.
#[derive(Default)]
pub(crate) struct Catalogue {
    pub entries: Vec<Entry>,
    /// The add-ons folder, `None` when there is no state folder (clean or isolated runs).
    pub root: Option<PathBuf>,
    /// A scan is on its way.
    pub scanning: bool,
    /// The number of the latest scan started. Scans run on workers and may finish out of order; only
    /// the result of the latest is installed, so an older folder listing never replaces a newer one.
    pub scan_seq: u64,
    /// Replies of `addon_reload` calls waiting for the scan.
    pub waiters: Vec<ParkedReply>,
    /// Why the folder could not be read at all.
    pub scan_error: Option<String>,
    /// The runtime state of each enabled process add-on, by id (`plugins.rs`).
    pub runtime: BTreeMap<String, PluginRuntime>,
    /// What each running process add-on offers under "Remote…" (`csm/remotes_changed`).
    pub remotes: BTreeMap<String, Vec<csm_core::addons::Remote>>,
    /// The manifests the theme watcher polls (enabled add-ons with themes), shared with its
    /// thread; `None` until the first one is enabled.
    pub watch: Option<Arc<std::sync::Mutex<Vec<PathBuf>>>>,
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
    /// The SHA-256 of the `addon.toml` the entries above come from. Enabling sends it back, so
    /// the engine enables exactly what the consent dialog listed (`addon_enable`).
    pub digest: Option<String>,
    pub profiles: Vec<ProfileSummary>,
    pub run_templates: Vec<RunTemplateSummary>,
    pub slash: Vec<SlashCommand>,
    pub commands: Vec<CommandSummary>,
    /// For a process add-on: the effects it declares (shown in the consent dialog).
    pub process_effects: Vec<String>,
    /// For a process add-on: the program and its arguments, joined with spaces (every token plain).
    pub process_command_line: Option<String>,
    /// For a process add-on: the core tools it may call without a consent card once enabled
    /// (`[process] calls`). Approved with the rest of the manifest, and gone if the file changes.
    pub process_calls: Vec<StandingCall>,
    /// For a process add-on: its `[[new_session]]` forms (the New session dialog offers them while it runs).
    pub new_sessions: Vec<NewSessionSummary>,
    /// For a process add-on that is enabled: whether it is running, its last error, its tools.
    pub process: Option<PluginRuntime>,
    /// Entries that were skipped, one line each.
    /// `[[theme]]` palettes, offered in Settings › Appearance while the add-on is enabled (ids
    /// there are `addon-id.theme-id`).
    pub themes: Vec<csm_core::addons::ThemeSpec>,
    /// The font files those themes name that loaded (`[theme.style]`); not part of `addon_list`.
    #[serde(skip)]
    pub theme_fonts: ThemeFonts,
    pub problems: Vec<String>,
}

/// A New session form as the consent dialog and `addon_list` show it: its title and the tools it runs
/// (`choices_tool` for the picker, `tool` on submit).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NewSessionSummary {
    /// The form's id within the add-on.
    pub id: String,
    pub title: String,
    pub submit: String,
    pub choices_tool: String,
    pub tool: String,
}

/// A launch profile as the consent dialog and `addon_list` show it: the exact command it runs.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProfileSummary {
    /// `addon-id.name`.
    pub id: String,
    pub name: String,
    /// The program and its arguments, joined with spaces (every token is plain).
    pub command_line: String,
    /// `none`, `re-run` or `template` (`ResumeRule`).
    pub resume: &'static str,
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
    /// Each font file the themes name: loaded, or why not (reported as a problem).
    pub fonts: Vec<(String, Result<FontBytes, String>)>,
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
    ctx.state.rt.addons.scan_seq += 1;
    let seq = ctx.state.rt.addons.scan_seq;
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
        engine.post(Box::new(move |ctx: &mut Ctx<'_>| install_scan(ctx, seq, scanned, err)));
    });
    if spawned.is_err() {
        // Install an empty catalogue now, so the waiting `addon_reload` calls are answered too.
        install(ctx, Vec::new(), Some("could not start the add-on scan thread".into()));
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
    let fonts = match &parsed {
        Ok((p, _)) if p.meta.kind != AddonKind::Process => read_fonts(&f.path, &p.themes),
        _ => Vec::new(),
    };
    Scanned { folder: f.folder, path: f.path, parsed, fonts }
}

/// Reads each distinct font file the themes name (on the scan worker, never the engine or UI thread).
fn read_fonts(dir: &Path, themes: &[csm_core::addons::ThemeSpec]) -> Vec<(String, Result<FontBytes, String>)> {
    let mut seen: Vec<&str> = Vec::new();
    for t in themes {
        for file in t.style.font_files() {
            if !seen.contains(&file) {
                seen.push(file);
            }
        }
    }
    seen.into_iter().map(|rel| (rel.to_owned(), csm_services::addons::read_theme_font(dir, rel).map(|b| FontBytes(Arc::new(b))))).collect()
}

/// Installs the result of scan `seq` unless a newer scan was started meanwhile. The newer scan
/// answers the waiting `addon_reload` calls (each began before it), so a stale result touches nothing.
fn install_scan(ctx: &mut Ctx<'_>, seq: u64, scanned: Vec<Scanned>, scan_error: Option<String>) {
    if ctx.state.rt.addons.scan_seq != seq {
        return;
    }
    install(ctx, scanned, scan_error);
}

/// Engine thread: replaces the catalogue, derives each add-on's state from the approvals, and
/// answers the waiting `addon_reload` calls.
fn install(ctx: &mut Ctx<'_>, scanned: Vec<Scanned>, scan_error: Option<String>) {
    let registry = ctx.registry.clone();
    let entries: Vec<Entry> = scanned.into_iter().map(|s| build_entry(s, &registry)).collect();
    if carry_theme_edits(&ctx.state.rt.addons, &entries, &mut ctx.state.app.settings.addon_approvals) {
        ctx.state.persist();
    }
    let approvals = ctx.state.app.settings.addon_approvals.clone();
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
    watch_themes(ctx);
    plugins::sync(ctx);
    // Sessions from add-on profiles that were waiting for this scan can restore now.
    crate::lifecycle::restore_waiting_addons(ctx);
}

/// The tools a pack's palette command may name (SEC-21), besides read-only ones. A command
/// runs as the person, with no consent card, so it gets only tools that open, show or start something the pack itself
/// declared: never `settings_set`, `open_with_set`, `state_import` or the like. Everything an MCP
/// client is refused stays out.
pub(crate) const PACK_COMMAND_TOOLS: [&str; 14] = [
    "session_new",
    "session_focus",
    "session_wake",
    "session_recent",
    "layout_set",
    "theme_set",
    "view_set",
    "window_show",
    "inspector_show",
    "inbox_next",
    "pane_focus_next",
    "pane_focus_prev",
    "rail_tab_next",
    "rail_tab_prev",
];

fn build_entry(s: Scanned, registry: &Registry) -> Entry {
    match s.parsed {
        Err(e) => Entry { folder: s.folder, path: s.path, parsed: None, digest: None, state: AddonState::Error, message: Some(e), problems: Vec::new(), standing: Vec::new(), fonts: Vec::new() },
        Ok((mut p, digest)) => {
            let mut problems = Vec::new();
            if p.meta.kind == AddonKind::Process {
                if !(p.profiles.is_empty() && p.run_templates.is_empty() && p.slash.is_empty() && p.commands.is_empty() && p.themes.is_empty()) {
                    problems.push(Problem { scope: "process add-on".into(), message: "its profiles, run commands, slash presets, palette commands and themes are ignored; a process add-on contributes its tools only".into() });
                }
                p.profiles.clear();
                p.run_templates.clear();
                p.slash.clear();
                p.commands.clear();
                p.themes.clear();
            }
            // A palette command must name a real tool, never an add-on tool, and never one that
            // writes into a session or turns on another extension: a pack's fixed text must not
            // reach a PTY through one click, and that path is not reviewed here.
            p.commands.retain(|c| {
                let verdict = match registry.get(&c.tool) {
                    None => Some(format!("`{}` is not a tool this app has; ignored", c.tool)),
                    Some(t) if t.toolset == Toolset::Addons => Some("add-on tools cannot be palette commands; ignored".to_owned()),
                    Some(t) if t.effects.intersects(Effects::SENDS_INPUT | Effects::ENABLES_EXTENSION) => {
                        Some(format!("`{}` writes into sessions or enables extensions, which a palette command may not do; ignored", c.tool))
                    }
                    Some(t) if !(PACK_COMMAND_TOOLS.contains(&t.id) || t.effects.read_only_hint()) => Some(format!("`{}` is not one of the tools a palette command may call (it could change settings or what runs); ignored", c.tool)),
                    Some(_) => None,
                };
                match verdict {
                    Some(message) => {
                        problems.push(Problem { scope: format!("command `{}`", c.id), message });
                        false
                    }
                    None => true,
                }
            });
            // `[process] calls`: only real core tools that an add-on may be given. Anything else
            // is dropped here and reported, so the list the person approves is the list in force.
            let mut standing = Vec::new();
            if let Some(proc) = p.process.as_mut() {
                proc.calls.retain(|name| {
                    let verdict = match registry.get(name) {
                        None => Some(format!("`{name}` is not a tool this app has; ignored")),
                        Some(t) if t.toolset == Toolset::Addons => Some(format!("`{name}` is an add-on tool; ignored")),
                        Some(t) if t.effects.contains(Effects::ENABLES_EXTENSION) => Some(format!("`{name}` turns on extensions, which only the person may do; ignored")),
                        Some(_) => None,
                    };
                    match verdict {
                        Some(message) => {
                            problems.push(Problem { scope: "[process] calls".into(), message });
                            false
                        }
                        None => true,
                    }
                });
                for name in &proc.calls {
                    if let Some(t) = registry.get(name) {
                        standing.push(StandingCall { tool: name.clone(), title: t.title.to_owned(), effects: effects_words(t.effects), asks: t.effects.needs_consent() });
                    }
                }
            }
            // A font that did not load is reported, and the theme draws with the default font.
            let mut fonts = Vec::new();
            for (rel, loaded) in s.fonts {
                match loaded {
                    Ok(bytes) => fonts.push((rel, bytes)),
                    Err(message) => {
                        let scope = p.themes.iter().filter(|t| t.style.font_files().contains(&rel.as_str())).map(|t| format!("theme `{}`", t.id)).collect::<Vec<_>>().join(", ");
                        problems.push(Problem { scope, message: format!("font {message}; the default font is used") });
                    }
                }
            }
            Entry { folder: s.folder, path: s.path, parsed: Some(Arc::new(p)), digest: Some(digest), state: AddonState::Disabled, message: None, problems, standing, fonts }
        }
    }
}

/// A tool's effects as short words for the consent dialog (`read` when it only reads).
pub(crate) fn effects_words(e: Effects) -> String {
    const WORDS: [(Effects, &str); 9] = [
        (Effects::CHANGES_UI_STATE, "changes the view"),
        (Effects::MUTATES_STATE, "changes app state"),
        (Effects::LAUNCHES_PROCESS, "starts programs"),
        (Effects::SENDS_INPUT, "types into sessions"),
        (Effects::MAY_DISCARD_DATA, "may discard work"),
        (Effects::WRITES_FILESYSTEM, "writes files"),
        (Effects::ACCESSES_NETWORK, "uses the network"),
        (Effects::READS_SESSION_CONTENT, "reads session content"),
        (Effects::STORES_SECRETS, "keeps secrets"),
    ];
    let words: Vec<&str> = WORDS.iter().filter(|(f, _)| e.contains(*f)).map(|(_, w)| *w).collect();
    if words.is_empty() { "read only".to_owned() } else { words.join(", ") }
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
    watch_themes(ctx);
    plugins::sync(ctx);
}

/// An enabled add-on whose file changed only in its `[[theme]]` tables stays enabled: themes are
/// colours, nothing that runs or types, so the person's approval moves to the new digest (live
/// reload while a theme is being written). Any other change still turns it off until it is
/// approved again. Returns whether an approval moved.
fn carry_theme_edits(old: &Catalogue, new: &[Entry], approvals: &mut BTreeMap<String, String>) -> bool {
    let without_themes = |p: &ParsedAddon| ParsedAddon { themes: Vec::new(), problems: Vec::new(), ..p.clone() };
    let mut moved = false;
    for e in new {
        let (Some(p), Some(digest)) = (e.parsed.as_deref(), e.digest.as_ref()) else { continue };
        let Some(approved) = approvals.get(&p.meta.id) else { continue };
        if approved == digest {
            continue;
        }
        let was = entry(old, &e.folder).filter(|o| o.state == AddonState::Loaded && o.digest.as_ref() == Some(approved));
        let Some(before) = was.and_then(|o| o.parsed.as_deref()) else { continue };
        if (!before.themes.is_empty() || !p.themes.is_empty()) && without_themes(before) == without_themes(p) {
            approvals.insert(p.meta.id.clone(), digest.clone());
            moved = true;
        }
    }
    moved
}

/// How often the theme watcher looks at the manifests of enabled add-ons with themes.
const THEME_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// Keeps the theme watcher's list current: the `addon.toml` of every enabled add-on with
/// themes. The watcher is a worker that compares each file's modified time and size about once
/// a second and posts a reload when one changes (no file I/O on the engine thread). It starts
/// with the first such add-on and ends when the engine's state is dropped.
fn watch_themes(ctx: &mut Ctx<'_>) {
    let cat = &mut ctx.state.rt.addons;
    let list: Vec<PathBuf> = cat
        .entries
        .iter()
        .filter(|e| e.state == AddonState::Loaded && e.parsed.as_ref().is_some_and(|p| !p.themes.is_empty()))
        .flat_map(|e| {
            let fonts = e.parsed.iter().flat_map(|p| p.themes.iter()).flat_map(|t| t.style.font_files()).map(|f| e.path.join(f)).collect::<Vec<_>>();
            std::iter::once(e.path.join(disk::MANIFEST_FILE)).chain(fonts)
        })
        .collect();
    if let Some(shared) = &cat.watch {
        *shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = list;
        return;
    }
    if list.is_empty() {
        return;
    }
    let shared = Arc::new(std::sync::Mutex::new(list));
    let mine = shared.clone();
    let engine = ctx.engine.clone();
    let spawned = std::thread::Builder::new().name("csm-theme-watch".into()).spawn(move || {
        type Stamp = Option<(Option<std::time::SystemTime>, u64)>;
        let mut seen: BTreeMap<PathBuf, Stamp> = BTreeMap::new();
        loop {
            std::thread::sleep(THEME_POLL);
            // The catalogue is gone (the engine stopped): so is the reason to watch.
            if Arc::strong_count(&mine) == 1 {
                return;
            }
            let paths = mine.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            seen.retain(|p, _| paths.contains(p));
            let mut changed = false;
            for p in paths {
                let now: Stamp = std::fs::metadata(&p).ok().map(|m| (m.modified().ok(), m.len()));
                match seen.insert(p, now) {
                    Some(before) if before != now => changed = true,
                    _ => {}
                }
            }
            if changed {
                engine.post(Box::new(|ctx: &mut Ctx<'_>| {
                    if !ctx.state.rt.addons.scanning {
                        reload(ctx);
                    }
                }));
            }
        }
    });
    if spawned.is_ok() {
        cat.watch = Some(shared);
    }
}

/// The themes of the enabled add-ons: `(addon id, addon name, theme)`.
pub(crate) fn themes(cat: &Catalogue) -> impl Iterator<Item = (&str, &str, &csm_core::addons::ThemeSpec)> {
    loaded(cat).flat_map(|p| p.themes.iter().map(move |t| (p.meta.id.as_str(), p.meta.name.as_str(), t)))
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

/// Whether a session launched from this profile is started again on restore. An empty id (a
/// session the person started by hand) always is. A profile with resume `none`, and any profile
/// whose add-on is not enabled, is not: the session comes back asleep and the person wakes it.
pub(crate) fn restores_on_start(cat: &Catalogue, profile_id: &str) -> bool {
    if profile_id.is_empty() {
        return true;
    }
    profile(cat, profile_id).is_ok_and(|(_, spec)| spec.resume != core::ResumeRule::None)
}

/// The screen's status for a rule set, in the app's terms.
pub(crate) fn screen_status(rules: &[StatusRule], screen: &str) -> Option<ProfileStatus> {
    core::match_status(rules, screen)
}

/// Everything the UI and `addon_list` show for the add-on folders, in folder order.
pub(crate) fn summaries(cat: &Catalogue) -> Vec<AddonSummary> {
    cat.entries.iter().map(|e| summary(e, cat.runtime.get(&e.folder))).collect()
}

/// The built-in add-ons (`addon_list`'s `builtin`, the snapshot's `builtin_addons`).
pub fn builtin_summaries(settings: &csm_core::models::AppSettings) -> Vec<AddonSummary> {
    vec![claude_usage_summary(settings)]
}

/// The built-in Claude usage add-on's id. Built-in ids have a `builtin:` prefix, which no add-on
/// folder can have (`:` is not allowed in an add-on id), so they never collide.
pub const CLAUDE_USAGE_ID: &str = "builtin:claude-usage";
/// What `addon_enable` takes as a built-in's version and digest (`addon_list` shows them), and
/// what its approval stores in `AppSettings::addon_approvals`.
pub const BUILTIN_VERSION: &str = "built-in";
pub const BUILTIN_DIGEST: &str = "builtin";
/// What a usage tool says while the add-on is off.
pub const CLAUDE_USAGE_OFF: &str = "Claude usage is off — enable it in Settings › Add-ons";

/// Whether a built-in add-on is known by this id.
pub(crate) fn is_builtin(id: &str) -> bool {
    id == CLAUDE_USAGE_ID
}

/// Whether the built-in Claude usage add-on is on. Off until the person enables it (existing
/// installs included); its index data is kept either way.
pub fn claude_usage_on(settings: &csm_core::models::AppSettings) -> bool {
    settings.addon_approvals.get(CLAUDE_USAGE_ID).is_some_and(|d| d == BUILTIN_DIGEST)
}

/// The built-in Claude usage add-on as `addon_list` and the snapshot show it.
pub fn claude_usage_summary(settings: &csm_core::models::AppSettings) -> AddonSummary {
    let on = claude_usage_on(settings);
    AddonSummary {
        id: CLAUDE_USAGE_ID.to_owned(),
        name: "Claude usage".to_owned(),
        version: BUILTIN_VERSION.to_owned(),
        description: "Built in. Reads Claude Code's own transcripts on this machine for tokens and API-equivalent cost: the inspector's Usage tab, the usage dashboard, cost on the sidebar chips, Settings › Usage and the usage tools. With \"Read past Claude history\" on (Settings › Usage) it also imports every past transcript, not only the sessions CSM runs. Numbers only; nothing leaves the machine. Context fill, subagents and Remote Control work without it.".to_owned(),
        kind: "builtin",
        author: None,
        homepage: None,
        state: if on { AddonState::Loaded } else { AddonState::Disabled },
        message: (!on).then(|| "built in; off until you enable it".to_owned()),
        folder: String::new(),
        path: String::new(),
        digest: Some(BUILTIN_DIGEST.to_owned()),
        profiles: Vec::new(),
        run_templates: Vec::new(),
        slash: Vec::new(),
        commands: Vec::new(),
        process_effects: Vec::new(),
        process_command_line: None,
        process_calls: Vec::new(),
        new_sessions: Vec::new(),
        themes: Vec::new(),
        theme_fonts: Vec::new(),
        process: None,
        problems: Vec::new(),
    }
}

fn summary(e: &Entry, runtime: Option<&PluginRuntime>) -> AddonSummary {
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
        digest: e.digest.clone(),
        profiles: p.map_or_else(Vec::new, |p| {
            p.profiles
                .iter()
                .map(|x| ProfileSummary {
                    id: format!("{id}.{}", x.name),
                    name: x.name.clone(),
                    command_line: x.argv.iter().chain(x.args.iter()).cloned().collect::<Vec<_>>().join(" "),
                    resume: match &x.resume {
                        core::ResumeRule::None => "none",
                        core::ResumeRule::ReRun => "re-run",
                        core::ResumeRule::Template(_) => "template",
                    },
                })
                .collect()
        }),
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
        // The stderr tail is not in the list (`addon_list` is read-only; `plugin_status` returns it).
        process_command_line: p.and_then(|p| p.process.as_ref()).map(|s| s.argv.join(" ")),
        process_calls: e.standing.clone(),
        new_sessions: p.map_or_else(Vec::new, |p| p.new_sessions.iter().map(|s| NewSessionSummary { id: s.id.clone(), title: s.title.clone(), submit: s.submit.clone(), choices_tool: s.choices_tool.clone(), tool: s.tool.clone() }).collect()),
        process: runtime.map(|r| PluginRuntime { stderr_tail: String::new(), ..r.clone() }),
        themes: p.map_or_else(Vec::new, |p| p.themes.clone()),
        theme_fonts: e.fonts.clone(),
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

    #[test]
    fn only_the_latest_scan_is_installed() {
        use crate::{Engine, State};
        let engine = Engine::start(State::default(), vec![]).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        engine.post(Box::new(move |ctx: &mut Ctx<'_>| {
            ctx.state.rt.addons.scan_seq = 2;
            ctx.state.rt.addons.scanning = true;
            // Scan 1 finishes after scan 2 was started: it is dropped, and the scan is still on its way.
            install_scan(ctx, 1, Vec::new(), Some("old".into()));
            let after_stale = (ctx.state.rt.addons.scanning, ctx.state.rt.addons.scan_error.clone());
            install_scan(ctx, 2, Vec::new(), None);
            let after_latest = (ctx.state.rt.addons.scanning, ctx.state.rt.addons.scan_error.clone());
            let _ = tx.send((after_stale, after_latest));
        }));
        let (stale, latest) = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(stale, (true, None), "a stale scan must not install");
        assert_eq!(latest, (false, None));
        engine.shutdown();
    }
}

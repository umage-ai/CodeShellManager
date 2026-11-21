//! The add-on manifest (`addon.toml`), parsed and validated (docs/addons.md).
//!
//! Pure: text in, [`ParsedAddon`] out. Parsing is tolerant per entry: one bad `[[profile]]`,
//! `[[slash]]` and so on is reported as a [`Problem`] and skipped, and the rest of the add-on
//! still loads. Only a manifest that cannot be read at all (TOML syntax, a missing or invalid
//! `[addon]` table, an unknown `api`) is a fatal [`AddonError`].
//!
//! Everything here is hostile input: no `unwrap`, no indexing, every size capped.

use std::fmt;

use regex::{Regex, RegexBuilder};
use semver::Version;
use serde_json::{Map, Value as Json};
use toml::{Table, Value};

use crate::models::SlashCommand;

/// The manifest API this build understands. A manifest with a larger `api` is refused with a
/// message that asks for a newer CodeShellManager; a smaller one is read as-is.
pub const API_VERSION: i64 = 1;
/// Most entries of each kind kept per add-on; the rest are reported and ignored.
pub const MAX_ENTRIES: usize = 32;
/// Longest name, title or short label, in characters.
pub const MAX_NAME: usize = 64;
/// Longest description, in characters.
pub const MAX_DESCRIPTION: usize = 1000;
/// Most elements in one argv list.
pub const MAX_ARGV: usize = 32;
/// Longest single argv element, in bytes.
pub const MAX_TOKEN: usize = 512;
/// Most status rules per profile.
pub const MAX_RULES: usize = 16;
/// Longest regex source, in bytes.
pub const MAX_REGEX: usize = 512;
/// Largest JSON params object for a palette command, in bytes.
pub const MAX_PARAMS_JSON: usize = 4096;
/// Largest `addon.toml`, in bytes (checked by the reader before parsing).
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// What an add-on is. A `pack` only declares data; a `process` add-on also declares a
/// `[process]` table (argv and the effects it may request). Process add-ons are parsed and
/// validated but not run yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddonKind {
    Pack,
    Process,
}

impl AddonKind {
    pub fn id(self) -> &'static str {
        match self {
            AddonKind::Pack => "pack",
            AddonKind::Process => "process",
        }
    }
}

/// The `[addon]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonMeta {
    pub id: String,
    pub name: String,
    pub version: Version,
    pub description: String,
    pub api: i64,
    pub kind: AddonKind,
    pub homepage: Option<String>,
    pub author: Option<String>,
}

/// The status a profile's rule can report. Mapped to the app's status vocabulary by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileStatus {
    NeedsYouApproval,
    NeedsYouQuestion,
    Working,
    Idle,
    Done,
}

impl ProfileStatus {
    pub fn parse(s: &str) -> Option<ProfileStatus> {
        Some(match s {
            "needs_you_approval" => ProfileStatus::NeedsYouApproval,
            "needs_you_question" => ProfileStatus::NeedsYouQuestion,
            "working" => ProfileStatus::Working,
            "idle" => ProfileStatus::Idle,
            "done" => ProfileStatus::Done,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            ProfileStatus::NeedsYouApproval => "needs_you_approval",
            ProfileStatus::NeedsYouQuestion => "needs_you_question",
            ProfileStatus::Working => "working",
            ProfileStatus::Idle => "idle",
            ProfileStatus::Done => "done",
        }
    }
}

/// One `match = regex, status = ...` rule. The regex is compiled with a size limit.
#[derive(Debug, Clone)]
pub struct StatusRule {
    pub pattern: String,
    pub regex: Regex,
    pub status: ProfileStatus,
}

impl PartialEq for StatusRule {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern && self.status == other.status
    }
}

/// How a profile's session comes back after the app restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeRule {
    /// Nothing: the session is not started again on restore. It comes back asleep and the
    /// person wakes it. (An add-on that is not enabled is treated the same way.)
    None,
    /// Reattach by running the same argv again (the restore path already does this).
    ReRun,
    /// A full argv template; `{session_id}` is replaced with the stored session id.
    Template(Vec<String>),
}

/// `[[profile]]`: an agent launch profile.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileSpec {
    pub name: String,
    /// Program and its fixed arguments; plain tokens only (see [`plain_token`]).
    pub argv: Vec<String>,
    /// Extra arguments appended after `argv`, plain tokens only.
    pub args: Vec<String>,
    pub resume: ResumeRule,
    pub status: Vec<StatusRule>,
}

/// `[[run_template]]`: a run command the person can add to a session's Runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTemplateSpec {
    pub name: String,
    /// Plain tokens only. Joined with single spaces when added to a session.
    pub argv: Vec<String>,
    /// Working folder template: `{folder}` or a plain relative token. Parsed, not yet applied
    /// (run commands have no working-folder field yet); see docs/addons.md.
    pub cwd: Option<String>,
}

/// `[[slash]]`: a Claude slash preset (merged into the slash menu).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSpec {
    pub command: String,
    pub description: String,
}

/// `[[command]]`: a palette command that calls an existing tool with fixed params.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandSpec {
    pub id: String,
    pub title: String,
    /// A tool id (`snake_case`). Checked against the registry by the engine, not here.
    pub tool: String,
    pub params: Json,
}

/// The `[process]` table of a process add-on (validated, not run yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub argv: Vec<String>,
    /// Effects the process may request, from [`PROCESS_EFFECTS`].
    pub effects: Vec<String>,
}

/// The effect names a process add-on may declare (the consent dialog lists them).
pub const PROCESS_EFFECTS: [&str; 5] = ["launches_process", "sends_input", "reads_session_content", "writes_filesystem", "accesses_network"];

/// One entry or table that was skipped, and why. `scope` names it ("profile 2 `claude`").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub scope: String,
    pub message: String,
}

/// A manifest that loaded, with whatever entries were valid.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedAddon {
    pub meta: AddonMeta,
    pub profiles: Vec<ProfileSpec>,
    pub run_templates: Vec<RunTemplateSpec>,
    pub slash: Vec<SlashSpec>,
    pub commands: Vec<CommandSpec>,
    pub process: Option<ProcessSpec>,
    /// Entries that were skipped, in file order.
    pub problems: Vec<Problem>,
}

/// Why a whole manifest cannot load. The message says what to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddonError {
    Syntax(String),
    MissingTable,
    Field { field: &'static str, message: String },
    NewerApi(i64),
    IdMismatch { folder: String, id: String },
}

impl fmt::Display for AddonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AddonError::Syntax(m) => write!(f, "addon.toml is not valid TOML: {m}"),
            AddonError::MissingTable => f.write_str("addon.toml needs an [addon] table"),
            AddonError::Field { field, message } => write!(f, "[addon] {field}: {message}"),
            AddonError::NewerApi(api) => {
                write!(f, "this add-on needs manifest API {api}; this version of CodeShellManager reads API {API_VERSION}. Update CodeShellManager.")
            }
            AddonError::IdMismatch { folder, id } => write!(f, "the folder is `{folder}` but the id is `{id}`; they must match"),
        }
    }
}

impl std::error::Error for AddonError {}

/// Parses `addon.toml` text. `folder` is the add-on's folder name, which must equal the id.
pub fn parse(text: &str, folder: &str) -> Result<ParsedAddon, AddonError> {
    let table: Table = text.parse().map_err(|e: toml::de::Error| AddonError::Syntax(e.message().to_owned()))?;
    let meta = parse_meta(&table, folder)?;
    let mut problems = Vec::new();

    let profiles = entries(&table, "profile", &mut problems, parse_profile);
    let run_templates = entries(&table, "run_template", &mut problems, parse_run_template);
    let slash = entries(&table, "slash", &mut problems, parse_slash);
    let commands = entries(&table, "command", &mut problems, parse_command);
    let process = match meta.kind {
        AddonKind::Process => match table.get("process") {
            Some(Value::Table(t)) => match parse_process(t) {
                Ok(p) => Some(p),
                Err(m) => {
                    problems.push(Problem { scope: "[process]".into(), message: m });
                    None
                }
            },
            Some(_) => {
                problems.push(Problem { scope: "[process]".into(), message: "must be a table".into() });
                None
            }
            None => {
                problems.push(Problem { scope: "[process]".into(), message: "a process add-on needs a [process] table".into() });
                None
            }
        },
        AddonKind::Pack => {
            if table.contains_key("process") {
                problems.push(Problem { scope: "[process]".into(), message: "only a `process` add-on may have a [process] table; ignored".into() });
            }
            None
        }
    };
    for key in table.keys() {
        if !matches!(key.as_str(), "addon" | "profile" | "run_template" | "slash" | "command" | "process") {
            problems.push(Problem { scope: format!("`{key}`"), message: "unknown table; ignored".into() });
        }
    }
    Ok(ParsedAddon { meta, profiles, run_templates, slash, commands, process, problems })
}

fn parse_meta(t: &Table, folder: &str) -> Result<AddonMeta, AddonError> {
    let Some(Value::Table(a)) = t.get("addon") else {
        return Err(AddonError::MissingTable);
    };
    let field = |name: &'static str| -> Result<&str, AddonError> {
        a.get(name).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| AddonError::Field {
            field: name,
            message: "a non-empty string is required".into(),
        })
    };
    let id = field("id")?;
    if !valid_id(id) {
        return Err(AddonError::Field { field: "id", message: "use 1 to 64 characters: a-z, 0-9 and '-'".into() });
    }
    if id != folder {
        return Err(AddonError::IdMismatch { folder: folder.to_owned(), id: id.to_owned() });
    }
    let name = clean_text(field("name")?, MAX_NAME);
    let version_text = field("version")?;
    let version = Version::parse(version_text)
        .map_err(|e| AddonError::Field { field: "version", message: format!("`{version_text}` is not a semantic version ({e})") })?;
    let api = match a.get("api") {
        Some(Value::Integer(n)) => *n,
        _ => return Err(AddonError::Field { field: "api", message: "an integer is required (this build reads 1)".into() }),
    };
    if api > API_VERSION {
        return Err(AddonError::NewerApi(api));
    }
    if api < 1 {
        return Err(AddonError::Field { field: "api", message: "must be 1 or more".into() });
    }
    let kind = match field("kind")? {
        "pack" => AddonKind::Pack,
        "process" => AddonKind::Process,
        other => return Err(AddonError::Field { field: "kind", message: format!("`{other}` is not \"pack\" or \"process\"") }),
    };
    let description = a.get("description").and_then(Value::as_str).map(|d| clean_text(d, MAX_DESCRIPTION)).unwrap_or_default();
    let optional = |name: &str| a.get(name).and_then(Value::as_str).map(|s| clean_text(s, MAX_DESCRIPTION)).filter(|s| !s.is_empty());
    Ok(AddonMeta {
        id: id.to_owned(),
        name,
        version,
        description,
        api,
        kind,
        homepage: optional("homepage"),
        author: optional("author"),
    })
}

/// Reads the `[[name]]` array of tables, skipping (and reporting) entries that do not parse.
fn entries<T>(t: &Table, key: &str, problems: &mut Vec<Problem>, parse_one: fn(&Table) -> Result<T, String>) -> Vec<T> {
    let items = match t.get(key) {
        None => return Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => {
            problems.push(Problem { scope: format!("[[{key}]]"), message: "must be an array of tables; ignored".into() });
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let label = format!("[[{key}]] #{}", i + 1);
        if i >= MAX_ENTRIES {
            problems.push(Problem { scope: label, message: format!("more than {MAX_ENTRIES} of this kind; ignored") });
            continue;
        }
        let Value::Table(table) = item else {
            problems.push(Problem { scope: label, message: "must be a table; ignored".into() });
            continue;
        };
        match parse_one(table) {
            Ok(v) => out.push(v),
            Err(m) => problems.push(Problem { scope: label, message: m }),
        }
    }
    out
}

fn parse_profile(t: &Table) -> Result<ProfileSpec, String> {
    let name = name_field(t, "name")?;
    let scope = format!("profile `{name}`");
    let argv = argv_field(t, "argv", true)?;
    let args = argv_field(t, "args", false)?;
    no_placeholders(&argv, "argv", &scope)?;
    no_placeholders(&args, "args", &scope)?;
    let resume = match t.get("resume") {
        None => ResumeRule::None,
        Some(Value::String(s)) => match s.as_str() {
            "none" => ResumeRule::None,
            "re-run" => ResumeRule::ReRun,
            other => return Err(format!("{scope}: resume `{other}` is not \"none\", \"re-run\" or an argv list with {{session_id}}")),
        },
        Some(Value::Array(_)) => {
            let tpl = argv_field(t, "resume", true)?;
            if !tpl.iter().any(|a| a.contains("{session_id}")) {
                return Err(format!("{scope}: a resume argv must contain {{session_id}}"));
            }
            if tpl.iter().any(|a| a.contains("{folder}")) {
                return Err(format!("{scope}: a resume argv may use {{session_id}} only"));
            }
            ResumeRule::Template(tpl)
        }
        Some(_) => return Err(format!("{scope}: resume must be a string or an argv list")),
    };
    let status = status_rules(t, &scope)?;
    Ok(ProfileSpec { name, argv, args, resume, status })
}

/// `status = [{ match = "...", status = "..." }]`. A bad rule fails the whole profile with a
/// message that names the rule; the other rules are not applied in that case.
fn status_rules(t: &Table, scope: &str) -> Result<Vec<StatusRule>, String> {
    let Some(v) = t.get("status") else {
        return Ok(Vec::new());
    };
    let Value::Array(items) = v else {
        return Err(format!("{scope}: status must be an array of {{ match, status }} tables"));
    };
    if items.len() > MAX_RULES {
        return Err(format!("{scope}: more than {MAX_RULES} status rules"));
    }
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let Value::Table(r) = item else {
            return Err(format!("{scope}: status rule {} must be a table", i + 1));
        };
        let pattern = r.get("match").and_then(Value::as_str).ok_or_else(|| format!("{scope}: status rule {} needs a `match` string", i + 1))?;
        if pattern.is_empty() || pattern.len() > MAX_REGEX {
            return Err(format!("{scope}: status rule {} `match` must be 1 to {MAX_REGEX} bytes", i + 1));
        }
        let regex = RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .dfa_size_limit(1 << 20)
            .build()
            .map_err(|e| format!("{scope}: status rule {} is not a valid regex ({})", i + 1, short(&e.to_string())))?;
        let status_name = r.get("status").and_then(Value::as_str).unwrap_or_default();
        let status = ProfileStatus::parse(status_name).ok_or_else(|| {
            format!("{scope}: status rule {} status `{status_name}` is not needs_you_approval, needs_you_question, working, idle or done", i + 1)
        })?;
        out.push(StatusRule { pattern: pattern.to_owned(), regex, status });
    }
    Ok(out)
}

fn parse_run_template(t: &Table) -> Result<RunTemplateSpec, String> {
    let name = name_field(t, "name")?;
    let scope = format!("run template `{name}`");
    let argv = argv_field(t, "argv", true)?;
    no_placeholders(&argv, "argv", &scope)?;
    let cwd = match t.get("cwd") {
        None => None,
        Some(Value::String(c)) => {
            let ok = c == "{folder}" || (plain_token(c) && !c.contains("..") && !c.starts_with('/') && !c.starts_with('\\'));
            if !ok {
                return Err(format!("{scope}: cwd must be {{folder}} or a relative folder name"));
            }
            Some(c.clone())
        }
        Some(_) => return Err(format!("{scope}: cwd must be a string")),
    };
    Ok(RunTemplateSpec { name, argv, cwd })
}

fn parse_slash(t: &Table) -> Result<SlashSpec, String> {
    let command = t.get("command").and_then(Value::as_str).ok_or("`command` must be a string")?;
    let command = SlashCommand::clean_command(command).ok_or_else(|| format!("slash command `{command}` is empty or not usable"))?;
    let description = t.get("description").and_then(Value::as_str).map(|d| clean_text(d, MAX_DESCRIPTION)).unwrap_or_default();
    Ok(SlashSpec { command, description })
}

fn parse_command(t: &Table) -> Result<CommandSpec, String> {
    let id = name_field(t, "id")?;
    if !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err(format!("command `{id}`: id may use a-z, 0-9, '-' and '_'"));
    }
    let title = clean_text(t.get("title").and_then(Value::as_str).ok_or("`title` must be a string")?, MAX_NAME);
    let tool = t.get("tool").and_then(Value::as_str).ok_or("`tool` must be a tool id string")?;
    if tool.is_empty() || tool.len() > 64 || !tool.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        return Err(format!("command `{id}`: `{tool}` is not a tool id"));
    }
    if tool.starts_with("addon_") {
        return Err(format!("command `{id}`: an add-on command cannot call the add-on tools"));
    }
    let params = match t.get("params") {
        None => Json::Object(Map::new()),
        Some(v) => {
            let j = toml_to_json(v).ok_or_else(|| format!("command `{id}`: params contain a value JSON cannot hold"))?;
            if !j.is_object() {
                return Err(format!("command `{id}`: params must be a table"));
            }
            let size = serde_json::to_string(&j).map(|s| s.len()).unwrap_or(usize::MAX);
            if size > MAX_PARAMS_JSON {
                return Err(format!("command `{id}`: params are larger than {MAX_PARAMS_JSON} bytes"));
            }
            j
        }
    };
    Ok(CommandSpec { id: id.to_owned(), title, tool: tool.to_owned(), params })
}

fn parse_process(t: &Table) -> Result<ProcessSpec, String> {
    let argv = argv_field(t, "argv", true)?;
    no_placeholders(&argv, "argv", "[process]")?;
    let effects = match t.get("effects") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::new();
            for e in items {
                let s = e.as_str().ok_or("process effects must be strings")?;
                if !PROCESS_EFFECTS.contains(&s) {
                    return Err(format!("process effect `{s}` is not one of {}", PROCESS_EFFECTS.join(", ")));
                }
                if !out.iter().any(|x: &String| x == s) {
                    out.push(s.to_owned());
                }
            }
            out
        }
        Some(_) => return Err("process effects must be an array of strings".into()),
    };
    Ok(ProcessSpec { argv, effects })
}

/// A name or id field: non-empty, control characters dropped, at most [`MAX_NAME`] characters.
fn name_field(t: &Table, key: &str) -> Result<String, String> {
    let s = t.get(key).and_then(Value::as_str).ok_or_else(|| format!("`{key}` must be a string"))?;
    let c = clean_text(s, MAX_NAME);
    if c.is_empty() {
        return Err(format!("`{key}` is empty"));
    }
    Ok(c)
}

/// An argv list. `required` lists must be non-empty. Every element is a plain token.
fn argv_field(t: &Table, key: &str, required: bool) -> Result<Vec<String>, String> {
    let items = match t.get(key) {
        None if required => return Err(format!("`{key}` is required (an argv list of plain tokens)")),
        None => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(format!("`{key}` must be a list of strings")),
    };
    if items.len() > MAX_ARGV {
        return Err(format!("`{key}` has more than {MAX_ARGV} elements"));
    }
    if required && items.is_empty() {
        return Err(format!("`{key}` is empty"));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let s = item.as_str().ok_or_else(|| format!("`{key}` elements must be strings"))?;
        if !plain_token(s) {
            return Err(format!(
                "`{key}` element `{}` is not a plain token: use letters, digits and - _ . / \\ : = + @ , only (split words into separate elements)",
                clean_text(s, 40)
            ));
        }
        out.push(s.to_owned());
    }
    Ok(out)
}

/// Argv lists take no placeholders: nothing substitutes them, so `{folder}` or `{session_id}`
/// would reach the program as literal text. The resume template takes `{session_id}` and a run
/// template's `cwd` takes `{folder}`; both are checked where they are parsed.
fn no_placeholders(argv: &[String], key: &str, scope: &str) -> Result<(), String> {
    if argv.iter().any(|a| a.contains('{')) {
        return Err(format!(
            "{scope}: `{key}` may not use {{session_id}} or {{folder}} (only a resume template takes {{session_id}}, and a run template's cwd takes {{folder}})"
        ));
    }
    Ok(())
}

/// True for a token that can be exec'd and joined with spaces without any shell meaning:
/// letters, digits and `_ - . / \ : = + @ ,`, plus the placeholders `{session_id}` and
/// `{folder}`. Nothing that a shell, cmd.exe or PowerShell would interpret (quotes, `%`, `$`,
/// `&`, `|`, `;`, spaces) is allowed, so the token is safe to put in a command line as it is.
pub fn plain_token(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_TOKEN {
        return false;
    }
    let stripped = s.replace("{session_id}", "x").replace("{folder}", "x");
    stripped.chars().all(|c| c.is_ascii_alphanumeric() || "_-./\\:=+@,".contains(c))
}

/// Add-on and entry ids: 1 to 64 characters, lowercase letters, digits and `-`.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Drops control and invisible format characters, trims, and caps the length (in characters).
pub fn clean_text(s: &str, cap: usize) -> String {
    s.chars().filter(|c| !c.is_control() && !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')).collect::<String>().trim().chars().take(cap).collect()
}

fn short(s: &str) -> String {
    clean_text(s, 120)
}

/// TOML values as JSON. Datetimes become strings; non-finite floats make the whole value fail.
fn toml_to_json(v: &Value) -> Option<Json> {
    Some(match v {
        Value::String(s) => Json::String(s.clone()),
        Value::Integer(n) => Json::from(*n),
        Value::Float(f) => Json::from(serde_json::Number::from_f64(*f)?),
        Value::Boolean(b) => Json::Bool(*b),
        Value::Datetime(d) => Json::String(d.to_string()),
        Value::Array(items) => Json::Array(items.iter().map(toml_to_json).collect::<Option<Vec<_>>>()?),
        Value::Table(t) => {
            let mut m = Map::new();
            for (k, v) in t {
                m.insert(k.clone(), toml_to_json(v)?);
            }
            Json::Object(m)
        }
    })
}

/// The first profile rule whose regex matches the tail of a screen. Only the last
/// `TAIL_LINES` non-empty lines (and at most `TAIL_BYTES` bytes) are searched, so an old
/// prompt scrolled far up does not count.
pub fn match_status(rules: &[StatusRule], screen: &str) -> Option<ProfileStatus> {
    const TAIL_LINES: usize = 12;
    const TAIL_BYTES: usize = 4096;
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut tail = lines.iter().rev().take(TAIL_LINES).rev().copied().collect::<Vec<_>>().join("\n");
    if tail.len() > TAIL_BYTES {
        let mut cut = tail.len() - TAIL_BYTES;
        while cut < tail.len() && !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = tail.get(cut..).unwrap_or_default().to_owned();
    }
    rules.iter().find(|r| r.regex.is_match(&tail)).map(|r| r.status)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "[addon]\nid = \"demo\"\nname = \"Demo\"\nversion = \"1.2.3\"\ndescription = \"A demo\"\napi = 1\nkind = \"pack\"\n";

    fn ok(extra: &str) -> ParsedAddon {
        parse(&format!("{HEAD}{extra}"), "demo").unwrap()
    }

    #[test]
    fn meta_round_trips_and_optional_fields_are_optional() {
        let p = ok("");
        assert_eq!(p.meta.id, "demo");
        assert_eq!(p.meta.name, "Demo");
        assert_eq!(p.meta.version, Version::new(1, 2, 3));
        assert_eq!(p.meta.kind, AddonKind::Pack);
        assert_eq!(p.meta.homepage, None);
        assert!(p.profiles.is_empty() && p.problems.is_empty());
    }

    #[test]
    fn folder_must_match_the_id() {
        let e = parse(HEAD, "other").unwrap_err();
        assert!(matches!(e, AddonError::IdMismatch { .. }), "{e}");
    }

    #[test]
    fn newer_api_is_refused_with_a_clear_message() {
        let text = HEAD.replace("api = 1", "api = 2");
        let e = parse(&text, "demo").unwrap_err();
        assert_eq!(e, AddonError::NewerApi(2));
        assert!(e.to_string().contains("Update CodeShellManager"));
    }

    #[test]
    fn bad_meta_fields_are_fatal_and_named() {
        for (from, to, field) in [
            ("id = \"demo\"", "id = \"De mo\"", "id"),
            ("version = \"1.2.3\"", "version = \"one\"", "version"),
            ("kind = \"pack\"", "kind = \"plugin\"", "kind"),
            ("name = \"Demo\"", "name = \"  \"", "name"),
            ("api = 1", "api = \"1\"", "api"),
        ] {
            let text = HEAD.replace(from, to);
            match parse(&text, "demo") {
                Err(AddonError::Field { field: f, .. }) => assert_eq!(f, field),
                other => panic!("{field}: {other:?}"),
            }
        }
    }

    #[test]
    fn missing_addon_table_and_bad_toml_are_fatal() {
        assert_eq!(parse("[other]\nx = 1\n", "demo"), Err(AddonError::MissingTable));
        assert!(matches!(parse("[addon\nid = ", "demo"), Err(AddonError::Syntax(_))));
    }

    #[test]
    fn profile_parses_argv_args_resume_and_status_rules() {
        let p = ok(r#"
[[profile]]
name = "generic"
argv = ["generic-agent", "--interactive"]
args = ["--fast"]
resume = ["generic-agent", "--resume", "{session_id}"]
status = [
  { match = "Allow this\\?", status = "needs_you_approval" },
  { match = "^> $", status = "idle" },
]
"#);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        let prof = &p.profiles[0];
        assert_eq!(prof.argv, vec!["generic-agent", "--interactive"]);
        assert_eq!(prof.args, vec!["--fast"]);
        assert_eq!(prof.resume, ResumeRule::Template(vec!["generic-agent".into(), "--resume".into(), "{session_id}".into()]));
        assert_eq!(prof.status.len(), 2);
        assert_eq!(prof.status[0].status, ProfileStatus::NeedsYouApproval);
    }

    #[test]
    fn resume_re_run_and_none() {
        let p = ok("[[profile]]\nname = \"a\"\nargv = [\"a\"]\nresume = \"re-run\"\n[[profile]]\nname = \"b\"\nargv = [\"b\"]\nresume = \"none\"\n");
        assert_eq!(p.profiles[0].resume, ResumeRule::ReRun);
        assert_eq!(p.profiles[1].resume, ResumeRule::None);
    }

    #[test]
    fn a_bad_entry_is_reported_and_the_rest_still_load() {
        let p = ok(r#"
[[profile]]
name = "bad"
argv = ["rm", "-rf", "x; del"]

[[profile]]
name = "good"
argv = ["good"]

[[slash]]
command = "/review"
description = "Review"

[[slash]]
command = ""
"#);
        assert_eq!(p.profiles.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["good"]);
        assert_eq!(p.slash.len(), 1);
        assert_eq!(p.problems.len(), 2, "{:?}", p.problems);
        assert!(p.problems[0].message.contains("not a plain token"));
    }

    #[test]
    fn shell_metacharacters_are_never_plain_tokens() {
        for bad in ["a b", "x&y", "$HOME", "%PATH%", "\"q\"", "a|b", "a;b", "`x`", "a*", "~", "{other}", "é", ""] {
            assert!(!plain_token(bad), "{bad:?} must not be plain");
        }
        for good in ["claude", "C:\\tools\\x.exe", "--flag=value", "a/b.c", "{session_id}", "{folder}", "+x@1,2"] {
            assert!(plain_token(good), "{good:?} must be plain");
        }
    }

    #[test]
    fn bad_regex_drops_that_profile_only() {
        let p = ok("[[profile]]\nname = \"p\"\nargv = [\"p\"]\nstatus = [{ match = \"(unclosed\", status = \"idle\" }]\n[[profile]]\nname = \"q\"\nargv = [\"q\"]\n");
        assert_eq!(p.profiles.len(), 1);
        assert_eq!(p.profiles[0].name, "q");
        assert!(p.problems[0].message.contains("not a valid regex"));
    }

    #[test]
    fn oversized_regex_is_refused() {
        let long = "a".repeat(MAX_REGEX + 1);
        let p = ok(&format!("[[profile]]\nname = \"p\"\nargv = [\"p\"]\nstatus = [{{ match = \"{long}\", status = \"idle\" }}]\n"));
        assert!(p.profiles.is_empty());
        assert!(p.problems[0].message.contains("1 to"));
    }

    #[test]
    fn unknown_status_and_resume_are_reported() {
        let p = ok("[[profile]]\nname = \"p\"\nargv = [\"p\"]\nstatus = [{ match = \"x\", status = \"sleeping\" }]\n[[profile]]\nname = \"q\"\nargv = [\"q\"]\nresume = \"always\"\n");
        assert!(p.profiles.is_empty());
        assert_eq!(p.problems.len(), 2);
    }

    #[test]
    fn resume_template_must_name_the_session_id() {
        let p = ok("[[profile]]\nname = \"p\"\nargv = [\"p\"]\nresume = [\"p\", \"--continue\"]\n");
        assert!(p.profiles.is_empty());
        assert!(p.problems[0].message.contains("{session_id}"));
    }

    #[test]
    fn argv_lists_refuse_placeholders_nothing_would_substitute() {
        let p = ok("[[profile]]\nname = \"p\"\nargv = [\"p\", \"--dir={folder}\"]\n[[profile]]\nname = \"q\"\nargv = [\"q\"]\nargs = [\"{session_id}\"]\n[[run_template]]\nname = \"r\"\nargv = [\"r\", \"{folder}\"]\n[[run_template]]\nname = \"ok\"\nargv = [\"ok\"]\ncwd = \"{folder}\"\n");
        assert!(p.profiles.is_empty());
        assert_eq!(p.problems.len(), 3, "{:?}", p.problems);
        assert!(p.problems.iter().all(|x| x.message.contains("may not use")));
        assert_eq!(p.run_templates.len(), 1);
        assert_eq!(p.run_templates[0].name, "ok");
    }

    #[test]
    fn run_template_with_cwd_rules() {
        let p = ok("[[run_template]]\nname = \"Test\"\nargv = [\"cargo\", \"test\"]\ncwd = \"{folder}\"\n[[run_template]]\nname = \"Up\"\nargv = [\"x\"]\ncwd = \"../escape\"\n");
        assert_eq!(p.run_templates.len(), 1);
        assert_eq!(p.run_templates[0].cwd.as_deref(), Some("{folder}"));
        assert_eq!(p.problems.len(), 1);
    }

    #[test]
    fn command_needs_a_snake_case_tool_and_no_addon_tools() {
        let p = ok(r#"
[[command]]
id = "sleep-idle"
title = "Sleep idle sessions"
tool = "group_sleep_all"
params = { group_id = "g1", force = true }

[[command]]
id = "self"
title = "Enable everything"
tool = "addon_enable"

[[command]]
id = "bad"
title = "Bad"
tool = "Rm -rf"
"#);
        assert_eq!(p.commands.len(), 1);
        assert_eq!(p.commands[0].tool, "group_sleep_all");
        assert_eq!(p.commands[0].params["force"], Json::Bool(true));
        assert_eq!(p.problems.len(), 2);
    }

    #[test]
    fn command_params_must_be_a_table_and_small() {
        let p = ok("[[command]]\nid = \"a\"\ntitle = \"A\"\ntool = \"app_info\"\nparams = [1]\n");
        assert!(p.commands.is_empty());
        let big = "x".repeat(MAX_PARAMS_JSON + 1);
        let p = ok(&format!("[[command]]\nid = \"a\"\ntitle = \"A\"\ntool = \"app_info\"\nparams = {{ text = \"{big}\" }}\n"));
        assert!(p.commands.is_empty());
        assert!(p.problems[0].message.contains("larger than"));
    }

    #[test]
    fn caps_truncate_long_lists_and_strings() {
        let mut text = String::from(HEAD);
        for i in 0..(MAX_ENTRIES + 3) {
            text.push_str(&format!("[[slash]]\ncommand = \"/c{i}\"\ndescription = \"d\"\n"));
        }
        let p = parse(&text, "demo").unwrap();
        assert_eq!(p.slash.len(), MAX_ENTRIES);
        assert_eq!(p.problems.len(), 3);
        let long_name = "n".repeat(500);
        let p = parse(&HEAD.replace("Demo", &long_name), "demo").unwrap();
        assert_eq!(p.meta.name.chars().count(), MAX_NAME);
    }

    #[test]
    fn process_kind_needs_a_process_table_and_known_effects() {
        let text = HEAD.replace("kind = \"pack\"", "kind = \"process\"");
        let p = parse(&text, "demo").unwrap();
        assert!(p.process.is_none());
        assert_eq!(p.problems.len(), 1);
        let p = parse(&format!("{text}[process]\nargv = [\"demo-helper\"]\neffects = [\"launches_process\", \"launches_process\", \"sends_input\"]\n"), "demo").unwrap();
        let spec = p.process.clone().unwrap();
        assert_eq!(spec.effects, vec!["launches_process", "sends_input"]);
        assert!(p.problems.is_empty());
        let p = parse(&format!("{text}[process]\nargv = [\"x\"]\neffects = [\"teleport\"]\n"), "demo").unwrap();
        assert!(p.process.is_none());
    }

    #[test]
    fn pack_with_a_process_table_is_reported_not_run() {
        let p = ok("[process]\nargv = [\"x\"]\n");
        assert!(p.process.is_none());
        assert!(p.problems.iter().any(|x| x.message.contains("only a `process` add-on")));
    }

    #[test]
    fn unknown_top_level_tables_are_reported() {
        let p = ok("[surprise]\nx = 1\n");
        assert!(p.problems.iter().any(|x| x.scope.contains("surprise")));
    }

    #[test]
    fn match_status_looks_only_at_the_tail() {
        let rules = parse(
            &format!("{HEAD}[[profile]]\nname = \"p\"\nargv = [\"p\"]\nstatus = [{{ match = \"Allow\", status = \"needs_you_approval\" }}]\n"),
            "demo",
        )
        .unwrap()
        .profiles
        .remove(0)
        .status;
        assert_eq!(match_status(&rules, "Allow this?\nmore\n"), Some(ProfileStatus::NeedsYouApproval));
        let mut old = String::from("Allow this (old)\n");
        for _ in 0..30 {
            old.push_str("line\n");
        }
        assert_eq!(match_status(&rules, &old), None);
        assert_eq!(match_status(&rules, ""), None);
    }

    #[test]
    fn clean_text_drops_control_and_bidi_characters() {
        assert_eq!(clean_text("a\u{202E}b\tc\n", 10), "abc");
        assert_eq!(clean_text("abcdef", 3), "abc");
    }
}

//! Turning a tool's JSON schema into a CLI subcommand, and the parsed flags back into params.
//!
//! Generated at run time from the registry: there is no hand-written list of commands. Only the
//! schema's top-level properties become flags. Strings, numbers and booleans are typed; arrays,
//! objects and anything more involved take a JSON value (or use `--json '{...}'` for the lot).

use clap::{Arg, ArgAction, ArgMatches, Command};
use csm_mcp::ToolInfo;
use serde_json::{Map, Value};

/// Flags on every tool command that are not tool params.
const RESERVED: [&str; 6] = ["json", "help", "version", "start", "app", "in-process"];

/// How a finished call is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// The message, one line.
    Message,
    /// `{"message": …, "data": …}`.
    Json,
}

fn props(schema: &Value) -> Map<String, Value> {
    schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default()
}

fn required(schema: &Value) -> Vec<String> {
    schema.get("required").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// The JSON type a flag is parsed as: "string", "integer", "number", "boolean", "array", "object",
/// or "any" (a real choice of types: the value is taken as JSON when it parses, else as a string).
fn prop_type(schema: &Value, prop: &Value) -> String {
    let resolved = resolve_ref(schema, prop);
    match resolved.get("type") {
        Some(Value::String(s)) => return s.clone(),
        Some(Value::Array(a)) => return a.iter().filter_map(Value::as_str).find(|t| *t != "null").unwrap_or("string").to_string(),
        _ => {}
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(arr) = resolved.get(key).and_then(Value::as_array) {
            let mut types = arr.iter().map(|alt| prop_type(schema, alt)).filter(|t| t != "null");
            match (types.next(), types.next()) {
                (Some(t), None) => return t,
                (Some(_), Some(_)) => return "any".into(),
                (None, _) => {}
            }
        }
    }
    if resolved.get("enum").is_some() { "string".into() } else { "any".into() }
}

fn resolve_ref<'a>(schema: &'a Value, prop: &'a Value) -> &'a Value {
    if let Some(r) = prop.get("$ref").and_then(Value::as_str) {
        let name = r.rsplit('/').next().unwrap_or_default();
        for defs in ["$defs", "definitions"] {
            if let Some(d) = schema.get(defs).and_then(|d| d.get(name)) {
                return d;
            }
        }
    }
    prop
}

fn describe(schema: &Value, prop: &Value) -> String {
    let resolved = resolve_ref(schema, prop);
    let mut text = resolved.get("description").or_else(|| prop.get("description")).and_then(Value::as_str).unwrap_or_default().to_string();
    if let Some(options) = resolved.get("enum").and_then(Value::as_array) {
        let names: Vec<&str> = options.iter().filter_map(Value::as_str).collect();
        if !names.is_empty() {
            text = format!("{text} [{}]", names.join(", ")).trim().to_string();
        }
    }
    text
}

fn flag_name(prop: &str) -> String {
    prop.replace('_', "-")
}

/// The clap command for one tool.
pub fn tool_command(info: &ToolInfo) -> Command {
    let mut cmd = Command::new(info.cli_name())
        .about(info.summary.clone())
        .hide(true)
        .arg(
            Arg::new("json")
                .long("json")
                .value_name("PARAMS")
                .num_args(0..=1)
                .help("With a value ({...} or @file.json): the tool's params as JSON. On its own: print the full result as JSON."),
        );
    let req = required(&info.input_schema);
    for (name, prop) in props(&info.input_schema) {
        let long = flag_name(&name);
        if RESERVED.contains(&long.as_str()) {
            continue;
        }
        let ty = prop_type(&info.input_schema, &prop);
        let mut help = describe(&info.input_schema, &prop);
        if req.contains(&name) {
            help = format!("{help} (required)").trim().to_string();
        }
        let mut arg = Arg::new(name.clone()).long(long).help(help).action(ArgAction::Set);
        arg = if ty == "boolean" { arg.num_args(0..=1).default_missing_value("true").value_name("true|false") } else { arg.num_args(1).value_name(ty) };
        cmd = cmd.arg(arg);
    }
    cmd
}

fn convert(ty: &str, flag: &str, raw: &str) -> Result<Value, String> {
    let bad = |what: &str| format!("--{flag} expects {what}, got `{raw}`");
    match ty {
        "integer" => raw.trim().parse::<i64>().map(Value::from).map_err(|_| bad("a whole number")),
        "number" => raw.trim().parse::<f64>().ok().filter(|n| n.is_finite()).map(Value::from).ok_or_else(|| bad("a number")),
        "boolean" => match raw.trim() {
            "true" | "yes" | "1" => Ok(Value::Bool(true)),
            "false" | "no" | "0" => Ok(Value::Bool(false)),
            _ => Err(bad("true or false")),
        },
        "object" | "array" => serde_json::from_str::<Value>(raw).map_err(|e| format!("--{flag} expects JSON: {e}")),
        "string" => Ok(Value::String(raw.to_string())),
        _ => Ok(serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))),
    }
}

/// `{...}` inline, or `@file.json`.
fn read_json_params(raw: &str) -> Result<Map<String, Value>, String> {
    let text = match raw.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?,
        None if raw.trim_start().starts_with('{') => raw.to_string(),
        None => return Err("--json takes a JSON object ({...}) or @file.json; use --json on its own to print the result as JSON".into()),
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("--json params must be a JSON object".into()),
        Err(e) => Err(format!("--json params are not valid JSON: {e}")),
    }
}

/// The params and output mode from a parsed tool command. `--json {...}` first, then flags on top.
/// Errors are usage errors (exit code 2).
pub fn params_from_matches(info: &ToolInfo, m: &ArgMatches) -> Result<(Value, Output), String> {
    let mut params = Map::new();
    let mut output = Output::Message;
    if m.contains_id("json") {
        match m.get_one::<String>("json") {
            Some(raw) => params = read_json_params(raw)?,
            None => output = Output::Json,
        }
    }
    for (name, prop) in props(&info.input_schema) {
        if RESERVED.contains(&flag_name(&name).as_str()) {
            continue;
        }
        if let Some(raw) = m.get_one::<String>(&name) {
            let ty = prop_type(&info.input_schema, &prop);
            params.insert(name.clone(), convert(&ty, &flag_name(&name), raw)?);
        }
    }
    let missing: Vec<String> = required(&info.input_schema).into_iter().filter(|r| params.get(r).is_none_or(Value::is_null)).collect();
    if !missing.is_empty() {
        let flags: Vec<String> = missing.iter().map(|n| format!("--{}", flag_name(n))).collect();
        return Err(format!("{} needs {}", info.cli_name(), flags.join(", ")));
    }
    Ok((Value::Object(params), output))
}

/// The whole CLI: fixed commands, plus one hidden subcommand per tool. Tools are listed by toolset
/// in the help footer instead of clap's flat "Commands" list.
pub fn build(tools: &[ToolInfo], note: Option<&str>) -> Command {
    let mut footer = String::new();
    for set in csm_engine::Toolset::ALL {
        let mine: Vec<&ToolInfo> = tools.iter().filter(|t| t.toolset == set).collect();
        if mine.is_empty() {
            continue;
        }
        footer.push_str(&format!("{} ({}):\n", set.title(), set.id()));
        let width = mine.iter().map(|t| t.cli_name().len()).max().unwrap_or(0);
        for t in mine {
            footer.push_str(&format!("  {:width$}  {}\n", t.cli_name(), t.summary));
        }
        footer.push('\n');
    }
    if let Some(n) = note {
        footer.push_str(n);
        footer.push('\n');
    }
    footer.push_str("Run `csm <tool> --help` for a tool's options. Exit codes: 0 ok, 1 tool error, 2 usage, 3 app not reachable.");

    let mut cmd = Command::new("csm")
        .about("CodeShellManager on the command line: every tool the app has, and its MCP server")
        .after_help(footer)
        .arg(Arg::new("g_start").long("start").global(true).action(ArgAction::SetTrue).help("Start CodeShellManager if it is not running"))
        .arg(Arg::new("g_app").long("app").global(true).value_name("PATH").help("The app to start with --start (default: $CSM_APP)"))
        .arg(
            Arg::new("g_in_process")
                .long("in-process")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Run an engine inside this process instead of talking to the app (no sessions of yours, for tests)"),
        )
        .subcommand(
            Command::new("mcp")
                .about("Serve the tools over MCP on stdio (for `claude mcp add`)")
                .arg(Arg::new("session").long("session").value_name("ID").help("The CodeShellManager session this server runs inside"))
                .arg(Arg::new("all_tools").long("all-tools").action(ArgAction::SetTrue).help("List every tool, ignoring toolsets"))
                .arg(
                    Arg::new("auto_allow")
                        .long("auto-allow")
                        .action(ArgAction::SetTrue)
                        .help("With --in-process only: allow every consent request. There is no app to ask."),
                ),
        )
        .subcommand(
            Command::new("tools")
                .visible_alias("commands")
                .about("List the tools")
                .arg(Arg::new("toolset").long("toolset").value_name("SET").help("Only this toolset"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue).help("Print JSON")),
        )
        .subcommand(
            Command::new("events")
                .about("Print engine events from the running app as JSON lines until interrupted"),
        )
        .subcommand(
            Command::new("setup")
                .about("Connect `csm mcp` to Claude Code, Claude Desktop, Codex and Cursor (comments kept, backup made, verified)")
                .arg(client_arg())
                .arg(Arg::new("command").long("command").value_name("PATH").help("The csm program to register (default: this one)"))
                .arg(Arg::new("dry_run").long("dry-run").action(ArgAction::SetTrue).help("Show the exact change and write nothing"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue).help("Print JSON")),
        )
        .subcommand(
            Command::new("doctor")
                .about("Check the install and print the fix for anything wrong")
                .arg(Arg::new("skip_self_test").long("skip-self-test").action(ArgAction::SetTrue).help("Do not start `csm mcp --in-process` to test it"))
                .arg(Arg::new("skip_signature").long("skip-signature").action(ArgAction::SetTrue).help("Do not check the ConPTY files' signatures (starts PowerShell)"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue).help("Print JSON")),
        )
        .subcommand(
            Command::new("uninstall")
                .about("Remove the entries `csm setup` added (and only those)")
                .arg(client_arg())
                .arg(Arg::new("dry_run").long("dry-run").action(ArgAction::SetTrue).help("Show what would be removed"))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue).help("Print JSON")),
        );
    for t in tools {
        cmd = cmd.subcommand(tool_command(t));
    }
    cmd
}

fn client_arg() -> Arg {
    Arg::new("client")
        .long("client")
        .value_name("ID")
        .action(ArgAction::Append)
        .help("An app: claude_code, claude_desktop, codex, cursor (repeatable; default: the ones found)")
}

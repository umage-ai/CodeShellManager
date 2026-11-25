//! The `csm` runner: pick a backend, learn the tools from it, parse the command line against them,
//! dispatch.

use std::io::Write;
use std::path::PathBuf;

use clap::ArgMatches;
use csm_engine::{Caller, Engine, Event, State, ToolSpec, Toolset};
use csm_mcp::{Backend, BridgeError, LocalBackend, McpOptions, McpService, RemoteBackend, RemoteOptions, RuntimeDir, ToolInfo};
use serde_json::{Value, json};

use crate::args::{self, Output};
use crate::setup_cmd;

pub const EXIT_OK: i32 = 0;
/// The tool ran and failed, or the engine refused it.
pub const EXIT_TOOL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
/// CodeShellManager could not be reached (and `--start` was not given, or failed).
pub const EXIT_APP: i32 = 3;

/// Things read from the environment, passed in so tests can set them without touching the process.
#[derive(Debug, Clone, Default)]
pub struct Env {
    /// `CSM_RUNTIME_DIR`: where the app's instance record lives (default: the per-user directory).
    pub runtime_dir: Option<PathBuf>,
    /// `CSM_APP`: the app binary `--start` launches.
    pub app: Option<PathBuf>,
    /// `CSM_INSTANCE`: the instance to use (a pid, or its state file); `--instance` wins.
    pub instance: Option<String>,
}

impl Env {
    pub fn from_process() -> Env {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        Env { runtime_dir: var("CSM_RUNTIME_DIR"), app: var("CSM_APP"), instance: var("CSM_INSTANCE").map(|p| p.to_string_lossy().into_owned()) }
    }
}

/// The tools an engine started inside this process has: those of the app. Until the engine's tool
/// modules are wired in here this is empty, and `--in-process` has only the built-in tools; the
/// running app is the normal way to use `csm`.
fn in_process_tools() -> Vec<ToolSpec> {
    // The engine registers all of its own tools; this is only for tools from other crates.
    Vec::new()
}

/// What the command line says before clap has seen it: which backend, and whether the command
/// needs the tool list at all.
#[derive(Debug, Default)]
struct Pre {
    start: bool,
    app: Option<String>,
    instance: Option<String>,
    in_process: bool,
    first: Option<String>,
    help: bool,
}

fn prescan(argv: &[String]) -> Pre {
    let mut pre = Pre::default();
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--start" => pre.start = true,
            "--in-process" => pre.in_process = true,
            "--help" | "-h" => pre.help = true,
            "--app" => pre.app = it.next().cloned(),
            // Before the command only: after it, `--instance` may be the tool's own parameter.
            "--instance" if pre.first.is_none() => pre.instance = it.next().cloned(),
            s if pre.first.is_none() && s.starts_with("--instance=") => pre.instance = Some(s["--instance=".len()..].to_string()),
            s if s.starts_with("--app=") => pre.app = Some(s["--app=".len()..].to_string()),
            // The value of another option, or its flag: only a bare word can be the command.
            s if s.starts_with('-') => {
                if pre.first.is_some() {
                    continue;
                }
            }
            s => {
                if pre.first.is_none() {
                    pre.first = Some(s.to_string());
                }
            }
        }
    }
    pre
}

enum Mode {
    InProcess(Engine),
    Remote,
}

fn make_backend(pre: &Pre, env: &Env) -> Result<(Box<dyn Backend>, Mode), String> {
    if pre.in_process {
        let engine = Engine::start(State::default(), in_process_tools()).map_err(|e| e.to_string())?;
        return Ok((Box::new(LocalBackend::new(engine.clone())), Mode::InProcess(engine)));
    }
    let runtime = match &env.runtime_dir {
        Some(p) => Some(RuntimeDir::at(p).map_err(|e| format!("cannot use the runtime directory {}: {e}", p.display()))?),
        None => None,
    };
    let options = RemoteOptions {
        runtime,
        start: pre.start,
        app_path: pre.app.as_ref().map(PathBuf::from).or_else(|| env.app.clone()),
        tcp: None,
        instance: pre.instance.clone().or_else(|| env.instance.clone()),
        app_args: Vec::new(),
    };
    Ok((Box::new(RemoteBackend::new(options)), Mode::Remote))
}

/// The tools this build of csm knows without the app: the engine's built-in ones plus whatever
/// [`in_process_tools`] adds. Used for help and `csm tools` when the app is not running.
fn builtin_tools() -> Vec<ToolInfo> {
    let Ok(engine) = Engine::start(State::default(), in_process_tools()) else { return Vec::new() };
    let tools = LocalBackend::new(engine.clone()).tools().unwrap_or_default();
    engine.shutdown();
    tools
}

fn print_error(e: &BridgeError, as_json: bool) {
    if as_json {
        let error = match e {
            BridgeError::Tool(t) => csm_mcp::wire::encode_error(t),
            BridgeError::Unreachable(m) => json!({"code": "unreachable", "message": m}),
        };
        println!("{}", json!({"error": error}));
    } else {
        eprintln!("error: {e}");
    }
}

fn exit_for(e: &BridgeError) -> i32 {
    match e {
        BridgeError::Tool(_) => EXIT_TOOL,
        BridgeError::Unreachable(_) => EXIT_APP,
    }
}

/// Run `csm` with `argv` (including the program name). Returns the exit code.
pub fn run(argv: &[String], env: &Env) -> i32 {
    let pre = prescan(argv);
    let (mut backend, mode) = match make_backend(&pre, env) {
        Ok(b) => b,
        Err(m) => {
            eprintln!("error: {m}");
            return EXIT_APP;
        }
    };

    // The commands are the registry's, so ask it, unless the command does not use them.
    let needs_tools = !matches!(pre.first.as_deref(), Some("mcp" | "events" | "setup" | "doctor" | "uninstall" | "attach"));
    let listing = pre.help || matches!(pre.first.as_deref(), None | Some("tools" | "commands" | "help"));
    let mut note = None;
    let tools = if needs_tools {
        match backend.tools() {
            Ok(t) => t,
            Err(BridgeError::Unreachable(m)) if listing => {
                note = Some(format!("(Not connected: {m} Showing the tools this csm build knows.)"));
                builtin_tools()
            }
            Err(e) => {
                print_error(&e, false);
                return exit_for(&e);
            }
        }
    } else {
        Vec::new()
    };

    let mut cmd = args::build(&tools, note.as_deref());
    let matches = match cmd.try_get_matches_from_mut(argv) {
        Ok(m) => m,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_USAGE } else { EXIT_OK };
        }
    };
    match matches.subcommand() {
        None => {
            let _ = cmd.print_help();
            EXIT_OK
        }
        Some(("mcp", m)) => run_mcp(m, backend, &mode),
        Some(("tools", m)) => run_tools(m, &tools, note.as_deref()),
        Some(("events", _)) => run_events(&pre, env),
        Some(("setup", m)) => setup_cmd::run_setup(m),
        Some(("doctor", m)) => setup_cmd::run_doctor(m, env),
        Some(("uninstall", m)) => setup_cmd::run_uninstall(m),
        Some(("attach", m)) => crate::attach_cmd::run(m),
        Some((name, m)) => match tools.iter().find(|t| t.cli_name() == name) {
            Some(info) => run_tool(info, m, backend.as_mut()),
            None => {
                eprintln!("error: no command `{name}`");
                EXIT_USAGE
            }
        },
    }
}

fn run_tool(info: &ToolInfo, m: &ArgMatches, backend: &mut dyn Backend) -> i32 {
    let (params, output) = match args::params_from_matches(info, m) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("error: {msg}");
            return EXIT_USAGE;
        }
    };
    match backend.call(&Caller::Cli, &info.id, params) {
        Ok(out) => {
            match output {
                Output::Message => println!("{}", human_text(&info.id, &out.message, &out.data)),
                Output::Json => {
                    let v = json!({"message": out.message, "data": out.data});
                    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                }
            }
            EXIT_OK
        }
        Err(e) => {
            print_error(&e, matches!(output, Output::Json));
            exit_for(&e)
        }
    }
}

/// What a tool prints without `--json`: its message, or, for the tools that read text (the
/// screen, the scrollback, a run's output, a note), that text itself. Their message only counts
/// it ("24 lines"), which is not what `csm session-read-screen` is asked for.
pub fn human_text(tool_id: &str, message: &str, data: &Value) -> String {
    let field = match tool_id {
        "session_read_screen" | "session_read_output" | "note_get" => "text",
        "run_output" => "output",
        _ => return message.to_owned(),
    };
    match data.get(field).and_then(Value::as_str) {
        // A run with no output yet: its message says why ("Build has not run yet.").
        Some("") if tool_id == "run_output" => message.to_owned(),
        // `println!` adds the final newline.
        Some(text) => text.strip_suffix('\n').unwrap_or(text).to_owned(),
        None => message.to_owned(),
    }
}

fn run_tools(m: &ArgMatches, tools: &[ToolInfo], note: Option<&str>) -> i32 {
    let only = match m.get_one::<String>("toolset") {
        None => None,
        Some(s) => match Toolset::from_id(s) {
            Some(t) => Some(t),
            None => {
                let valid: Vec<&str> = Toolset::ALL.iter().map(|t| t.id()).collect();
                eprintln!("error: no toolset `{s}`. Toolsets: {}", valid.join(", "));
                return EXIT_USAGE;
            }
        },
    };
    let shown: Vec<&ToolInfo> = tools.iter().filter(|t| only.is_none_or(|o| t.toolset == o)).collect();
    if m.get_flag("json") {
        let list: Vec<Value> = shown
            .iter()
            .map(|t| {
                json!({
                    "id": t.id, "command": t.cli_name(), "toolset": t.toolset.id(), "title": t.title, "summary": t.summary,
                    "effects": serde_json::to_value(t.effects).unwrap_or(Value::Null), "input_schema": t.input_schema,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&list).unwrap_or_default());
        return EXIT_OK;
    }
    let width = shown.iter().map(|t| t.cli_name().len()).max().unwrap_or(0);
    for set in Toolset::ALL {
        let mine: Vec<&&ToolInfo> = shown.iter().filter(|t| t.toolset == set).collect();
        if mine.is_empty() {
            continue;
        }
        println!("{} ({})", set.title(), set.id());
        for t in mine {
            println!("  {:width$}  {}", t.cli_name(), t.summary);
        }
    }
    if let Some(n) = note {
        eprintln!("{n}");
    }
    EXIT_OK
}

fn run_events(pre: &Pre, env: &Env) -> i32 {
    if pre.in_process {
        eprintln!("error: events come from the running app; --in-process has none");
        return EXIT_USAGE;
    }
    // `watch` needs the concrete type, so build it here rather than through `make_backend`.
    let runtime = env.runtime_dir.as_ref().and_then(|p| RuntimeDir::at(p).ok());
    let mut remote = RemoteBackend::new(RemoteOptions {
        runtime,
        start: pre.start,
        app_path: pre.app.as_ref().map(PathBuf::from).or_else(|| env.app.clone()),
        tcp: None,
        instance: pre.instance.clone().or_else(|| env.instance.clone()),
        app_args: Vec::new(),
    });
    let result = remote.watch(|ev| {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{ev}").and_then(|()| out.flush()).is_ok()
    });
    match result {
        Ok(()) => EXIT_OK,
        Err(e) => {
            print_error(&e, false);
            exit_for(&e)
        }
    }
}

fn run_mcp(m: &ArgMatches, backend: Box<dyn Backend>, mode: &Mode) -> i32 {
    let auto_allow = m.get_flag("auto_allow");
    match (auto_allow, mode) {
        (true, Mode::InProcess(engine)) => spawn_auto_allow(engine),
        (true, Mode::Remote) => {
            eprintln!("error: --auto-allow only applies with --in-process; with the app, answer the consent card in the app");
            return EXIT_USAGE;
        }
        (false, _) => {}
    }
    let options = McpOptions { session_id: m.get_one::<String>("session").cloned(), all_tools: m.get_flag("all_tools") };
    let mut service = McpService::new(backend, options);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match service.serve(stdin.lock(), stdout) {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_TOOL
        }
    }
}

/// `--in-process --auto-allow`: there is no app to show a consent card, so the person's flag stands
/// in for the click. Never the default, and never available against the real app.
fn spawn_auto_allow(engine: &Engine) {
    let events = engine.subscribe();
    let engine = engine.clone();
    let _ = std::thread::Builder::new().name("csm-auto-allow".into()).spawn(move || {
        while let Ok(ev) = events.recv() {
            if let Event::ConsentRequested { request } = ev {
                let params = json!({"request_id": request["request_id"], "answer": "allow_once"});
                let _ = engine.call(Caller::Ui, "consent_answer", params);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::human_text;
    use serde_json::json;

    #[test]
    fn reading_tools_print_their_text_not_the_count() {
        let data = json!({"session_id": "s", "text": "PS C:\\> dir\nhello\n"});
        assert_eq!(human_text("session_read_screen", "2 lines", &data), "PS C:\\> dir\nhello");
        assert_eq!(human_text("session_read_output", "2 lines", &data), "PS C:\\> dir\nhello");
        assert_eq!(human_text("run_output", "Build — 3 characters.", &json!({"output": "ok\n"})), "ok");
        assert_eq!(human_text("note_get", "Note", &json!({"text": "todo"})), "todo");
        assert_eq!(human_text("run_output", "Build has not run yet.", &json!({"output": ""})), "Build has not run yet.");
        // An empty screen prints an empty line, not "0 lines".
        assert_eq!(human_text("session_read_screen", "0 lines", &json!({"text": ""})), "");
    }

    #[test]
    fn other_tools_and_odd_replies_print_the_message() {
        assert_eq!(human_text("session_list", "3 sessions", &json!({"text": "x"})), "3 sessions");
        // A reply without the text (a parked "reading" answer, a wrong shape) falls back.
        assert_eq!(human_text("session_read_screen", "reading", &json!({})), "reading");
        assert_eq!(human_text("session_read_screen", "2 lines", &json!({"text": 5})), "2 lines");
    }
}

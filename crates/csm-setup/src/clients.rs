//! The apps we can connect to: where their config lives, how to recognise them, and the shape of
//! the registration entry each expects.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Value, json};

/// The name our server is registered under in every client.
pub const SERVER_NAME: &str = "codeshell";

/// Environment variables that affect where clients keep their config. Only these are read.
pub const ENV_KEYS: &[&str] = &["XDG_CONFIG_HOME", "APPDATA", "CLAUDE_CONFIG_DIR", "CODEX_HOME"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Linux,
    Macos,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        match std::env::consts::OS {
            "macos" => Os::Macos,
            "windows" => Os::Windows,
            _ => Os::Linux,
        }
    }
}

/// The machine being configured. Injectable so tests use a temporary home.
#[derive(Debug, Clone)]
pub struct Host {
    pub home: PathBuf,
    pub env: BTreeMap<String, String>,
    pub os: Os,
}

impl Host {
    /// The real machine: the user's home and only the config-related environment variables.
    /// On Windows the apps keep settings in the user profile and ignore `HOME` (which MSYS2 or
    /// Cygwin point elsewhere), so `USERPROFILE` comes first there.
    pub fn current() -> Host {
        let (first, second) = if cfg!(windows) { ("USERPROFILE", "HOME") } else { ("HOME", "USERPROFILE") };
        let home = std::env::var_os(first)
            .filter(|h| !h.is_empty())
            .or_else(|| std::env::var_os(second).filter(|h| !h.is_empty()))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let env = ENV_KEYS.iter().filter_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()).map(|v| ((*k).to_owned(), v))).collect();
        Host { home, env, os: Os::current() }
    }

    /// A host rooted at `home` with no environment overrides (tests).
    pub fn with_home(home: impl Into<PathBuf>) -> Host {
        Host { home: home.into(), env: BTreeMap::new(), os: Os::current() }
    }

    pub fn var(&self, k: &str) -> Option<PathBuf> {
        self.env.get(k).filter(|v| !v.is_empty()).map(|v| self.expand(v))
    }

    /// A leading `~` is the home folder; a relative path is relative to it.
    fn expand(&self, v: &str) -> PathBuf {
        if v == "~" {
            return self.home.clone();
        }
        if let Some(rest) = v.strip_prefix("~/") {
            return self.home.join(rest);
        }
        let p = PathBuf::from(v);
        if p.is_absolute() { p } else { self.home.join(p) }
    }

    /// Per-user application data (Windows `%APPDATA%`, macOS Application Support, Linux XDG config).
    pub fn app_data(&self) -> PathBuf {
        match self.os {
            Os::Windows => self.var("APPDATA").unwrap_or_else(|| self.home.join("AppData").join("Roaming")),
            Os::Macos => self.home.join("Library").join("Application Support"),
            Os::Linux => self.var("XDG_CONFIG_HOME").unwrap_or_else(|| self.home.join(".config")),
        }
    }
}

/// How the config file is parsed and edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// JSON with comments and trailing commas tolerated (and preserved).
    Jsonc,
    Toml,
}

/// The registration entry's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// `{command, args}`
    Plain,
    /// `{type: "stdio", command, args}`
    Stdio,
    /// `{command, args, startup_timeout_sec: 30}`
    WithStartupTimeout,
}

/// One supported client, resolved for a host.
#[derive(Debug, Clone, Serialize)]
pub struct Client {
    pub id: &'static str,
    pub display: &'static str,
    /// The file we write.
    pub config: PathBuf,
    /// Paths whose existence means the client is installed.
    pub markers: Vec<PathBuf>,
    pub format: Format,
    /// Key path of the servers table, e.g. `["mcpServers"]`.
    pub table: Vec<&'static str>,
    pub shape: Shape,
}

impl Client {
    /// Whether `name` (an id, with `-` or `_`) means this client.
    pub fn is(&self, name: &str) -> bool {
        name.replace('-', "_").eq_ignore_ascii_case(self.id)
    }

    pub fn detected(&self) -> bool {
        self.config.exists() || self.markers.iter().any(|m| m.exists())
    }

    /// The table path plus our server name.
    pub fn entry_path(&self) -> Vec<&str> {
        let mut p: Vec<&str> = self.table.clone();
        p.push(SERVER_NAME);
        p
    }

    /// The entry this client should hold for `command` (program followed by args).
    pub fn desired_entry(&self, command: &[String]) -> Value {
        let (program, args) = match command.split_first() {
            Some((p, a)) => (p.clone(), a.to_vec()),
            None => (String::new(), Vec::new()),
        };
        match self.shape {
            Shape::Plain => json!({"command": program, "args": args}),
            Shape::Stdio => json!({"type": "stdio", "command": program, "args": args}),
            Shape::WithStartupTimeout => json!({"command": program, "args": args, "startup_timeout_sec": 30}),
        }
    }
}

/// Every supported client for `host`, in the order setup lists them.
pub fn clients(host: &Host) -> Vec<Client> {
    let claude_dir = host.var("CLAUDE_CONFIG_DIR").unwrap_or_else(|| host.home.join(".claude"));
    // Claude Code keeps its user config in `<config dir>/.claude.json`; without CLAUDE_CONFIG_DIR
    // that file sits in the home folder, not inside `.claude`.
    let claude_json = match host.var("CLAUDE_CONFIG_DIR") {
        Some(d) => d.join(".claude.json"),
        None => host.home.join(".claude.json"),
    };
    let codex_dir = host.var("CODEX_HOME").unwrap_or_else(|| host.home.join(".codex"));
    let desktop_dir = host.app_data().join("Claude");
    vec![
        Client {
            id: "claude_code",
            display: "Claude Code",
            config: claude_json,
            markers: vec![claude_dir],
            format: Format::Jsonc,
            table: vec!["mcpServers"],
            shape: Shape::Stdio,
        },
        Client {
            id: "claude_desktop",
            display: "Claude Desktop",
            config: desktop_dir.join("claude_desktop_config.json"),
            markers: vec![desktop_dir],
            format: Format::Jsonc,
            table: vec!["mcpServers"],
            shape: Shape::Plain,
        },
        Client {
            id: "codex",
            display: "Codex",
            config: codex_dir.join("config.toml"),
            markers: vec![codex_dir],
            format: Format::Toml,
            table: vec!["mcp_servers"],
            shape: Shape::WithStartupTimeout,
        },
        Client {
            id: "cursor",
            display: "Cursor",
            config: host.home.join(".cursor").join("mcp.json"),
            markers: vec![host.home.join(".cursor")],
            format: Format::Jsonc,
            table: vec!["mcpServers"],
            shape: Shape::Plain,
        },
    ]
}

/// Whether `entry` is one setup wrote: `csm mcp` (the program is `csm`, the first argument `mcp`).
/// An entry the person made by hand under the same name is left alone by `uninstall`.
pub fn is_owned_entry(entry: &Value) -> bool {
    let command = entry.get("command").and_then(Value::as_str).unwrap_or("");
    // Either path style, on every OS: a config can hold a Windows path read on Unix (synced
    // dotfiles, WSL) and `Path` only splits on the host's separator.
    let name = command.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(name);
    let first_arg = entry.get("args").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str);
    stem.eq_ignore_ascii_case("csm") && first_arg == Some("mcp")
}

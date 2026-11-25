//! Starting the app when the person signs in to the OS ("Start CodeShellManager when I sign in").
//!
//! The OS is the source of truth: the setting is whatever the OS's own autostart entry says, read
//! back every time, never a copy in `state.json`. One entry per user:
//!
//! - **Windows:** `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value [`ENTRY_NAME`] =
//!   `"<exe>" --autostart`. Task Manager's Startup apps tab can turn it off without deleting it
//!   (`...\Explorer\StartupApproved\Run`, an odd first byte means disabled); that reads back as
//!   [`Status::disabled_by_os`], and turning the setting on here clears it.
//! - **macOS:** `~/Library/LaunchAgents/ai.umage.CodeShellManager.plist` with `ProgramArguments`
//!   `[exe, --autostart]` and `RunAtLoad`.
//! - **Linux:** `$XDG_CONFIG_HOME/autostart/ai.umage.CodeShellManager.desktop` (default
//!   `~/.config/autostart`) with `Exec=<exe> --autostart`, escaped per the desktop-entry spec.
//!   `Hidden=true` or `X-GNOME-Autostart-enabled=false` read back as disabled.
//!
//! The command line, plist and desktop entry are built here from the exe's path (never from text a
//! person typed), each with its format's quoting. The generators and parsers are pure; the OS
//! access is behind [`Backend`] (and, on Windows, [`Registry`]) so tests never touch the real
//! registry or the person's autostart folders. These functions do I/O: call them from the engine
//! or at start-up, never from frame code.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

use crate::argv::quote_for_cmd;

/// The Windows `Run` value's name for the stable channel. It stays the WPF app's name so a stable
/// install keeps (and upgrades) the entry the WPF app wrote.
pub const ENTRY_NAME: &str = "CodeShellManager";
/// The beta channel's own `Run` value, so a beta never reads, rewrites or removes the stable app's
/// entry, and the reverse.
pub const BETA_ENTRY_NAME: &str = "CodeShellManager-Beta";

/// The `Run` value name a channel uses.
pub const fn entry_name_for(channel: csm_core::channel::Channel) -> &'static str {
    match channel {
        csm_core::channel::Channel::Stable => ENTRY_NAME,
        csm_core::channel::Channel::Beta => BETA_ENTRY_NAME,
    }
}
/// The launchd label and the desktop entry's file stem (the app id the Linux packages use).
pub const LAUNCH_ID: &str = "ai.umage.CodeShellManager";
/// The argument the entry starts the app with, so a start at sign-in can be told apart in the log.
pub const AUTOSTART_ARG: &str = "--autostart";

/// `HKCU\...` paths, relative to `HKEY_CURRENT_USER`.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const APPROVED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// What the OS's autostart entry says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// This platform has an autostart entry this app can manage.
    pub supported: bool,
    /// The app starts at sign-in: the entry exists and the OS has not turned it off.
    pub enabled: bool,
    /// The entry exists (enabled or not).
    pub registered: bool,
    /// The entry exists but the OS turned it off (Task Manager's Startup apps on Windows,
    /// `Hidden=true` on Linux, `Disabled` on macOS).
    pub disabled_by_os: bool,
    /// The program the entry starts, when it could be read.
    pub program: Option<String>,
    /// The entry starts this very exe.
    pub current: bool,
    /// Where the entry lives (a registry value or a file), for messages.
    pub location: String,
}

/// One entry, as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The program it starts; `None` when the entry could not be parsed.
    pub program: Option<PathBuf>,
    pub disabled_by_os: bool,
}

/// Why an entry could not be written.
#[derive(Debug)]
pub enum Error {
    /// The exe's path can't be written into this format (not absolute, not Unicode, or it holds
    /// a quote or a control character).
    BadPath(String),
    Io(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadPath(why) => f.write_str(why),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// Where one platform keeps the entry.
pub trait Backend: Send + Sync {
    /// Where the entry lives (`HKCU\...\Run\CodeShellManager`, a file path).
    fn location(&self) -> String;
    fn read(&self) -> io::Result<Option<Entry>>;
    /// Writes (or rewrites) the entry to start `program`, enabled.
    fn write(&self, program: &Path) -> Result<(), Error>;
    /// Removes the entry; no error when there is none.
    fn remove(&self) -> io::Result<()>;
    /// Whether two program paths name the same file (Windows paths ignore case).
    fn same_program(&self, a: &Path, b: &Path) -> bool {
        a == b
    }
}

// ── The operations ─────────────────────────────────────────────────────────────────────────────

/// What the OS says, compared with `exe` (this app).
pub fn status(b: &dyn Backend, exe: &Path) -> io::Result<Status> {
    let entry = b.read()?;
    let program = entry.as_ref().and_then(|e| e.program.clone());
    let disabled = entry.as_ref().is_some_and(|e| e.disabled_by_os);
    Ok(Status {
        supported: true,
        enabled: entry.is_some() && !disabled,
        registered: entry.is_some(),
        disabled_by_os: disabled,
        current: program.as_deref().is_some_and(|p| b.same_program(p, exe)),
        program: program.map(|p| p.display().to_string()),
        location: b.location(),
    })
}

/// Turns starting at sign-in on (an entry for `exe`, enabled even where the OS had turned it
/// off) or off (no entry), and returns what the OS says afterwards.
pub fn set(b: &dyn Backend, exe: &Path, enabled: bool) -> Result<Status, Error> {
    if enabled {
        b.write(exe)?;
    } else {
        b.remove()?;
    }
    Ok(status(b, exe)?)
}

/// The start-up fix-up: an entry that starts another exe (the app moved, or was upgraded into a
/// new folder) is rewritten to start `exe`, keeping whether the OS had turned it off. With
/// `only_if_missing` the entry is left alone while the program it names still exists: a copy
/// of the app (a portable folder, a development build) must not take over a working entry. The
/// entry is per channel (see [`entry_name_for`]), so the other channel's value is never touched. Returns the program the
/// entry named before, when it was rewritten.
pub fn refresh(b: &dyn Backend, exe: &Path, only_if_missing: bool) -> Result<Option<PathBuf>, Error> {
    let Some(entry) = b.read()? else { return Ok(None) };
    if entry.program.as_deref().is_some_and(|p| b.same_program(p, exe)) {
        return Ok(None);
    }
    if only_if_missing && entry.program.as_deref().is_some_and(Path::exists) {
        return Ok(None);
    }
    // Rewriting must not turn back on what the person turned off in the OS.
    if entry.disabled_by_os {
        return Ok(None);
    }
    b.write(exe)?;
    Ok(Some(entry.program.unwrap_or_default()))
}

/// The uninstaller's clean-up: removes the entry when it starts `exe` or a program that no longer
/// exists, and leaves another install's entry alone. True when something was removed.
pub fn remove_if_ours(b: &dyn Backend, exe: &Path) -> io::Result<bool> {
    let Some(entry) = b.read()? else { return Ok(false) };
    let ours = match entry.program.as_deref() {
        Some(p) => b.same_program(p, exe) || !p.exists(),
        None => true,
    };
    if ours {
        b.remove()?;
    }
    Ok(ours)
}

/// The program an entry should start: this exe; on Linux an AppImage's own file (`$APPIMAGE`),
/// not the temporary mount the exe runs from. Reads the environment: start-up only.
pub fn current_program() -> Option<PathBuf> {
    if cfg!(target_os = "linux")
        && let Some(p) = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_absolute())
    {
        return Some(p);
    }
    let exe = std::env::current_exe().ok()?;
    // A verbatim `\\?\C:\...` path works for the process but not for every reader of a Run value.
    let s = exe.to_str()?;
    Some(match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => exe,
    })
}

/// This platform's backend over the real OS: the current user's registry, LaunchAgents folder or
/// XDG autostart folder. `None` where there is none (no home folder; another OS).
pub fn system() -> Option<Box<dyn Backend>> {
    #[cfg(windows)]
    {
        Some(Box::new(WindowsRun::new(CurrentUserRegistry)))
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute())?;
        Some(Box::new(LaunchAgent::new(home.join("Library").join("LaunchAgents"))))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute()).map(|h| h.join(".config")))?;
        Some(Box::new(XdgAutostart::new(config.join("autostart"))))
    }
    #[cfg(not(any(windows, unix)))]
    {
        None
    }
}

fn utf8_path<'a>(exe: &'a Path, format: &str) -> Result<&'a str, Error> {
    let s = exe.to_str().ok_or_else(|| Error::BadPath(format!("{} is not valid Unicode, so it can't go in {format}", exe.display())))?;
    if !exe.is_absolute() {
        return Err(Error::BadPath(format!("{s} is not an absolute path")));
    }
    if s.chars().any(char::is_control) {
        return Err(Error::BadPath(format!("{} holds a control character", exe.display())));
    }
    Ok(s)
}

// ── Windows: the Run key ───────────────────────────────────────────────────────────────────────

/// The `Run` value for `exe`: the path in double quotes (the program-name token runs from quote
/// to quote, so spaces and any other character are literal), then [`AUTOSTART_ARG`]. A path
/// holding `"` can't be quoted that way and is refused (Windows forbids it in file names anyway).
pub fn windows_command_line(exe: &Path) -> Result<String, Error> {
    let s = utf8_path(exe, "a command line")?;
    if s.contains('"') {
        return Err(Error::BadPath(format!("{s} holds a quote")));
    }
    if s.ends_with('\\') {
        return Err(Error::BadPath(format!("{s} is a folder, not a program")));
    }
    Ok(format!("{} {AUTOSTART_ARG}", quote_for_cmd(s, true)))
}

/// The program a `Run` value starts, by the program-name rules: quote to quote when it starts
/// with `"`, otherwise up to the first space or tab. `None` for an empty value.
pub fn windows_program(command_line: &str) -> Option<PathBuf> {
    let s = command_line.trim_start_matches([' ', '\t']);
    let program = match s.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => s.split([' ', '\t']).next().unwrap_or(s),
    };
    (!program.is_empty()).then(|| PathBuf::from(program))
}

/// Task Manager's `StartupApproved` data for an entry: 12 bytes, the first even when the entry
/// is enabled (`02`, `06`) and odd when the person turned it off (`03`, `01`, `07`).
pub fn startup_approved_disabled(data: &[u8]) -> bool {
    data.first().is_some_and(|b| b & 1 == 1)
}

/// The registry, as far as the `Run` entry needs it. Keys are relative to `HKEY_CURRENT_USER`.
pub trait Registry: Send + Sync {
    /// A string value; `None` when the key or value doesn't exist.
    fn get_string(&self, key: &str, name: &str) -> io::Result<Option<String>>;
    /// A value's raw bytes; `None` when the key or value doesn't exist.
    fn get_binary(&self, key: &str, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// Writes a `REG_SZ` value, creating the key if needed.
    fn set_string(&self, key: &str, name: &str, value: &str) -> io::Result<()>;
    /// Deletes a value; no error when it doesn't exist.
    fn delete(&self, key: &str, name: &str) -> io::Result<()>;
}

/// The `Run` key entry over a [`Registry`].
pub struct WindowsRun<R: Registry> {
    reg: R,
    name: String,
}

impl<R: Registry> WindowsRun<R> {
    /// The entry of this build's channel.
    pub fn new(reg: R) -> Self {
        Self::with_name(reg, entry_name_for(csm_core::channel::Channel::current()))
    }

    /// An entry under an explicit value name (tests, and the other channel's entry).
    pub fn with_name(reg: R, name: &str) -> Self {
        WindowsRun { reg, name: name.to_string() }
    }

    pub fn registry(&self) -> &R {
        &self.reg
    }
}

impl<R: Registry> Backend for WindowsRun<R> {
    fn location(&self) -> String {
        format!(r"HKEY_CURRENT_USER\{RUN_KEY}\{}", self.name)
    }

    fn read(&self) -> io::Result<Option<Entry>> {
        let Some(line) = self.reg.get_string(RUN_KEY, &self.name)? else { return Ok(None) };
        // An unreadable approval is "not turned off": the Run value alone decides.
        let disabled = self.reg.get_binary(APPROVED_KEY, &self.name).ok().flatten().is_some_and(|d| startup_approved_disabled(&d));
        Ok(Some(Entry { program: windows_program(&line), disabled_by_os: disabled }))
    }

    fn write(&self, program: &Path) -> Result<(), Error> {
        let line = windows_command_line(program)?;
        self.reg.set_string(RUN_KEY, &self.name, &line)?;
        // Turned on here: whatever Task Manager said before no longer holds.
        self.reg.delete(APPROVED_KEY, &self.name)?;
        Ok(())
    }

    fn remove(&self) -> io::Result<()> {
        self.reg.delete(RUN_KEY, &self.name)?;
        self.reg.delete(APPROVED_KEY, &self.name)
    }

    fn same_program(&self, a: &Path, b: &Path) -> bool {
        let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
        norm(a) == norm(b)
    }
}

/// A registry in memory: tests, and the default of an engine that must not touch the real one.
#[derive(Debug, Default)]
pub struct MemoryRegistry {
    values: Mutex<std::collections::BTreeMap<(String, String), MemValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemValue {
    String(String),
    Binary(Vec<u8>),
}

impl MemoryRegistry {
    fn map(&self) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<(String, String), MemValue>> {
        self.values.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sets a raw value (what Task Manager writes to `StartupApproved`).
    pub fn put(&self, key: &str, name: &str, value: MemValue) {
        self.map().insert((key.to_lowercase(), name.to_lowercase()), value);
    }

    pub fn value(&self, key: &str, name: &str) -> Option<MemValue> {
        self.map().get(&(key.to_lowercase(), name.to_lowercase())).cloned()
    }
}

impl Registry for MemoryRegistry {
    fn get_string(&self, key: &str, name: &str) -> io::Result<Option<String>> {
        match self.value(key, name) {
            Some(MemValue::String(s)) => Ok(Some(s)),
            Some(MemValue::Binary(_)) => Err(io::Error::new(io::ErrorKind::InvalidData, "not a string value")),
            None => Ok(None),
        }
    }

    fn get_binary(&self, key: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(match self.value(key, name) {
            Some(MemValue::Binary(b)) => Some(b),
            Some(MemValue::String(s)) => Some(s.into_bytes()),
            None => None,
        })
    }

    fn set_string(&self, key: &str, name: &str, value: &str) -> io::Result<()> {
        self.put(key, name, MemValue::String(value.to_string()));
        Ok(())
    }

    fn delete(&self, key: &str, name: &str) -> io::Result<()> {
        self.map().remove(&(key.to_lowercase(), name.to_lowercase()));
        Ok(())
    }
}

/// The current user's real registry (`winreg`).
#[cfg(windows)]
pub struct CurrentUserRegistry;

#[cfg(windows)]
impl CurrentUserRegistry {
    fn open(key: &str) -> io::Result<Option<winreg::RegKey>> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        match winreg::RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(key, KEY_READ) {
            Ok(k) => Ok(Some(k)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(windows)]
impl Registry for CurrentUserRegistry {
    fn get_string(&self, key: &str, name: &str) -> io::Result<Option<String>> {
        let Some(k) = Self::open(key)? else { return Ok(None) };
        match k.get_value::<String, _>(name) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn get_binary(&self, key: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        let Some(k) = Self::open(key)? else { return Ok(None) };
        match k.get_raw_value(name) {
            Ok(v) => Ok(Some(v.bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn set_string(&self, key: &str, name: &str, value: &str) -> io::Result<()> {
        use winreg::enums::HKEY_CURRENT_USER;
        let (k, _) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(key)?;
        k.set_value(name, &value.to_string())
    }

    fn delete(&self, key: &str, name: &str) -> io::Result<()> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        let k = match winreg::RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(key, KEY_SET_VALUE) {
            Ok(k) => k,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        match k.delete_value(name) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

// ── Files (macOS, Linux) ───────────────────────────────────────────────────────────────────────

fn read_file(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(String::from_utf8_lossy(&b).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Writes through a temporary file beside it, so a crash never leaves half an entry.
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no folder"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn remove_file(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// `&`, `<`, `>`, `"` and `'` as XML entities.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(rest.get(..i).unwrap_or_default());
        let tail = rest.get(i..).unwrap_or_default();
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let name = tail.get(1..end).unwrap_or_default();
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            n => {
                let code = match n.strip_prefix("#x").or_else(|| n.strip_prefix("#X")) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.strip_prefix('#').and_then(|d| d.parse().ok()),
                };
                code.and_then(char::from_u32)
            }
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(tail.get(..=end).unwrap_or_default()),
        }
        rest = tail.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// The LaunchAgent property list that starts `exe` with [`AUTOSTART_ARG`] at sign-in.
pub fn launch_agent_plist(exe: &Path) -> Result<String, Error> {
    let s = utf8_path(exe, "a property list")?;
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCH_ID}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>{AUTOSTART_ARG}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
</dict>
</plist>
"#,
        xml_escape(s)
    ))
}

/// The text after `<key>{key}</key>` (and whitespace), if the key is there.
fn after_key<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("<key>{key}</key>");
    let i = plist.find(&marker)?;
    plist.get(i + marker.len()..).map(str::trim_start)
}

/// The first `ProgramArguments` entry of a LaunchAgent property list.
pub fn launch_agent_program(plist: &str) -> Option<PathBuf> {
    let array = after_key(plist, "ProgramArguments")?.strip_prefix("<array>")?.trim_start();
    let body = array.strip_prefix("<string>")?;
    let end = body.find("</string>")?;
    let program = xml_unescape(body.get(..end)?);
    (!program.is_empty()).then(|| PathBuf::from(program))
}

/// A LaunchAgent with `<key>Disabled</key><true/>` doesn't start.
pub fn launch_agent_disabled(plist: &str) -> bool {
    after_key(plist, "Disabled").is_some_and(|rest| rest.starts_with("<true/>"))
}

/// `~/Library/LaunchAgents/ai.umage.CodeShellManager.plist` (macOS).
pub struct LaunchAgent {
    path: PathBuf,
}

impl LaunchAgent {
    /// `dir` is the LaunchAgents folder.
    pub fn new(dir: PathBuf) -> Self {
        LaunchAgent { path: dir.join(format!("{LAUNCH_ID}.plist")) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Backend for LaunchAgent {
    fn location(&self) -> String {
        self.path.display().to_string()
    }

    fn read(&self) -> io::Result<Option<Entry>> {
        Ok(read_file(&self.path)?.map(|t| Entry { program: launch_agent_program(&t), disabled_by_os: launch_agent_disabled(&t) }))
    }

    fn write(&self, program: &Path) -> Result<(), Error> {
        Ok(write_file(&self.path, &launch_agent_plist(program)?)?)
    }

    fn remove(&self) -> io::Result<()> {
        remove_file(&self.path)
    }
}

/// Characters that make an `Exec` argument need quotes (desktop-entry spec, "The Exec key").
const EXEC_RESERVED: &[char] = &[' ', '\t', '\n', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')', '`'];

/// One `Exec` argument: quoted when it holds a reserved character, with `"`, `` ` ``, `$` and `\`
/// backslash-escaped inside the quotes; `%` doubled (a lone `%` would be a field code).
fn exec_arg(arg: &str) -> String {
    let quoted = if arg.is_empty() || arg.contains(EXEC_RESERVED) {
        let mut q = String::from("\"");
        for c in arg.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                q.push('\\');
            }
            q.push(c);
        }
        q.push('"');
        q
    } else {
        arg.to_string()
    };
    quoted.replace('%', "%%")
}

/// A desktop-entry `string` value: `\` written as `\\` (the file-level escape, applied on top of
/// the `Exec` quoting).
fn entry_value_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
}

fn entry_value_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// The `Exec` value's arguments (after the file-level unescape): quotes removed, `\x` inside
/// quotes taken as `x`, `%%` as `%`.
fn exec_args(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '%' if chars.peek() == Some(&'%') => {
                chars.next();
                cur.push('%');
                in_arg = true;
            }
            ' ' | '\t' if !quoted => {
                if in_arg {
                    args.push(std::mem::take(&mut cur));
                    in_arg = false;
                }
            }
            c => {
                cur.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(cur);
    }
    args
}

/// The XDG autostart desktop entry that starts `exe` with [`AUTOSTART_ARG`] at sign-in.
pub fn desktop_entry(exe: &Path) -> Result<String, Error> {
    let s = utf8_path(exe, "a desktop entry")?;
    let exec = entry_value_escape(&format!("{} {}", exec_arg(s), exec_arg(AUTOSTART_ARG)));
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=CodeShellManager\nComment=Start CodeShellManager when you sign in\nExec={exec}\nIcon={LAUNCH_ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    ))
}

/// A key's value in the `[Desktop Entry]` group.
fn entry_key<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut in_group = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim_start());
        }
    }
    None
}

/// The program a desktop entry's `Exec` starts.
pub fn desktop_entry_program(text: &str) -> Option<PathBuf> {
    let exec = entry_value_unescape(entry_key(text, "Exec")?);
    exec_args(&exec).into_iter().next().filter(|p| !p.is_empty()).map(PathBuf::from)
}

/// `Hidden=true` deletes the entry as far as the session is concerned; GNOME also honours
/// `X-GNOME-Autostart-enabled=false` (its Startup Applications switch).
pub fn desktop_entry_disabled(text: &str) -> bool {
    entry_key(text, "Hidden").is_some_and(|v| v.eq_ignore_ascii_case("true"))
        || entry_key(text, "X-GNOME-Autostart-enabled").is_some_and(|v| v.eq_ignore_ascii_case("false"))
}

/// `$XDG_CONFIG_HOME/autostart/ai.umage.CodeShellManager.desktop` (Linux and other XDG desktops).
pub struct XdgAutostart {
    path: PathBuf,
}

impl XdgAutostart {
    /// `dir` is the autostart folder.
    pub fn new(dir: PathBuf) -> Self {
        XdgAutostart { path: dir.join(format!("{LAUNCH_ID}.desktop")) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Backend for XdgAutostart {
    fn location(&self) -> String {
        self.path.display().to_string()
    }

    fn read(&self) -> io::Result<Option<Entry>> {
        Ok(read_file(&self.path)?.map(|t| Entry { program: desktop_entry_program(&t), disabled_by_os: desktop_entry_disabled(&t) }))
    }

    fn write(&self, program: &Path) -> Result<(), Error> {
        Ok(write_file(&self.path, &desktop_entry(program)?)?)
    }

    fn remove(&self) -> io::Result<()> {
        remove_file(&self.path)
    }
}

/// An entry kept in memory: the engine's default, so tests and the demo never change what starts
/// at sign-in. `disabled_by_os` stands in for the OS turning the entry off.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    entry: Mutex<Option<Entry>>,
}

impl MemoryBackend {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Entry>> {
        self.entry.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Sets the entry as the OS would have it.
    pub fn put(&self, entry: Option<Entry>) {
        *self.lock() = entry;
    }
}

impl Backend for MemoryBackend {
    fn location(&self) -> String {
        "(memory)".into()
    }

    fn read(&self) -> io::Result<Option<Entry>> {
        Ok(self.lock().clone())
    }

    fn write(&self, program: &Path) -> Result<(), Error> {
        utf8_path(program, "an autostart entry")?;
        *self.lock() = Some(Entry { program: Some(program.to_path_buf()), disabled_by_os: false });
        Ok(())
    }

    fn remove(&self) -> io::Result<()> {
        *self.lock() = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use crate::argv::split_command_line as split_args;

    fn win(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    /// An absolute path on the host OS (for the file formats, whose writers check `is_absolute`).
    fn abs(rest: &str) -> PathBuf {
        if cfg!(windows) { PathBuf::from(format!(r"C:\{rest}")) } else { PathBuf::from(format!("/{rest}")) }
    }

    #[cfg(windows)]
    #[test]
    fn the_run_value_quotes_the_path_and_round_trips() {
        for p in [r"C:\Program Files\umage.ai\CodeShellManager\codeshell.exe", r"C:\Users\Åsa Ølstrøm\apps\codeshell.exe", r"C:\x\codeshell.exe"] {
            let line = windows_command_line(&win(p)).unwrap();
            assert_eq!(line, format!("\"{p}\" --autostart"));
            assert_eq!(windows_program(&line), Some(win(p)));
            // The arguments after the program token, by the MSVCRT rules, are just the flag.
            let rest = line.get(p.len() + 2..).unwrap();
            assert_eq!(split_args(rest), vec!["--autostart".to_string()]);
        }
    }

    #[test]
    fn the_run_value_refuses_what_it_cannot_quote() {
        assert!(windows_command_line(Path::new("relative\\codeshell.exe")).is_err());
        #[cfg(windows)]
        {
            assert!(windows_command_line(Path::new("C:\\a\"b\\codeshell.exe")).is_err());
            assert!(windows_command_line(Path::new("C:\\a\nb\\codeshell.exe")).is_err());
            assert!(windows_command_line(Path::new("C:\\apps\\")).is_err());
        }
    }

    #[test]
    fn the_program_of_a_run_value_follows_the_program_name_rules() {
        assert_eq!(windows_program(r#""C:\Program Files\x\a.exe" --autostart"#), Some(win(r"C:\Program Files\x\a.exe")));
        assert_eq!(windows_program(r"C:\x\a.exe --autostart"), Some(win(r"C:\x\a.exe")));
        assert_eq!(windows_program(r#"  "C:\x y\a.exe""#), Some(win(r"C:\x y\a.exe")));
        assert_eq!(windows_program(r#""C:\unterminated"#), Some(win(r"C:\unterminated")));
        assert_eq!(windows_program(""), None);
        assert_eq!(windows_program(r#""""#), None);
    }

    #[test]
    fn task_manager_marks_a_disabled_entry_with_an_odd_first_byte() {
        assert!(!startup_approved_disabled(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!startup_approved_disabled(&[6, 0, 0, 0]));
        assert!(startup_approved_disabled(&[3, 0, 0, 0, 0x10, 0x20, 0, 0, 0, 0, 0, 0]));
        assert!(startup_approved_disabled(&[1]));
        assert!(!startup_approved_disabled(&[]));
    }

    #[cfg(windows)]
    fn run_key() -> WindowsRun<MemoryRegistry> {
        WindowsRun::new(MemoryRegistry::default())
    }

    #[cfg(windows)]
    #[test]
    fn the_run_key_is_written_read_back_and_removed() {
        let b = run_key();
        let exe = win(r"C:\Program Files\umage.ai\CodeShellManager\codeshell.exe");
        let s = status(&b, &exe).unwrap();
        assert!(!s.enabled && !s.registered && s.program.is_none());
        let s = set(&b, &exe, true).unwrap();
        assert!(s.enabled && s.registered && s.current && !s.disabled_by_os, "{s:?}");
        assert_eq!(
            b.registry().value(RUN_KEY, ENTRY_NAME),
            Some(MemValue::String(r#""C:\Program Files\umage.ai\CodeShellManager\codeshell.exe" --autostart"#.into()))
        );
        assert_eq!(s.location, r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run\CodeShellManager");
        // Paths compare without case.
        assert!(status(&b, &win(r"c:\program files\UMAGE.AI\codeshellmanager\CODESHELL.EXE")).unwrap().current);
        let s = set(&b, &exe, false).unwrap();
        assert!(!s.registered && b.registry().value(RUN_KEY, ENTRY_NAME).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_task_manager_disable_reads_back_and_turning_on_clears_it() {
        let b = run_key();
        let exe = win(r"C:\apps\codeshell.exe");
        set(&b, &exe, true).unwrap();
        b.registry().put(APPROVED_KEY, ENTRY_NAME, MemValue::Binary(vec![3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8]));
        let s = status(&b, &exe).unwrap();
        assert!(s.registered && s.disabled_by_os && !s.enabled, "{s:?}");
        // The fix-up never turns it back on.
        assert_eq!(refresh(&b, &win(r"C:\new\codeshell.exe"), false).unwrap(), None);
        let s = set(&b, &exe, true).unwrap();
        assert!(s.enabled && !s.disabled_by_os);
        assert!(b.registry().value(APPROVED_KEY, ENTRY_NAME).is_none());
        // Off removes Task Manager's record too.
        b.registry().put(APPROVED_KEY, ENTRY_NAME, MemValue::Binary(vec![2; 12]));
        set(&b, &exe, false).unwrap();
        assert!(b.registry().value(APPROVED_KEY, ENTRY_NAME).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn the_fix_up_moves_the_entry_to_the_current_exe() {
        let b = run_key();
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old").join("codeshell.exe");
        let new = dir.path().join("new").join("codeshell.exe");
        assert_eq!(refresh(&b, &new, false).unwrap(), None, "nothing registered: nothing to fix");
        set(&b, &old, true).unwrap();
        // The old exe is gone (an upgrade into a new folder): rewritten even in "only if missing" mode.
        assert_eq!(refresh(&b, &new, true).unwrap(), Some(old.clone()));
        assert!(status(&b, &new).unwrap().current);
        assert_eq!(refresh(&b, &new, false).unwrap(), None, "already ours");
        // The other exe still exists: a development build leaves it alone, the release build takes it.
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"").unwrap();
        set(&b, &old, true).unwrap();
        assert_eq!(refresh(&b, &new, true).unwrap(), None);
        assert_eq!(refresh(&b, &new, false).unwrap(), Some(old.clone()));
    }

    #[cfg(windows)]
    #[test]
    fn the_fix_up_follows_the_move_from_the_umage_folder_to_umage_ai() {
        // 2.0 installs under `umage.ai`; an entry written by an older 2.x names the old folder,
        // which the upgrade removed (here it never existed, so it is gone).
        let b = run_key();
        let old = win(r"C:\Program Files\umage\CodeShellManager\codeshell.exe");
        let new = win(r"C:\Program Files\umage.ai\CodeShellManager\codeshell.exe");
        set(&b, &old, true).unwrap();
        assert!(!status(&b, &new).unwrap().current, "the two folders are different programs");
        assert_eq!(refresh(&b, &new, false).unwrap(), Some(old.clone()));
        assert!(status(&b, &new).unwrap().current);
        assert_eq!(
            b.registry().value(RUN_KEY, ENTRY_NAME),
            Some(MemValue::String(r#""C:\Program Files\umage.ai\CodeShellManager\codeshell.exe" --autostart"#.into()))
        );
    }

    #[cfg(windows)]
    #[test]
    fn each_channel_has_its_own_run_value() {
        use csm_core::channel::Channel;
        assert_eq!(entry_name_for(Channel::Stable), "CodeShellManager");
        assert_ne!(entry_name_for(Channel::Beta), entry_name_for(Channel::Stable));
        let beta = WindowsRun::with_name(MemoryRegistry::default(), entry_name_for(Channel::Beta));
        let exe = win(r"C:\Program Files\umage.ai\CodeShellManager Beta\codeshell.exe");
        // A stable value exists in the registry; the beta backend never sees or touches it.
        beta.registry().put(RUN_KEY, ENTRY_NAME, MemValue::String(r#""C:\x\codeshell.exe" --autostart"#.into()));
        assert!(beta.read().unwrap().is_none(), "the beta never reads the stable value");
        set(&beta, &exe, true).unwrap();
        assert!(beta.registry().value(RUN_KEY, ENTRY_NAME).is_some(), "the stable value is untouched");
        assert!(beta.registry().value(RUN_KEY, BETA_ENTRY_NAME).is_some());
        beta.remove().unwrap();
        assert!(beta.registry().value(RUN_KEY, ENTRY_NAME).is_some(), "removing the beta's entry keeps the stable one");
        assert!(beta.registry().value(RUN_KEY, BETA_ENTRY_NAME).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn the_uninstaller_removes_only_its_own_or_a_dangling_entry() {
        let b = run_key();
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other.exe");
        std::fs::write(&other, b"").unwrap();
        let me = dir.path().join("me.exe");
        set(&b, &other, true).unwrap();
        assert!(!remove_if_ours(&b, &me).unwrap(), "another install's entry stays");
        assert!(status(&b, &me).unwrap().registered);
        set(&b, &me, true).unwrap();
        assert!(remove_if_ours(&b, &me).unwrap());
        assert!(!status(&b, &me).unwrap().registered);
        set(&b, &dir.path().join("gone.exe"), true).unwrap();
        assert!(remove_if_ours(&b, &me).unwrap(), "dangling");
        assert!(!remove_if_ours(&b, &me).unwrap(), "nothing left");
    }

    #[test]
    fn the_launch_agent_escapes_the_path_and_round_trips() {
        for rest in ["Applications/CodeShellManager.app/Contents/MacOS/codeshell", "Users/Åsa & Bo/<apps>/it's \"here\"/codeshell"] {
            let exe = abs(rest);
            let plist = launch_agent_plist(&exe).unwrap();
            assert_eq!(launch_agent_program(&plist), Some(exe.clone()), "{plist}");
            assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
            assert!(plist.contains("<string>ai.umage.CodeShellManager</string>"));
            assert!(plist.contains("<string>--autostart</string>"));
            assert!(!launch_agent_disabled(&plist));
            assert!(!plist.contains(" & "), "escaped: {plist}");
        }
        let disabled = launch_agent_plist(&abs("x/codeshell"))
            .unwrap()
            .replace("<key>RunAtLoad</key>", "<key>Disabled</key>\n\t<true/>\n\t<key>RunAtLoad</key>");
        assert!(launch_agent_disabled(&disabled));
        assert!(launch_agent_plist(Path::new("relative/codeshell")).is_err());
        assert_eq!(xml_unescape("a&#38;b&#x3C;c&bogus;d&"), "a&b<c&bogus;d&");
    }

    #[test]
    fn the_desktop_entry_escapes_per_the_spec_and_round_trips() {
        let plain = desktop_entry(&abs("opt/csm/codeshell")).unwrap();
        let line = plain.lines().find(|l| l.starts_with("Exec=")).unwrap();
        if cfg!(windows) {
            // A Windows-style path has a backslash: quoted, escaped inside, then doubled again.
            assert_eq!(line, r#"Exec="C:\\\\opt/csm/codeshell" --autostart"#);
        } else {
            assert_eq!(line, "Exec=/opt/csm/codeshell --autostart");
        }
        for rest in ["home/åsa/Apps/Code Shell/codeshell", "x/100% $HOME `id` \"q\" 'a' \\b; (c) #d/codeshell", "x/日本語/codeshell.AppImage"] {
            let exe = abs(rest);
            let text = desktop_entry(&exe).unwrap();
            assert_eq!(desktop_entry_program(&text), Some(exe.clone()), "{text}");
            let exec = entry_value_unescape(entry_key(&text, "Exec").unwrap());
            assert_eq!(exec_args(&exec), vec![exe.to_str().unwrap().to_string(), "--autostart".into()], "{exec}");
            assert!(!desktop_entry_disabled(&text));
        }
        let with_space = desktop_entry(&abs("a b/codeshell")).unwrap();
        assert!(with_space.contains("Exec=\""), "{with_space}");
        assert!(desktop_entry(&abs("100%/codeshell")).unwrap().contains("100%%"));
        assert!(desktop_entry_disabled("[Desktop Entry]\nExec=x\nHidden=true\n"));
        assert!(desktop_entry_disabled("[Desktop Entry]\nExec=x\nX-GNOME-Autostart-enabled=false\n"));
        assert!(!desktop_entry_disabled("[Other]\nHidden=true\n[Desktop Entry]\nExec=x\n"));
        assert!(desktop_entry(&abs("a\nb/codeshell")).is_err());
    }

    #[test]
    fn the_file_backends_write_read_and_remove_in_their_folder() {
        let dir = tempfile::tempdir().unwrap();
        let exe = abs("Apps/Code Shell/codeshell");
        let backends: [Box<dyn Backend>; 2] =
            [Box::new(LaunchAgent::new(dir.path().join("LaunchAgents"))), Box::new(XdgAutostart::new(dir.path().join("autostart")))];
        for b in backends {
            assert!(!status(b.as_ref(), &exe).unwrap().registered);
            let s = set(b.as_ref(), &exe, true).unwrap();
            assert!(s.enabled && s.current, "{s:?}");
            assert!(Path::new(&s.location).is_file());
            assert!(s.location.contains("ai.umage.CodeShellManager"));
            let s = set(b.as_ref(), &exe, false).unwrap();
            assert!(!s.registered && !Path::new(&s.location).exists());
            b.remove().unwrap();
        }
        let xdg = XdgAutostart::new(dir.path().join("autostart"));
        xdg.write(&exe).unwrap();
        let text = std::fs::read_to_string(xdg.path()).unwrap() + "Hidden=true\n";
        std::fs::write(xdg.path(), text).unwrap();
        let s = status(&xdg, &exe).unwrap();
        assert!(s.registered && s.disabled_by_os && !s.enabled);
    }

    #[test]
    fn the_memory_backend_stands_in_for_the_os() {
        let b = MemoryBackend::default();
        let exe = abs("x/codeshell");
        assert!(set(&b, &exe, true).unwrap().enabled);
        b.put(Some(Entry { program: Some(exe.clone()), disabled_by_os: true }));
        assert!(!status(&b, &exe).unwrap().enabled);
        assert!(!set(&b, &exe, false).unwrap().registered);
    }

    /// The real registry adapter, on a test-only key of its own (never the Run key), removed after.
    #[cfg(windows)]
    #[test]
    fn the_current_user_registry_reads_writes_and_deletes_on_a_test_key() {
        use winreg::enums::HKEY_CURRENT_USER;
        let key = format!(r"Software\umage.ai\CodeShellManagerAutostartTest-{}", std::process::id());
        let reg = CurrentUserRegistry;
        assert_eq!(reg.get_string(&key, "Entry").unwrap(), None, "no key yet");
        reg.delete(&key, "Entry").unwrap();
        let line = windows_command_line(Path::new(r"C:\Program Files\Åsa\codeshell.exe")).unwrap();
        reg.set_string(&key, "Entry", &line).unwrap();
        assert_eq!(reg.get_string(&key, "Entry").unwrap().as_deref(), Some(line.as_str()));
        assert_eq!(reg.get_binary(&key, "Missing").unwrap(), None);
        assert!(reg.get_binary(&key, "Entry").unwrap().is_some_and(|b| !b.is_empty()));
        reg.delete(&key, "Entry").unwrap();
        assert_eq!(reg.get_string(&key, "Entry").unwrap(), None);
        winreg::RegKey::predef(HKEY_CURRENT_USER).delete_subkey(&key).unwrap();
    }
}

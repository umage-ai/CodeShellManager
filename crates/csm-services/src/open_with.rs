//! "Open with..." (editors and IDEs): template parsing, placeholder substitution and every
//! validation rule. Port of `OpenWithService`, minus detection and anything that touches the
//! disk, registry or a process; those questions are asked through [`DiskProbe`] and
//! [`OpenWithHost`], which tests fake.
//!
//! **Never a command line built by interpolation.** A template is split into tokens first;
//! only then are placeholders substituted, each inside its own token. A folder named
//! `a" --x "b` therefore stays one argv element, and the launcher must exec the program
//! directly with that argv (no shell).
//!
//! **Batch files are the exception that has to be fenced.** CreateProcess runs `.cmd`/`.bat`
//! through `cmd.exe`, which re-parses the arguments with its own rules (`%` expands even inside
//! quotes, `&` chains a command: BatBadBut, CVE-2024-24576). When a batch file is allowed at
//! all, every argument containing a cmd metacharacter is **refused, never escaped**.
//!
//! **An argument must not be readable as a switch.** A value that starts an argv element may not
//! begin with `-` or `/`: Chromium (so every Electron editor) accepts `/name=value` as a switch
//! on Windows; verified against Code.exe 1.143.1, where `/js-flags=...` reached the renderer as
//! `--js-flags=...`. That is why the seeded VS Code remote forms are the single token
//! `--folder-uri vscode-remote://...` and not a bare Linux path argument.
//!
//! Path rules are Windows rules (see [`crate::winpath`]), because the programs being launched
//! are Windows programs; macOS and Linux editors need their own rules when this goes
//! cross-platform.

use crate::argv::quote_for_cmd;
use crate::launch::{LaunchSpec, SessionKind};
use crate::winpath;
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum entries kept when sanitising a list.
pub const MAX_ENTRIES: usize = 50;

/// Characters cmd.exe interprets in a batch file's command line, quoted or not: `%` and `!`
/// expand variables, `& | < >` chain and redirect, `^` escapes, `"` toggles cmd's quote state,
/// `( )` group, and `, ; =` are argument delimiters to a batch script.
pub const BATCH_META_CHARS: &str = "%!&|<>^\"(),;=";

/// Programs that run text (or an argument) as code. Checked against the file name, the
/// long-path name and the PE version-info original name. A speed bump, not the boundary: a
/// renamed copy of an arbitrary program can't be recognised by name, which is why imported
/// lists are confirmed by the user and why everything under `%SystemRoot%` is refused outright.
const SHELL_LIKE_NAMES: &[&str] = &[
    "cmd",
    "powershell",
    "powershell_ise",
    "pwsh",
    "wsl",
    "bash",
    "sh",
    "wscript",
    "cscript",
    "mshta",
    "rundll32",
    "regsvr32",
    "msiexec",
    "conhost",
    "wt",
    "schtasks",
    "forfiles",
    "runas",
    "start",
    "reg",
    "certutil",
    "bitsadmin",
    "ssh",
    "scp",
    "sftp",
    "wmic",
    "pcalua",
    "mmc",
    "hh",
    "explorer",
    "msbuild",
    "installutil",
    "odbcconf",
    "regasm",
    "regsvcs",
    "cmstp",
    "msxsl",
    "ieexec",
    "dotnet",
    "node",
    "python",
    "pythonw",
    "py",
    "pyw",
    "perl",
    "ruby",
    "java",
    "javaw",
    "csi",
    "fsi",
    "winrm",
    "psexec",
    "at",
];

/// Seeded VS Code templates (single `--folder-uri` token for the remote forms).
pub const VSCODE_LOCAL_ARGS: &str = "{folder}";
pub const VSCODE_WSL_ARGS: &str = "--folder-uri vscode-remote://wsl+{distro:uri}{linuxPath:uri}";
pub const VSCODE_SSH_ARGS: &str =
    "--folder-uri vscode-remote://ssh-remote+{userAtHost:uri}{remoteFolder:uri}";

/// One "Open with" entry. Mirrors `Models.OpenWithEntry` (unify with `csm-core` later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWithEntry {
    pub id: String,
    pub label: String,
    /// Absolute path to the program. Never a shell.
    pub executable: String,
    pub local_args: Option<String>,
    pub wsl_args: Option<String>,
    pub ssh_args: Option<String>,
    /// Exactly one entry is the default (the toolbar button opens it).
    pub is_default: bool,
}

impl Default for OpenWithEntry {
    fn default() -> Self {
        Self {
            id: new_id(),
            label: String::new(),
            executable: String::new(),
            local_args: None,
            wsl_args: None,
            ssh_args: None,
            is_default: false,
        }
    }
}

impl OpenWithEntry {
    #[must_use]
    pub fn args_for(&self, kind: SessionKind) -> Option<&str> {
        match kind {
            SessionKind::Local => self.local_args.as_deref(),
            SessionKind::Wsl => self.wsl_args.as_deref(),
            SessionKind::Ssh => self.ssh_args.as_deref(),
        }
    }

    #[must_use]
    pub fn applies_to(&self, kind: SessionKind) -> bool {
        self.args_for(kind).is_some_and(|t| !t.trim().is_empty())
    }

    /// The seeded VS Code-family entry (`OpenWithDetector.VsCodeEntry`).
    #[must_use]
    pub fn vscode(label: &str, exe: &str) -> Self {
        Self {
            label: label.to_string(),
            executable: exe.to_string(),
            local_args: Some(VSCODE_LOCAL_ARGS.to_string()),
            wsl_args: Some(VSCODE_WSL_ARGS.to_string()),
            ssh_args: Some(VSCODE_SSH_ARGS.to_string()),
            ..Self::default()
        }
    }
}

/// Leaves exactly one default when the list is non-empty: the first flagged, else the first.
pub fn ensure_single_default(list: &mut [OpenWithEntry]) {
    let mut seen = false;
    for e in list.iter_mut() {
        if e.is_default && !seen {
            seen = true;
        } else {
            e.is_default = false;
        }
    }
    if !seen && let Some(first) = list.first_mut() {
        first.is_default = true;
    }
}

/// A fresh opaque id (32 hex digits). Not security-sensitive: it only has to be unique within
/// the list. Replace with the `csm-core` id generator when that exists.
#[must_use]
pub fn new_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut h1 = RandomState::new().build_hasher();
    h1.write_u64(n);
    h1.write_u128(nanos);
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(n.wrapping_add(1));
    h2.write_u128(nanos.rotate_left(17));
    format!("{:016x}{:016x}", h1.finish(), h2.finish())
}

/// The values a session offers to an argument template: a snapshot built from the live session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWithContext {
    pub kind: SessionKind,
    pub folder: String,
    pub distro: String,
    pub linux_path: String,
    pub wsl_user: String,
    pub user: String,
    pub host: String,
    pub port: i32,
    pub remote_folder: String,
}

impl OpenWithContext {
    #[must_use]
    pub fn new(kind: SessionKind) -> Self {
        Self {
            kind,
            folder: String::new(),
            distro: String::new(),
            linux_path: String::new(),
            wsl_user: String::new(),
            user: String::new(),
            host: String::new(),
            port: 22,
            remote_folder: String::new(),
        }
    }

    #[must_use]
    pub fn from_spec(s: &LaunchSpec) -> Self {
        Self {
            kind: s.kind,
            folder: s.working_folder.clone(),
            distro: s.wsl_distro.clone(),
            linux_path: s.wsl_working_folder.clone(),
            wsl_user: s.wsl_user.clone(),
            user: s.ssh_user.clone(),
            host: s.ssh_host.clone(),
            port: s.ssh_port,
            remote_folder: s.ssh_remote_folder.clone(),
        }
    }

    /// Whether a session of this shape has anything to open: a local folder, a distro, or a
    /// host. Pure; no disk access.
    #[must_use]
    pub fn has_target(&self) -> bool {
        let set = |s: &str| !s.trim().is_empty();
        match self.kind {
            SessionKind::Local => set(&self.folder),
            SessionKind::Wsl => set(&self.distro),
            SessionKind::Ssh => set(&self.host),
        }
    }
}

/// A resolved launch: the program and its argv. Each element is passed as-is (an
/// `argv`-style launch, never through a shell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWithCommand {
    pub executable: String,
    pub arguments: Vec<String>,
}

impl OpenWithCommand {
    /// The command line for the editor preview, menu tooltips and error messages. Never
    /// executed. Quoted with the MSVCRT rules, which encode the same argv as a real launch.
    #[must_use]
    pub fn display(&self) -> String {
        std::iter::once(quote_for_cmd(&self.executable, false))
            .chain(self.arguments.iter().map(|a| quote_for_cmd(a, false)))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Placeholders each session kind provides. Anything else in a template is an error, not
/// literal text.
#[must_use]
pub fn placeholders_for(kind: SessionKind) -> &'static [&'static str] {
    match kind {
        SessionKind::Local => &["folder"],
        SessionKind::Wsl => &["folder", "distro", "linuxPath"],
        SessionKind::Ssh => &["user", "host", "userAtHost", "port", "remoteFolder"],
    }
}

/// Placeholders whose `:uri` form keeps `/` (a path) rather than encoding it.
fn is_path_placeholder(name: &str) -> bool {
    matches!(name, "linuxPath" | "remoteFolder")
}

#[must_use]
pub fn kind_name(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Local => "local",
        SessionKind::Wsl => "WSL",
        SessionKind::Ssh => "SSH",
    }
}

// ── Executable ───────────────────────────────────────────────────────────────────────────

#[must_use]
pub fn is_batch(path: &str) -> bool {
    let ext = winpath::extension(path);
    ext.eq_ignore_ascii_case(".cmd") || ext.eq_ignore_ascii_case(".bat")
}

/// `%SystemRoot%`, e.g. `C:\Windows`, with the usual fallback.
#[must_use]
pub fn default_system_root() -> String {
    match std::env::var("SystemRoot") {
        Ok(v) if !v.is_empty() => v,
        _ => r"C:\Windows".to_string(),
    }
}

fn is_under(path: &str, root: &str) -> bool {
    if root.is_empty() || !winpath::is_fully_qualified(path) || !winpath::is_fully_qualified(root) {
        return false;
    }
    let p = winpath::normalize_full(path);
    let p = p.trim_end_matches('\\');
    let r = winpath::normalize_full(root);
    let r = r.trim_end_matches('\\');
    winpath::starts_with_ic(p, &format!("{r}\\")) || winpath::eq_ic(p, r)
}

fn is_shell_like(name: &str) -> bool {
    let lower = name.to_lowercase();
    SHELL_LIKE_NAMES.contains(&lower.as_str())
}

/// `None` when `path` is acceptable as an entry's program, else the reason. Pure; no disk
/// access ([`validate_executable_on_disk`] repeats the checks against the real path).
///
/// Rules: an absolute local path (no relative path, so nothing is resolved against PATH or the
/// cwd; no UNC/device path, so nothing runs from a share), a `.exe` (or `.cmd`/`.bat` only
/// when `allow_batch`), not under `system_root`, and not a known shell or script host.
#[must_use]
pub fn validate_executable_with(
    path: &str,
    allow_batch: bool,
    system_root: &str,
) -> Option<String> {
    if path.trim().is_empty() {
        return Some("No program is set.".to_string());
    }
    if path.chars().any(char::is_control) {
        return Some("The program path contains control characters.".to_string());
    }
    if path.starts_with(r"\\") || path.starts_with("//") {
        return Some(
            "Network and device paths are not accepted — use a program on this machine."
                .to_string(),
        );
    }
    if !winpath::is_fully_qualified(path) {
        return Some(
            "The program must be a full path, e.g. C:\\Program Files\\Editor\\editor.exe."
                .to_string(),
        );
    }

    if is_batch(path) {
        if !allow_batch {
            return Some(
                "Batch files (.cmd/.bat) run through cmd.exe and are not accepted here — point at the program's .exe."
                    .to_string(),
            );
        }
    } else if !winpath::extension(path).eq_ignore_ascii_case(".exe") {
        return Some("The program must be an .exe.".to_string());
    }

    if is_under(path, system_root) {
        return Some(format!(
            "Programs under {system_root} are not accepted — no editor lives there, and many tools there run commands from their arguments."
        ));
    }
    if is_shell_like(winpath::file_name_without_extension(path)) {
        return Some(format!(
            "{} is a shell or script host, not an editor — it is not accepted.",
            winpath::file_name(path)
        ));
    }
    None
}

/// [`validate_executable_with`] using `%SystemRoot%`.
#[must_use]
pub fn validate_executable(path: &str, allow_batch: bool) -> Option<String> {
    validate_executable_with(path, allow_batch, &default_system_root())
}

/// What the real file system says about a program. Implemented over the OS later; faked in
/// tests.
pub trait DiskProbe {
    /// Whether the drive containing `root` (e.g. `Z:\`) is a network drive.
    fn drive_is_network(&self, root: &str) -> bool;
    fn file_exists(&self, path: &str) -> bool;
    /// The file's final path (long names, links and subst resolved); `None` on failure.
    fn final_path(&self, path: &str) -> Option<String>;
    /// The PE version-info `OriginalFilename` and `InternalName` (blank ones omitted).
    fn version_names(&self, path: &str) -> Vec<String>;
}

fn remove_ignore_ascii_case(haystack: &str, needle: &str) -> String {
    let lower_h = haystack.to_ascii_lowercase();
    let lower_n = needle.to_ascii_lowercase();
    let mut out = String::new();
    let mut idx = 0usize;
    while let Some(pos) = lower_h.get(idx..).and_then(|s| s.find(&lower_n)) {
        out.push_str(haystack.get(idx..idx + pos).unwrap_or(""));
        idx += pos + needle.len();
    }
    out.push_str(haystack.get(idx..).unwrap_or(""));
    out
}

/// [`validate_executable`] against the program's REAL path: 8.3 short names expanded
/// (`POWERS~1.EXE`), junctions, symlinks and subst drives followed, then a network drive refused
/// (mapped or subst'd onto a share), and the PE original file name checked too, so a renamed
/// copy of a known shell is caught. A program that doesn't exist yet passes (it fails at launch).
#[must_use]
pub fn validate_executable_on_disk(
    path: &str,
    allow_batch: bool,
    probe: &dyn DiskProbe,
) -> Option<String> {
    let shape_error = validate_executable(path, allow_batch);
    if shape_error.is_some() {
        return shape_error;
    }
    if probe.drive_is_network(&winpath::path_root(path)) {
        return Some(
            "Programs on a network drive are not accepted — use a program on this machine."
                .to_string(),
        );
    }
    if !probe.file_exists(path) {
        return None;
    }

    if let Some(real) = probe.final_path(path)
        && !winpath::eq_ic(&real, path)
    {
        if real.starts_with(r"\\") {
            return Some(
                "That program resolves to a network location — it is not accepted.".to_string(),
            );
        }
        if probe.drive_is_network(&winpath::path_root(&real)) {
            return Some(
                "Programs on a network drive are not accepted — use a program on this machine."
                    .to_string(),
            );
        }
        let real_error = validate_executable(&real, allow_batch);
        if real_error.is_some() {
            return real_error;
        }
    }

    for name in probe.version_names(path) {
        if name.trim().is_empty() {
            continue;
        }
        let stripped = remove_ignore_ascii_case(name.trim(), ".mui");
        let bare = winpath::file_name_without_extension(&stripped);
        if is_shell_like(bare) {
            return Some(format!(
                "{} is a copy of {}, a shell or script host — it is not accepted.",
                winpath::file_name(path),
                name.trim()
            ));
        }
    }
    None
}

// ── Templates ────────────────────────────────────────────────────────────────────────────

/// Splits a template into tokens on whitespace; a `"..."` span groups text containing spaces
/// into one token (the quotes are removed; there is no escape syntax, and a literal `"` cannot
/// be written). `None` on an unbalanced quote.
#[must_use]
pub fn tokenize(template: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    if template.trim().is_empty() {
        return Some(tokens);
    }
    let mut cur = String::new();
    let (mut in_quotes, mut in_token) = (false, false);
    for c in template.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
            in_token = true;
            continue;
        }
        if !in_quotes && c.is_whitespace() {
            if in_token {
                tokens.push(std::mem::take(&mut cur));
                in_token = false;
            }
            continue;
        }
        cur.push(c);
        in_token = true;
    }
    if in_quotes {
        return None;
    }
    if in_token {
        tokens.push(cur);
    }
    Some(tokens)
}

/// One `{name}` / `{name:uri}` occurrence in a token.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placeholder {
    start: usize,
    end: usize,
    name: String,
    uri: bool,
}

/// All matches of `\{([A-Za-z]+)(:uri)?\}`, leftmost and non-overlapping (hand-written so the
/// grammar is one obvious place).
fn find_placeholders(token: &str) -> Vec<Placeholder> {
    let bytes = token.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes.get(i) != Some(&b'{') {
            i += 1;
            continue;
        }
        let name_start = i + 1;
        let mut j = name_start;
        while bytes.get(j).is_some_and(u8::is_ascii_alphabetic) {
            j += 1;
        }
        if j == name_start {
            i += 1;
            continue;
        }
        let name = token.get(name_start..j).unwrap_or("").to_string();
        let rest = bytes.get(j..).unwrap_or(&[]);
        let (uri, end) = if rest.first() == Some(&b'}') {
            (false, j + 1)
        } else if rest.starts_with(b":uri}") {
            (true, j + 5)
        } else {
            i += 1;
            continue;
        };
        out.push(Placeholder {
            start: i,
            end,
            name,
            uri,
        });
        i = end;
    }
    out
}

/// `None` when the template is usable for `kind`, else the reason. Blank is valid (not offered
/// for that kind). A non-blank template must use at least one placeholder: an Open-with entry
/// opens the session's location, and a template of only literal arguments would be a fixed
/// command line behind a friendly label.
#[must_use]
pub fn validate_template(template: &str, kind: SessionKind) -> Option<String> {
    if template.trim().is_empty() {
        return None;
    }
    if template.chars().any(char::is_control) {
        return Some("The arguments contain control characters.".to_string());
    }
    let Some(tokens) = tokenize(template) else {
        return Some("Unbalanced quote in the arguments.".to_string());
    };
    let allowed = placeholders_for(kind);
    let allowed_list = allowed
        .iter()
        .map(|a| format!("{{{a}}}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut count = 0usize;
    for token in &tokens {
        for m in find_placeholders(token) {
            count += 1;
            if !allowed.contains(&m.name.as_str()) {
                return Some(format!(
                    "{{{}}} is not available for {} sessions (use {allowed_list}).",
                    m.name,
                    kind_name(kind)
                ));
            }
        }
    }
    if count == 0 {
        return Some(format!(
            "The arguments must use a placeholder for the session's location (e.g. {allowed_list})."
        ));
    }
    None
}

// ── Build ────────────────────────────────────────────────────────────────────────────────

/// Resolves `entry` for `ctx` into a program and argv, or the reason it can't. Pure: no disk
/// access (folder existence and the program file are checked by [`prepare`]).
///
/// # Errors
/// A sentence for the user.
pub fn build(entry: &OpenWithEntry, ctx: &OpenWithContext) -> Result<OpenWithCommand, String> {
    if let Some(e) = validate_executable(&entry.executable, true) {
        return Err(e);
    }
    let template = match entry.args_for(ctx.kind) {
        Some(t) if !t.trim().is_empty() => t,
        _ => {
            return Err(format!(
                "\"{}\" has no arguments for {} sessions.",
                entry.label,
                kind_name(ctx.kind)
            ));
        }
    };
    if let Some(e) = validate_template(template, ctx.kind) {
        return Err(e);
    }

    let values = values_for(ctx);
    let value_of = |name: &str| -> &str { lookup(&values, name) };
    let mut args: Vec<String> = Vec::new();
    for token in tokenize(template).unwrap_or_default() {
        let matches = find_placeholders(&token);
        let Some(first) = matches.first() else {
            args.push(token);
            continue;
        };

        // A token that is nothing but one placeholder, whose value is empty, is dropped rather
        // than passed as "".
        if matches.len() == 1
            && first.start == 0
            && first.end == token.len()
            && value_of(&first.name).is_empty()
        {
            continue;
        }

        for m in &matches {
            let value = value_of(&m.name);
            if let Some(e) = validate_value(&m.name, value) {
                return Err(e);
            }
            // Empty inside a larger token (`vscode-remote://ssh-remote+box{remoteFolder:uri}`)
            // would silently produce a different target. Only {user} may be empty.
            if value.is_empty() && m.name != "user" {
                return Err(missing_value_message(&m.name, ctx.kind));
            }
        }

        let mut arg = String::new();
        let mut last = 0usize;
        for m in &matches {
            arg.push_str(token.get(last..m.start).unwrap_or(""));
            arg.push_str(&encode(&m.name, value_of(&m.name), m.uri));
            last = m.end;
        }
        arg.push_str(token.get(last..).unwrap_or(""));

        // A placeholder that STARTS an argv element must not make it switch-shaped. '-' is
        // refused for every value already; '/' is refused here because Chromium/Electron parse
        // /name=value as a switch on Windows (see the module docs).
        if first.start == 0
            && let Some(c) = arg.chars().next().filter(|c| *c == '-' || *c == '/')
        {
            return Err(format!(
                "The session's {} would start an argument with '{c}', which the program could read as a switch.                  Put it inside a larger argument (e.g. --folder-uri vscode-remote://…{{linuxPath:uri}}).",
                first.name
            ));
        }
        args.push(arg);
    }

    if is_batch(&entry.executable) {
        // BatBadBut: cmd.exe re-parses a batch file's arguments. Refuse, never escape: there
        // is no escaping that is correct for every batch script.
        for a in &args {
            if let Some(bad) = a.chars().find(|c| BATCH_META_CHARS.contains(*c)) {
                return Err(format!(
                    "{} is a batch file, which runs through cmd.exe, and the argument \"{a}\" contains '{bad}', \
                     which cmd.exe would interpret. Point the entry at the program's .exe instead.",
                    winpath::file_name(&entry.executable)
                ));
            }
        }
    }

    Ok(OpenWithCommand {
        executable: entry.executable.clone(),
        arguments: args,
    })
}

fn lookup<'a>(values: &'a [(&'static str, String)], name: &str) -> &'a str {
    values
        .iter()
        .find(|(k, _)| *k == name)
        .map_or("", |(_, v)| v.as_str())
}

fn missing_value_message(name: &str, kind: SessionKind) -> String {
    match name {
        "remoteFolder" => "This SSH session has no remote folder. Set one with Edit session…, or use an editor entry that can open a host without a folder.".to_string(),
        "linuxPath" => "Couldn't work out this WSL session's Linux folder. Set one with Edit session….".to_string(),
        _ => format!("This {} session has no {name}.", kind_name(kind)),
    }
}

/// The `:uri` form, for a value spliced into a URI such as
/// `vscode-remote://wsl+{distro:uri}{linuxPath:uri}`. Path placeholders encode each segment and
/// keep `/`; anything else is encoded whole (so `@` in `{userAtHost:uri}` becomes `%40` and
/// cannot read as URI user-info). VS Code's `URI.parse` percent-decodes both the authority and
/// the path.
#[must_use]
pub fn encode(name: &str, value: &str, uri: bool) -> String {
    if !uri {
        return value.to_string();
    }
    if is_path_placeholder(name) {
        return value
            .split('/')
            .map(escape_data_string)
            .collect::<Vec<_>>()
            .join("/");
    }
    escape_data_string(value)
}

/// `Uri.EscapeDataString`: everything but the RFC 3986 unreserved set (`A-Za-z0-9-._~`) is
/// percent-encoded as UTF-8 bytes with upper-case hex.
#[must_use]
pub fn escape_data_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn values_for(ctx: &OpenWithContext) -> Vec<(&'static str, String)> {
    let user = ctx.user.trim().to_string();
    let host = ctx.host.trim().to_string();
    let user_at_host = if user.is_empty() {
        host.clone()
    } else {
        format!("{user}@{host}")
    };
    vec![
        ("folder", ctx.folder.clone()),
        ("distro", ctx.distro.trim().to_string()),
        ("linuxPath", ctx.linux_path.clone()),
        ("user", user),
        ("host", host),
        ("userAtHost", user_at_host),
        ("port", ctx.port.to_string()),
        ("remoteFolder", ctx.remote_folder.clone()),
    ]
}

/// Rules for a substituted value. Every value comes from state.json, an import or the user's own
/// session: untrusted. Argv quoting already keeps it one element; these stop it being read as
/// something else by the program that receives it.
fn validate_value(name: &str, value: &str) -> Option<String> {
    if value.chars().any(char::is_control) {
        return Some(format!("The session's {name} contains control characters."));
    }
    // An option-shaped value (-x, --foo) could be taken by the editor as a flag. Refused
    // wherever it sits: stricter than needed, never wrong for a path, host or distro.
    if value.starts_with('-') {
        return Some(format!(
            "The session's {name} starts with '-', which the program could read as an option."
        ));
    }
    match name {
        "folder" => {
            if !value.is_empty() && !winpath::is_fully_qualified(value) {
                return Some("The session's folder is not a full path.".to_string());
            }
        }
        "host" | "user" | "userAtHost" | "distro" => {
            if value.chars().any(char::is_whitespace) {
                return Some(format!("The session's {name} contains whitespace."));
            }
        }
        "port" => match value.parse::<i32>() {
            Ok(p) if (1..=65535).contains(&p) => {}
            _ => return Some("The session's port is out of range.".to_string()),
        },
        _ => {}
    }
    None
}

// ── Selection / display ──────────────────────────────────────────────────────────────────

#[must_use]
pub fn applicable_to(list: &[OpenWithEntry], kind: SessionKind) -> Vec<&OpenWithEntry> {
    list.iter().filter(|e| e.applies_to(kind)).collect()
}

/// The default entry when it applies to this kind; otherwise the first entry that does.
#[must_use]
pub fn default_for(list: &[OpenWithEntry], kind: SessionKind) -> Option<&OpenWithEntry> {
    let applicable = applicable_to(list, kind);
    applicable
        .iter()
        .find(|e| e.is_default)
        .or_else(|| applicable.first())
        .copied()
}

/// What a click on `entry` for this session would run, or why it can't: for menu tooltips, so a
/// friendly label can't hide the program or its arguments.
#[must_use]
pub fn describe(entry: &OpenWithEntry, ctx: &OpenWithContext) -> String {
    match build(entry, ctx) {
        Ok(cmd) => cmd.display(),
        Err(error) => format!("{} — {error}", quote_for_cmd(&entry.executable, false)),
    }
}

/// The whole list, spelled out for the import confirmation: each entry's program and the literal
/// argument template for every kind it applies to.
#[must_use]
pub fn describe_for_review(list: &[OpenWithEntry]) -> String {
    let mut out = String::new();
    for e in list {
        out.push_str(if e.is_default { "★ " } else { "• " });
        out.push_str(&e.label);
        out.push('\n');
        for kind in [SessionKind::Local, SessionKind::Wsl, SessionKind::Ssh] {
            let Some(t) = e.args_for(kind).filter(|t| !t.trim().is_empty()) else {
                continue;
            };
            out.push_str(&format!(
                "    {}: {} {t}\n",
                kind_name(kind),
                quote_for_cmd(&e.executable, false)
            ));
        }
    }
    out
}

/// True when two lists would launch the same things (labels, programs, templates, default,
/// order).
#[must_use]
pub fn same_commands(a: &[OpenWithEntry], b: &[OpenWithEntry]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.label == y.label
                && winpath::eq_ic(&x.executable, &y.executable)
                && x.local_args == y.local_args
                && x.wsl_args == y.wsl_args
                && x.ssh_args == y.ssh_args
                && x.is_default == y.is_default
        })
}

// ── Sanitize (state.json / import) ───────────────────────────────────────────────────────

/// A cleaned copy of a list that came from disk: entries with an unacceptable program are
/// dropped; templates that don't validate are cleared (an entry left with none is dropped);
/// labels and ids are filled in; exactly one default. state.json passes `allow_batch = true`
/// (the user may have chosen a batch file in the editor); an import passes `false`. Pure.
/// `log` receives one line per drop or clear.
#[must_use]
pub fn sanitize(
    list: &[Option<OpenWithEntry>],
    allow_batch: bool,
    log: &mut dyn FnMut(&str),
) -> Vec<OpenWithEntry> {
    sanitize_core(list, &|e| validate_executable(e, allow_batch), log)
}

/// [`sanitize`] plus [`validate_executable_on_disk`] per program (no batch files).
#[must_use]
pub fn sanitize_imported(
    list: &[Option<OpenWithEntry>],
    probe: &dyn DiskProbe,
    log: &mut dyn FnMut(&str),
) -> Vec<OpenWithEntry> {
    sanitize_core(list, &|e| validate_executable_on_disk(e, false, probe), log)
}

fn sanitize_core(
    list: &[Option<OpenWithEntry>],
    validate_exe: &dyn Fn(&str) -> Option<String>,
    log: &mut dyn FnMut(&str),
) -> Vec<OpenWithEntry> {
    let mut result: Vec<OpenWithEntry> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for raw in list.iter().flatten() {
        if result.len() >= MAX_ENTRIES {
            log(&format!("dropped entries beyond {MAX_ENTRIES}"));
            break;
        }
        let mut e = raw.clone();
        e.executable = e.executable.trim().to_string();
        if let Some(err) = validate_exe(&e.executable) {
            log(&format!("dropped '{}' ({}): {err}", e.label, e.executable));
            continue;
        }
        e.local_args = clean_template(e.local_args.as_deref(), SessionKind::Local, &e.label, log);
        e.wsl_args = clean_template(e.wsl_args.as_deref(), SessionKind::Wsl, &e.label, log);
        e.ssh_args = clean_template(e.ssh_args.as_deref(), SessionKind::Ssh, &e.label, log);
        if e.local_args.is_none() && e.wsl_args.is_none() && e.ssh_args.is_none() {
            log(&format!(
                "dropped '{}' ({}): no usable arguments for any session kind",
                e.label, e.executable
            ));
            continue;
        }
        e.label = if e.label.trim().is_empty() {
            winpath::file_name_without_extension(&e.executable).to_string()
        } else {
            e.label
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect()
        };
        if e.id.trim().is_empty() || ids.contains(&e.id) {
            e.id = new_id();
        }
        ids.push(e.id.clone());
        result.push(e);
    }
    ensure_single_default(&mut result);
    result
}

fn clean_template(
    template: Option<&str>,
    kind: SessionKind,
    label: &str,
    log: &mut dyn FnMut(&str),
) -> Option<String> {
    let t = template.filter(|t| !t.trim().is_empty())?;
    match validate_template(t, kind) {
        None => Some(t.trim().to_string()),
        Some(err) => {
            log(&format!(
                "'{label}': cleared {} arguments: {err}",
                kind_name(kind)
            ));
            None
        }
    }
}

// ── Shims ────────────────────────────────────────────────────────────────────────────────

/// The Electron exe a known VS Code-family launcher script fronts (`code.cmd` -> `Code.exe`).
#[must_use]
pub fn known_shim_exe(script_stem: &str) -> Option<&'static str> {
    match script_stem.to_lowercase().as_str() {
        "code" => Some("Code.exe"),
        "code-insiders" => Some("Code - Insiders.exe"),
        "cursor" => Some("Cursor.exe"),
        "windsurf" => Some("Windsurf.exe"),
        "codium" => Some("VSCodium.exe"),
        _ => None,
    }
}

/// Where the exe behind a known shim would live: the script's folder and up to three levels up
/// (`<install>\bin\code.cmd`, `<install>\resources\app\bin\cursor.cmd`). The caller checks which
/// exists. Empty for an unknown script.
#[must_use]
pub fn known_shim_candidates(script_path: &str) -> Vec<String> {
    let Some(exe) = known_shim_exe(winpath::file_name_without_extension(script_path)) else {
        return Vec::new();
    };
    if !winpath::is_fully_qualified(script_path) {
        return Vec::new();
    }
    let full = winpath::normalize_full(script_path);
    let root = winpath::path_root(&full);
    let mut dir = parent_dir(&full, &root);
    let mut out = Vec::new();
    for _ in 0..4 {
        let Some(d) = dir else { break };
        let sep = if d.ends_with('\\') { "" } else { "\\" };
        out.push(format!("{d}{sep}{exe}"));
        dir = parent_dir(&d, &root);
    }
    out
}

fn parent_dir(path: &str, root: &str) -> Option<String> {
    if path.len() <= root.len() {
        return None;
    }
    match path.rfind('\\') {
        Some(i) if i >= root.len() => path.get(..i).map(str::to_string),
        Some(_) => Some(root.to_string()),
        None => None,
    }
}

/// Quoted absolute `.exe` paths named in a batch script (`"C:\...\x.exe"`), in order, with
/// duplicates (case-insensitive) removed.
#[must_use]
pub fn script_exe_candidates(script_text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let chars: Vec<char> = script_text.chars().collect();
    let mut i = 0usize;
    while let Some(&c) = chars.get(i) {
        if c != '"' {
            i += 1;
            continue;
        }
        // content up to the next quote on the same line
        let mut j = i + 1;
        while chars
            .get(j)
            .is_some_and(|&x| x != '"' && x != '\r' && x != '\n')
        {
            j += 1;
        }
        let closes = chars.get(j) == Some(&'"');
        let content: String = chars.iter().skip(i + 1).take(j - i - 1).collect();
        let c0 = content.chars().next();
        let ok_shape = closes
            && c0.is_some_and(|d| d.is_ascii_alphabetic())
            && content.chars().nth(1) == Some(':')
            && content.chars().nth(2) == Some('\\')
            && content.to_ascii_lowercase().ends_with(".exe")
            && content.chars().count() > 3 + ".exe".len();
        if ok_shape {
            if !found.iter().any(|f| winpath::eq_ic(f, &content)) {
                found.push(content);
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    found
}

/// The program behind a launcher script, from its text: it resolves only when exactly ONE
/// distinct candidate is acceptable and `exists`; a script that names several programs is
/// ambiguous and returns `None` rather than a guess (JetBrains Toolbox scripts have the
/// one-candidate shape).
#[must_use]
pub fn resolve_shim_from_script(
    script_text: &str,
    exists: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    for exe in script_exe_candidates(script_text) {
        if validate_executable(&exe, false).is_none() && exists(&exe) {
            let full = winpath::normalize_full(&exe);
            if !candidates.iter().any(|c| winpath::eq_ic(c, &full)) {
                candidates.push(full);
            }
        }
    }
    match candidates.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

// ── Prepare ──────────────────────────────────────────────────────────────────────────────

/// Everything [`prepare`] needs from the outside world.
pub trait OpenWithHost: DiskProbe {
    /// The distro's `$HOME` for the user (a WSL probe; may boot the distro).
    fn probe_wsl_home(&self, distro: &str, user: &str) -> Option<String>;
    /// `FolderOpener.Target`: the normalised Windows path when `folder` is an existing
    /// directory, else `None` (a file would be *run* by the shell, never accept it).
    fn folder_target(&self, folder: &str) -> Option<String>;
}

/// Everything a launch does short of starting the process. **Order matters**: everything that
/// can be decided without side effects (program shape, template, values) is validated FIRST,
/// then the program on disk, and only then is a WSL `$HOME` probed. Never spawn `wsl.exe` for a
/// command that was going to be refused anyway.
///
/// # Errors
/// A sentence for the user.
pub fn prepare(
    entry: &OpenWithEntry,
    ctx: &OpenWithContext,
    host: &dyn OpenWithHost,
) -> Result<OpenWithCommand, String> {
    let template = entry.args_for(ctx.kind);
    let uses_folder = template.is_some_and(|t| t.contains("{folder"));
    let lp = ctx.linux_path.trim();
    let needs_home = ctx.kind == SessionKind::Wsl
        && template.is_some_and(|t| t.contains("{linuxPath"))
        && (lp.is_empty() || lp == "~" || lp.starts_with("~/"));
    let after_tilde = lp.strip_prefix('~').unwrap_or("");

    // 1. Pure validation, with a stand-in for the not-yet-resolved home folder.
    let mut ctx = ctx.clone();
    if needs_home {
        let stand_in = OpenWithContext {
            linux_path: format!("/home/stand-in{after_tilde}"),
            ..ctx.clone()
        };
        build(entry, &stand_in)?;
    } else {
        build(entry, &ctx)?;
    }

    // 2. The program on disk (long path, links, network drive, renamed shell).
    if let Some(e) = validate_executable_on_disk(&entry.executable, true, host) {
        return Err(e);
    }
    if !host.file_exists(&entry.executable) {
        return Err(format!(
            "{}: program not found at {}. Edit the entry under Open with → Edit…",
            entry.label, entry.executable
        ));
    }

    // 3. Only now the side-effecting probe: a blank Linux folder means $HOME.
    if needs_home {
        ctx.linux_path = match host.probe_wsl_home(&ctx.distro, &ctx.wsl_user) {
            None => String::new(),
            Some(home) if lp.len() <= 1 => home,
            Some(home) => format!("{}{after_tilde}", home.trim_end_matches('/')),
        };
    }

    // 4. The folder must be an existing directory (same check as Open in Explorer).
    if uses_folder {
        match host.folder_target(&ctx.folder) {
            Some(target) => ctx.folder = target,
            None => {
                return Err(format!(
                    "The session's folder doesn't exist: {}",
                    ctx.folder
                ));
            }
        }
    }

    build(entry, &ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::split_command_line;
    use std::cell::Cell;
    use std::collections::{HashMap, HashSet};

    const EXE: &str = r"C:\Program Files\Microsoft VS Code\Code.exe";

    fn vs() -> OpenWithEntry {
        OpenWithEntry::vscode("VS Code", EXE)
    }

    fn local(folder: &str) -> OpenWithContext {
        OpenWithContext {
            folder: folder.into(),
            ..OpenWithContext::new(SessionKind::Local)
        }
    }

    fn wsl_ctx(distro: &str, linux: &str) -> OpenWithContext {
        OpenWithContext {
            distro: distro.into(),
            linux_path: linux.into(),
            ..OpenWithContext::new(SessionKind::Wsl)
        }
    }

    fn ssh_ctx(user: &str, host: &str, remote: &str) -> OpenWithContext {
        OpenWithContext {
            user: user.into(),
            host: host.into(),
            remote_folder: remote.into(),
            ..OpenWithContext::new(SessionKind::Ssh)
        }
    }

    fn entry(
        exe: &str,
        local: Option<&str>,
        wsl: Option<&str>,
        ssh: Option<&str>,
    ) -> OpenWithEntry {
        OpenWithEntry {
            executable: exe.into(),
            local_args: local.map(Into::into),
            wsl_args: wsl.map(Into::into),
            ssh_args: ssh.map(Into::into),
            ..OpenWithEntry::default()
        }
    }

    fn argv_of(cmd: &OpenWithCommand) -> Vec<String> {
        split_command_line(
            &cmd.arguments
                .iter()
                .map(|a| quote_for_cmd(a, false))
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    // ── Substitution: one placeholder = one argv element ───────────────────────────────

    #[test]
    fn local_folder_is_exactly_one_argument() {
        for folder in [
            r"C:\repos\plain",
            r"C:\repos\has space",
            r"C:\repos\a&calc",
            r"C:\repos\100%PATH%",
            r#"C:\repos\x" --new-window "y"#,
            r"C:\repos\semi;colon,comma",
            r#"C:\repos\back\"slash"#,
            r"C:\repos\trailing\",
            r"C:\repos\$(id)`whoami`",
            r"C:\repos\caret^pipe|lt<gt>",
            r"C:\repos\uniçødé ✓",
        ] {
            let cmd = build(&vs(), &local(folder)).unwrap();
            assert_eq!(cmd.arguments, strs(&[folder]));
            assert_eq!(argv_of(&cmd), strs(&[folder]), "{folder}");
        }
    }

    #[test]
    fn placeholder_inside_a_token_stays_inside_that_token() {
        let e = entry(
            EXE,
            Some("--folder-uri=file:///{folder} --reuse-window"),
            None,
            None,
        );
        let cmd = build(&e, &local(r"C:\a b")).unwrap();
        assert_eq!(
            cmd.arguments,
            strs(&[r"--folder-uri=file:///C:\a b", "--reuse-window"])
        );
        assert_eq!(argv_of(&cmd), cmd.arguments);
    }

    #[test]
    fn wsl_vscode_folder_uri_is_one_encoded_argument() {
        for (linux, uri) in [
            (
                "/home/alice/proj",
                "vscode-remote://wsl+Ubuntu/home/alice/proj",
            ),
            (
                "/home/alice/my proj",
                "vscode-remote://wsl+Ubuntu/home/alice/my%20proj",
            ),
            (
                "/home/alice/$(id)",
                "vscode-remote://wsl+Ubuntu/home/alice/%24%28id%29",
            ),
            (
                "/home/a/x\" --disable-extensions \"y",
                "vscode-remote://wsl+Ubuntu/home/a/x%22%20--disable-extensions%20%22y",
            ),
            (
                "/home/a/#tag?q=1&100%",
                "vscode-remote://wsl+Ubuntu/home/a/%23tag%3Fq%3D1%26100%25",
            ),
            (
                "/home/a/ünï",
                "vscode-remote://wsl+Ubuntu/home/a/%C3%BCn%C3%AF",
            ),
        ] {
            let mut ctx = wsl_ctx("Ubuntu", linux);
            ctx.folder = r"\\wsl$\Ubuntu\home".into();
            let cmd = build(&vs(), &ctx).unwrap();
            let expected = strs(&["--folder-uri", uri]);
            assert_eq!(cmd.arguments, expected);
            assert_eq!(argv_of(&cmd), expected);
            // What VS Code's URI.parse would recover: the original path, intact.
            let tail = &uri["vscode-remote://wsl+Ubuntu".len()..];
            assert_eq!(percent_decode(tail), linux);
        }
    }

    fn percent_decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn wsl_distro_in_the_authority() {
        let cmd = build(&vs(), &wsl_ctx("Ubuntu-22.04", "/srv")).unwrap();
        assert_eq!(cmd.arguments[1], "vscode-remote://wsl+Ubuntu-22.04/srv");
    }

    #[test]
    fn ssh_user_at_host_is_percent_encoded_in_the_authority() {
        // '@' would otherwise be URI user-info; VS Code decodes %40 back to "alice@box".
        let cmd = build(&vs(), &ssh_ctx("alice", "box", "/srv/my app")).unwrap();
        assert_eq!(
            cmd.arguments,
            strs(&[
                "--folder-uri",
                "vscode-remote://ssh-remote+alice%40box/srv/my%20app"
            ])
        );
    }

    #[test]
    fn ssh_host_only() {
        let cmd = build(&vs(), &ssh_ctx("", "box", "/srv")).unwrap();
        assert_eq!(
            cmd.arguments,
            strs(&["--folder-uri", "vscode-remote://ssh-remote+box/srv"])
        );
    }

    #[test]
    fn ssh_no_remote_folder_is_a_clear_error_not_a_different_target() {
        let err = build(&vs(), &ssh_ctx("", "box", "")).unwrap_err();
        assert!(err.contains("remote folder"), "{err}");
    }

    #[test]
    fn empty_standalone_placeholder_token_is_dropped() {
        let e = entry(EXE, None, None, Some("--host {host} {remoteFolder}"));
        let cmd = build(&e, &ssh_ctx("", "box", "")).unwrap();
        assert_eq!(cmd.arguments, strs(&["--host", "box"]));
    }

    // ── '/' and '-' at the start of an argument (Chromium reads /name=value as a switch) ─

    #[test]
    fn slash_leading_value_starting_an_argument_is_refused() {
        for template in [
            "{linuxPath}",
            "--remote wsl+{distro} {linuxPath}", // the pre-review seeded form
            "{linuxPath:uri}",
        ] {
            let e = entry(EXE, None, Some(template), None);
            let err = build(&e, &wsl_ctx("Ubuntu", "/js-flags=--x")).unwrap_err();
            assert!(err.contains("'/'"), "{template}: {err}");
        }
    }

    #[test]
    fn slash_leading_value_inside_an_argument_is_fine() {
        let e = entry(EXE, None, None, Some("--path={remoteFolder}"));
        let cmd = build(&e, &ssh_ctx("", "h", "/srv")).unwrap();
        assert_eq!(cmd.arguments, strs(&["--path=/srv"]));
    }

    #[test]
    fn ssh_port_placeholder() {
        let e = entry(EXE, None, None, Some("--host {host} --port {port}"));
        let mut ctx = ssh_ctx("", "h", "");
        ctx.port = 2222;
        assert_eq!(
            build(&e, &ctx).unwrap().arguments,
            strs(&["--host", "h", "--port", "2222"])
        );
    }

    #[test]
    fn quoted_template_token_keeps_spaces() {
        let e = entry(EXE, Some("\"--title=My Editor\" {folder}"), None, None);
        assert_eq!(
            build(&e, &local(r"C:\x")).unwrap().arguments,
            strs(&["--title=My Editor", r"C:\x"])
        );
    }

    // ── Value rules ────────────────────────────────────────────────────────────────────

    #[test]
    fn option_shaped_values_are_refused() {
        let cases: [(SessionKind, &str, &str); 3] = [
            (SessionKind::Ssh, "-oProxyCommand=calc", ""),
            (SessionKind::Ssh, "box", "--help"),
            (SessionKind::Wsl, "-x", ""),
        ];
        for (kind, first, second) in cases {
            let ctx = if kind == SessionKind::Ssh {
                ssh_ctx("", first, second)
            } else {
                wsl_ctx(first, "/home")
            };
            let err = build(&vs(), &ctx).unwrap_err();
            assert!(err.contains("'-'"), "{first}: {err}");
        }
    }

    #[test]
    fn host_with_whitespace_or_control_chars_is_refused() {
        for host in ["bad host", "host\nx", "host\0"] {
            assert!(build(&vs(), &ssh_ctx("", host, "")).is_err(), "{host:?}");
        }
    }

    #[test]
    fn local_folder_must_be_a_full_path_without_control_chars() {
        for folder in [r"relative\path", "C:\\a\\r\\nb\n"] {
            assert!(build(&vs(), &local(folder)).is_err(), "{folder:?}");
        }
    }

    #[test]
    fn port_out_of_range_is_refused() {
        for port in [0, 70000, -1] {
            let e = entry(EXE, None, None, Some("{host}:{port}"));
            let mut ctx = ssh_ctx("", "h", "");
            ctx.port = port;
            assert!(build(&e, &ctx).is_err(), "{port}");
        }
    }

    // ── Templates ──────────────────────────────────────────────────────────────────────

    #[test]
    fn tokenize_cases() {
        for (template, expected) in [
            ("{folder}", vec!["{folder}"]),
            ("  a   b  ", vec!["a", "b"]),
            ("\"a b\" c", vec!["a b", "c"]),
            ("x\"y z\"w", vec!["xy zw"]),
            ("\"\"", vec![""]),
            ("", vec![]),
        ] {
            assert_eq!(tokenize(template), Some(strs(&expected)), "{template:?}");
        }
    }

    #[test]
    fn tokenize_unbalanced_quote_is_none() {
        assert_eq!(tokenize("a \"b"), None);
    }

    #[test]
    fn invalid_templates_are_reported() {
        for (template, kind) in [
            ("{host}", SessionKind::Local),
            ("{linuxPath}", SessionKind::Local),
            ("{folder}", SessionKind::Ssh),
            ("{bogus}", SessionKind::Wsl),
            ("\"{folder}", SessionKind::Local),
            ("--literal-only", SessionKind::Local), // must open the session's location
            ("/c calc", SessionKind::Local),
        ] {
            assert!(validate_template(template, kind).is_some(), "{template}");
        }
    }

    #[test]
    fn valid_templates_pass() {
        for (template, kind) in [
            ("{folder}", SessionKind::Local),
            ("--remote wsl+{distro} {linuxPath}", SessionKind::Wsl),
            ("{folder}", SessionKind::Wsl),
            (
                "--remote ssh-remote+{userAtHost} {remoteFolder}",
                SessionKind::Ssh,
            ),
            ("", SessionKind::Ssh),
            (
                "--folder-uri vscode-remote://wsl+{distro:uri}{linuxPath:uri}",
                SessionKind::Wsl,
            ),
        ] {
            assert_eq!(validate_template(template, kind), None, "{template}");
        }
    }

    #[test]
    fn placeholder_grammar_edges() {
        // `{folder:uriX}` and `{folder:}` are not placeholders at all.
        assert!(find_placeholders("{folder:uriX}").is_empty());
        assert!(find_placeholders("{folder:}").is_empty());
        assert!(find_placeholders("{}").is_empty());
        assert!(find_placeholders("{a1}").is_empty()); // letters only
        let m = find_placeholders("x{folder}y{distro:uri}");
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].start, m[0].end, m[0].uri), (1, 9, false));
        assert!(m[1].uri);
        assert!(validate_template("{folder:uriX}", SessionKind::Local).is_some());
    }

    #[test]
    fn control_chars_in_a_template_are_refused() {
        assert!(
            validate_template("{folder}\u{7}", SessionKind::Local)
                .unwrap()
                .contains("control")
        );
    }

    #[test]
    fn build_kind_without_template_is_an_error() {
        let mut e = entry(EXE, Some("{folder}"), None, None);
        e.label = "VS".into();
        let err = build(&e, &ssh_ctx("", "h", "")).unwrap_err();
        assert!(err.contains("SSH"), "{err}");
    }

    // ── Executable ─────────────────────────────────────────────────────────────────────

    #[test]
    fn validate_executable_cases() {
        for (path, allow_batch, ok) in [
            (r"C:\Tools\editor.exe", false, true),
            (r"C:\Tools\EDITOR.EXE", false, true),
            ("editor.exe", false, false), // relative: would resolve via PATH/cwd
            ("C:editor.exe", false, false), // drive-relative
            (r"\\attacker\share\editor.exe", false, false), // network
            (r"\\?\C:\Tools\editor.exe", false, false), // device path
            (r"C:\Tools\script.ps1", false, false),
            (r"C:\Tools\thing.com", false, false),
            (r"C:\Tools\launch.cmd", false, false),
            (r"C:\Tools\launch.cmd", true, true),
            (r"C:\Tools\launch.bat", true, true),
            (r"C:\Windows\System32\cmd.exe", true, false),
            (
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                true,
                false,
            ),
            (r"C:\Program Files\PowerShell\7\pwsh.exe", true, false),
            (r"C:\Windows\System32\wsl.exe", true, false),
            (r"C:\Windows\System32\mshta.exe", true, false),
            (r"C:\Windows\System32\rundll32.exe", true, false),
            (r"C:\Windows\System32\cmd.cmd", true, false),
            ("", true, false),
        ] {
            assert_eq!(
                validate_executable_with(path, allow_batch, r"C:\Windows").is_none(),
                ok,
                "{path} batch={allow_batch}"
            );
        }
    }

    #[test]
    fn system_root_programs_are_refused() {
        for path in [
            r"C:\Windows\System32\OpenSSH\ssh.exe",
            r"C:\Windows\System32\wbem\WMIC.exe",
            r"C:\Windows\System32\pcalua.exe",
            r"C:\Windows\System32\mmc.exe",
            r"C:\Windows\hh.exe",
            r"C:\Windows\explorer.exe",
            r"C:\Windows\Microsoft.NET\Framework64\v4.0.30319\MSBuild.exe",
            r"C:\Windows\Microsoft.NET\Framework64\v4.0.30319\InstallUtil.exe",
            r"C:\Windows\System32\odbcconf.exe",
            r"c:\WINDOWS\system32\notepad.exe",
            r"C:\Tools\..\Windows\System32\anything.exe",
            // Win32 drops trailing dots and spaces from a segment before it resolves
            r"C:\Windows .\System32\anything.exe",
            r"C:/Windows/System32/anything.exe",
        ] {
            assert!(
                validate_executable_with(path, true, r"C:\Windows").is_some(),
                "{path}"
            );
        }
        // a sibling folder that merely starts with the root's name is fine
        assert!(validate_executable_with(r"C:\WindowsApps\x.exe", true, r"C:\Windows").is_none());
    }

    #[test]
    fn command_running_tools_outside_system_root_are_refused_by_name() {
        for path in [
            r"C:\Tools\ssh.exe",
            r"C:\Tools\wmic.exe",
            r"C:\Tools\explorer.exe",
            r"C:\Tools\MSBuild.exe",
            r"C:\Tools\node.exe",
        ] {
            assert!(
                validate_executable_with(path, true, r"C:\Windows").is_some(),
                "{path}"
            );
        }
    }

    // ── on-disk checks via a fake probe ────────────────────────────────────────────────

    #[derive(Default)]
    struct FakeHost {
        network_drives: HashSet<String>,
        files: HashSet<String>,
        final_paths: HashMap<String, String>,
        version_names: HashMap<String, Vec<String>>,
        home: Option<String>,
        folders: HashMap<String, String>,
        probes: Cell<u32>,
    }

    impl DiskProbe for FakeHost {
        fn drive_is_network(&self, root: &str) -> bool {
            self.network_drives.contains(&root.to_uppercase())
        }
        fn file_exists(&self, path: &str) -> bool {
            self.files.contains(path)
        }
        fn final_path(&self, path: &str) -> Option<String> {
            self.final_paths.get(path).cloned()
        }
        fn version_names(&self, path: &str) -> Vec<String> {
            self.version_names.get(path).cloned().unwrap_or_default()
        }
    }

    impl OpenWithHost for FakeHost {
        fn probe_wsl_home(&self, _distro: &str, _user: &str) -> Option<String> {
            self.probes.set(self.probes.get() + 1);
            self.home.clone()
        }
        fn folder_target(&self, folder: &str) -> Option<String> {
            self.folders.get(folder).cloned()
        }
    }

    const TMP_EXE: &str = r"C:\Users\u\AppData\Local\Temp\csm\Code.exe";

    fn host_with_exe() -> FakeHost {
        let mut h = FakeHost {
            home: Some("/home/u".into()),
            ..FakeHost::default()
        };
        h.files.insert(TMP_EXE.into());
        h
    }

    #[test]
    fn short_name_of_a_shell_is_caught_on_disk() {
        // The pure, name-only check is fooled by POWERS~1.EXE: that is the bypass...
        let short = r"C:\Users\u\A_LONG~1\POWERS~1.EXE";
        let long = r"C:\Users\u\A Long Folder Name\powershell.exe";
        assert_eq!(validate_executable(short, false), None);
        // ...and the on-disk check resolves the real name first.
        let mut h = FakeHost::default();
        h.files.insert(short.into());
        h.final_paths.insert(short.into(), long.into());
        let err = validate_executable_on_disk(short, false, &h).unwrap();
        assert!(err.contains("powershell.exe"), "{err}");
    }

    #[test]
    fn renamed_copy_of_a_shell_is_caught_by_its_version_info() {
        let copy = r"C:\Users\u\Temp\editor.exe";
        let mut h = FakeHost::default();
        h.files.insert(copy.into());
        h.version_names
            .insert(copy.into(), vec!["".into(), "PowerShell.EXE.mui".into()]);
        assert_eq!(validate_executable(copy, false), None);
        let err = validate_executable_on_disk(copy, false, &h).unwrap();
        assert!(err.contains("copy of"), "{err}");
        // an ordinary editor's version info passes
        h.version_names
            .insert(copy.into(), vec!["Code.exe".into(), "code".into()]);
        assert_eq!(validate_executable_on_disk(copy, false, &h), None);
    }

    #[test]
    fn network_drive_is_refused() {
        let mut h = FakeHost::default();
        h.network_drives.insert(r"Z:\".into());
        let err = validate_executable_on_disk(r"Z:\tools\editor.exe", false, &h).unwrap();
        assert!(err.contains("network"), "{err}");
    }

    #[test]
    fn program_resolving_to_a_unc_or_network_location_is_refused() {
        let p = r"C:\Tools\editor.exe";
        let mut h = FakeHost::default();
        h.files.insert(p.into());
        h.final_paths
            .insert(p.into(), r"\\server\share\editor.exe".into());
        assert!(
            validate_executable_on_disk(p, false, &h)
                .unwrap()
                .contains("network location")
        );
        h.final_paths.insert(p.into(), r"Y:\editor.exe".into());
        h.network_drives.insert(r"Y:\".into());
        assert!(
            validate_executable_on_disk(p, false, &h)
                .unwrap()
                .contains("network drive")
        );
    }

    #[test]
    fn local_program_passes_on_disk_and_a_missing_one_passes_until_launch() {
        let h = host_with_exe();
        assert_eq!(validate_executable_on_disk(TMP_EXE, false, &h), None);
        assert_eq!(
            validate_executable_on_disk(r"C:\nope\x.exe", false, &h),
            None
        );
    }

    // ── BatBadBut ──────────────────────────────────────────────────────────────────────

    #[test]
    fn batch_target_refuses_cmd_metacharacters() {
        for folder in [
            r"C:\repos\a&calc",
            r"C:\repos\100%PATH%",
            r"C:\repos\bang!",
            r#"C:\repos\x"y"#,
            r"C:\repos\a|b",
            r"C:\repos\a^b",
            r"C:\repos\(a)",
            r"C:\repos\a,b",
            r"C:\repos\a;b",
            r"C:\repos\a=b",
            r"C:\repos\a<b>",
        ] {
            let mut e = entry(r"C:\Tools\code.cmd", Some("{folder}"), None, None);
            e.label = "shim".into();
            let err = build(&e, &local(folder)).unwrap_err();
            assert!(err.contains("cmd.exe"), "{folder}: {err}");
        }
    }

    #[test]
    fn batch_target_plain_folder_is_allowed() {
        let mut e = entry(r"C:\Tools\code.cmd", Some("{folder}"), None, None);
        e.label = "shim".into();
        let cmd = build(&e, &local(r"C:\repos\my project")).unwrap();
        assert_eq!(cmd.arguments, strs(&[r"C:\repos\my project"]));
    }

    #[test]
    fn exe_target_does_not_apply_the_batch_restriction() {
        assert!(build(&vs(), &local(r"C:\repos\a&b%c%")).is_ok());
    }

    #[test]
    fn command_is_argv_only() {
        // The launcher gets the program and an argv; there is no command-line string to parse.
        let cmd = build(&vs(), &local(r"C:\a b&c")).unwrap();
        assert_eq!(cmd.executable, EXE);
        assert_eq!(cmd.arguments, strs(&[r"C:\a b&c"]));
        assert_eq!(cmd.display(), format!("\"{EXE}\" \"C:\\a b&c\""));
    }

    // ── Selection ──────────────────────────────────────────────────────────────────────

    #[test]
    fn default_for_falls_back_to_first_applicable_entry() {
        let mut vs_entry = entry(EXE, Some("{folder}"), None, None);
        vs_entry.label = "VS".into();
        vs_entry.is_default = true;
        let code = vs();
        let list = vec![vs_entry.clone(), code.clone()];
        assert_eq!(default_for(&list, SessionKind::Local), Some(&list[0]));
        assert_eq!(default_for(&list, SessionKind::Wsl), Some(&list[1]));
        assert_eq!(applicable_to(&list, SessionKind::Ssh), vec![&list[1]]);
        assert_eq!(default_for(&[], SessionKind::Local), None);
    }

    #[test]
    fn context_from_spec_wsl() {
        let s = LaunchSpec {
            kind: SessionKind::Wsl,
            wsl_distro: "Ubuntu".into(),
            wsl_working_folder: "/home/a".into(),
            working_folder: r"\\wsl$\Ubuntu\home\a".into(),
            ..LaunchSpec::default()
        };
        let ctx = OpenWithContext::from_spec(&s);
        assert!(ctx.has_target());
        assert_eq!(ctx.distro, "Ubuntu");
        assert_eq!(ctx.linux_path, "/home/a");
        let ssh = LaunchSpec {
            kind: SessionKind::Ssh,
            ..LaunchSpec::default()
        };
        assert!(!OpenWithContext::from_spec(&ssh).has_target());
    }

    // ── Sanitize / import trust ────────────────────────────────────────────────────────

    fn labelled(label: &str, exe: &str, local: Option<&str>) -> Option<OpenWithEntry> {
        let mut e = entry(exe, local, None, None);
        e.label = label.into();
        Some(e)
    }

    fn quiet() -> impl FnMut(&str) {
        |_| {}
    }

    #[test]
    fn sanitize_drops_shells_relative_network_and_on_import_batch() {
        let list = vec![
            None,
            labelled("ok", r"C:\Tools\ed.exe", Some("{folder}")),
            labelled("shell", r"C:\Windows\System32\cmd.exe", Some("/c calc")),
            labelled("rel", "calc.exe", Some("{folder}")),
            labelled("net", r"\\evil\x\ed.exe", Some("{folder}")),
            labelled("bat", r"C:\Tools\ed.cmd", Some("{folder}")),
        ];
        let labels = |v: Vec<OpenWithEntry>| v.into_iter().map(|e| e.label).collect::<Vec<_>>();
        assert_eq!(labels(sanitize(&list, false, &mut quiet())), ["ok"]);
        assert_eq!(labels(sanitize(&list, true, &mut quiet())), ["ok", "bat"]);
    }

    #[test]
    fn sanitize_clears_bad_templates_drops_entries_left_with_none_fixes_ids_and_default() {
        let mk = |id: &str,
                  label: &str,
                  exe: &str,
                  local: Option<&str>,
                  wsl: Option<&str>,
                  ssh: Option<&str>,
                  def: bool| {
            let mut e = entry(exe, local, wsl, ssh);
            e.id = id.into();
            e.label = label.into();
            e.is_default = def;
            Some(e)
        };
        let list = vec![
            mk(
                "x",
                " A ",
                r"C:\a.exe",
                Some("{folder}"),
                None,
                Some("{folder}"),
                false,
            ),
            mk("x", "", r"C:\b.exe", None, Some("{distro}"), None, true),
            mk("y", "C", r"C:\c.exe", Some("{nope}"), None, None, false),
            mk("z", "D", r"C:\d.exe", Some("{folder}"), None, None, true),
        ];
        let mut log = Vec::new();
        let result = sanitize(&list, false, &mut |m| log.push(m.to_string()));
        let labels: Vec<_> = result.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["A", "b", "D"]);
        assert_eq!(result[0].ssh_args, None);
        assert_eq!(result[0].local_args.as_deref(), Some("{folder}"));
        let ids: HashSet<_> = result.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(result.iter().filter(|e| e.is_default).count(), 1);
        assert!(result[1].is_default);
        assert!(log.iter().any(|l| l.contains("cleared SSH arguments")));
        assert!(log.iter().any(|l| l.contains("dropped 'C'")));
    }

    #[test]
    fn sanitize_caps_entries() {
        let list: Vec<Option<OpenWithEntry>> = (0..80)
            .map(|i| labelled(&format!("e{i}"), &format!(r"C:\e{i}.exe"), Some("{folder}")))
            .collect();
        assert_eq!(sanitize(&list, true, &mut quiet()).len(), MAX_ENTRIES);
    }

    #[test]
    fn sanitize_label_is_stripped_of_control_chars_and_capped() {
        let long = format!("{}\u{7}x", "a".repeat(100));
        let list = vec![labelled(&long, r"C:\a.exe", Some("{folder}"))];
        let result = sanitize(&list, true, &mut quiet());
        assert_eq!(result[0].label.chars().count(), 80);
        assert!(!result[0].label.contains('\u{7}'));
    }

    #[test]
    fn import_drops_batch_and_shell_entries() {
        let list = vec![
            Some(vs()),
            labelled("shim", r"C:\Tools\code.cmd", Some("{folder}")),
            labelled(
                "evil",
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                Some("-c calc"),
            ),
        ];
        let h = FakeHost::default();
        let result = sanitize_imported(&list, &h, &mut quiet());
        let labels: Vec<_> = result.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["VS Code"]);
    }

    #[test]
    fn import_checks_the_real_program_on_disk() {
        let mut h = FakeHost::default();
        let sneaky = r"C:\Users\u\Temp\editor.exe";
        h.files.insert(sneaky.into());
        h.version_names
            .insert(sneaky.into(), vec!["cmd.exe".into()]);
        let list = vec![labelled("sneaky", sneaky, Some("{folder}"))];
        assert!(sanitize_imported(&list, &h, &mut quiet()).is_empty());
    }

    #[test]
    fn ensure_single_default_keeps_the_first_flagged_or_the_first() {
        let mut list = vec![
            OpenWithEntry::default(),
            OpenWithEntry {
                is_default: true,
                ..OpenWithEntry::default()
            },
            OpenWithEntry {
                is_default: true,
                ..OpenWithEntry::default()
            },
        ];
        ensure_single_default(&mut list);
        assert_eq!(
            list.iter().map(|e| e.is_default).collect::<Vec<_>>(),
            [false, true, false]
        );
        let mut none = vec![OpenWithEntry::default(), OpenWithEntry::default()];
        ensure_single_default(&mut none);
        assert_eq!(
            none.iter().map(|e| e.is_default).collect::<Vec<_>>(),
            [true, false]
        );
        ensure_single_default(&mut []);
    }

    #[test]
    fn new_ids_are_distinct_hex() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ── Prepare (disk checks through a fake host) ──────────────────────────────────────

    fn vs_at(exe: &str) -> OpenWithEntry {
        OpenWithEntry::vscode("VS Code", exe)
    }

    #[test]
    fn prepare_missing_folder_is_refused() {
        let h = host_with_exe();
        let err = prepare(&vs_at(TMP_EXE), &local(r"C:\nope"), &h).unwrap_err();
        assert!(err.contains("doesn't exist"), "{err}");
    }

    #[test]
    fn prepare_folder_that_is_a_file_is_refused() {
        // FolderOpener.Target returns None for a file (the shell would *run* it).
        let h = host_with_exe();
        assert!(prepare(&vs_at(TMP_EXE), &local(r"C:\x\evil.exe"), &h).is_err());
    }

    #[test]
    fn prepare_missing_program_is_reported() {
        let h = host_with_exe();
        let err = prepare(&vs_at(r"C:\missing\Code.exe"), &local(r"C:\x"), &h).unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn prepare_normalizes_the_folder_through_the_host() {
        let mut h = host_with_exe();
        h.folders
            .insert("C:/t/wt feat/".into(), r"C:\t\wt feat".into());
        let cmd = prepare(&vs_at(TMP_EXE), &local("C:/t/wt feat/"), &h).unwrap();
        assert_eq!(cmd.arguments, strs(&[r"C:\t\wt feat"]));
    }

    #[test]
    fn prepare_refuses_a_network_drive_program_before_anything_else() {
        let h = host_with_exe();
        let err = prepare(&vs_at(r"\\server\share\Code.exe"), &local(r"C:\x"), &h).unwrap_err();
        assert!(err.contains("Network"), "{err}");
    }

    #[test]
    fn prepare_refuses_a_mapped_network_drive_program() {
        let mut h = host_with_exe();
        h.network_drives.insert(r"Z:\".into());
        h.files.insert(r"Z:\tools\Code.exe".into());
        let err = prepare(&vs_at(r"Z:\tools\Code.exe"), &local(r"C:\x"), &h).unwrap_err();
        assert!(err.contains("network drive"), "{err}");
    }

    #[test]
    fn prepare_invalid_command_never_probes_wsl() {
        let cases: [(Option<&str>, &str); 3] = [
            (Some(r"C:\Windows\System32\cmd.exe"), "Ubuntu"), // refused program
            (None, "-x"),                                     // option-shaped distro
            (None, "bad distro"),                             // whitespace in distro
        ];
        for (exe, distro) in cases {
            let h = host_with_exe();
            let entry = vs_at(exe.unwrap_or(TMP_EXE));
            assert!(prepare(&entry, &wsl_ctx(distro, ""), &h).is_err());
            assert_eq!(h.probes.get(), 0, "{distro}");
        }
    }

    #[test]
    fn prepare_missing_program_never_probes_wsl() {
        let h = host_with_exe();
        let _ = prepare(&vs_at(r"C:\absent\Code.exe"), &wsl_ctx("Ubuntu", ""), &h);
        assert_eq!(h.probes.get(), 0);
    }

    #[test]
    fn prepare_valid_command_probes_home_once() {
        for (linux, uri) in [
            ("", "vscode-remote://wsl+Ubuntu/home/u"),
            ("~", "vscode-remote://wsl+Ubuntu/home/u"),
            ("~/my proj", "vscode-remote://wsl+Ubuntu/home/u/my%20proj"),
        ] {
            let h = host_with_exe();
            let cmd = prepare(&vs_at(TMP_EXE), &wsl_ctx("Ubuntu", linux), &h).unwrap();
            assert_eq!(h.probes.get(), 1, "{linux}");
            assert_eq!(cmd.arguments, strs(&["--folder-uri", uri]));
        }
    }

    #[test]
    fn prepare_absolute_linux_path_does_not_probe() {
        let h = host_with_exe();
        let cmd = prepare(&vs_at(TMP_EXE), &wsl_ctx("Ubuntu", "/srv/app"), &h).unwrap();
        assert_eq!(h.probes.get(), 0);
        assert_eq!(cmd.arguments[1], "vscode-remote://wsl+Ubuntu/srv/app");
    }

    #[test]
    fn prepare_home_probe_fails_is_a_clear_error() {
        let mut h = host_with_exe();
        h.home = None;
        let err = prepare(&vs_at(TMP_EXE), &wsl_ctx("Ubuntu", ""), &h).unwrap_err();
        assert!(err.contains("Linux folder"), "{err}");
    }

    // ── Shims ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn shim_vscode_layout() {
        let c = known_shim_candidates(r"C:\t\Microsoft VS Code\bin\code.cmd");
        assert_eq!(c[0], r"C:\t\Microsoft VS Code\bin\Code.exe");
        assert_eq!(c[1], r"C:\t\Microsoft VS Code\Code.exe");
    }

    #[test]
    fn shim_cursor_layout_three_levels_up() {
        let c = known_shim_candidates(r"C:\t\cursor\resources\app\bin\cursor.cmd");
        assert_eq!(c.len(), 4);
        assert_eq!(c[3], r"C:\t\cursor\Cursor.exe");
    }

    #[test]
    fn shim_unknown_or_non_absolute_script_has_no_candidates() {
        assert!(known_shim_candidates(r"C:\t\rider.cmd").is_empty());
        assert!(known_shim_candidates(r"bin\code.cmd").is_empty());
        assert_eq!(known_shim_exe("CODE-INSIDERS"), Some("Code - Insiders.exe"));
        assert_eq!(known_shim_exe("windsurf"), Some("Windsurf.exe"));
        assert_eq!(known_shim_exe("codium"), Some("VSCodium.exe"));
    }

    #[test]
    fn shim_candidates_stop_at_the_drive_root() {
        let c = known_shim_candidates(r"C:\code.cmd");
        assert_eq!(c, [r"C:\Code.exe"]);
    }

    #[test]
    fn shim_generic_script_with_quoted_exe() {
        let exe = r"C:\Tools\rider64.exe";
        let script = format!("@echo off\r\nstart \"\" \"{exe}\" %*\r\n");
        let exists = |p: &str| p == exe;
        assert_eq!(
            resolve_shim_from_script(&script, &exists).as_deref(),
            Some(exe)
        );
    }

    #[test]
    fn shim_generic_script_ignores_shells_and_missing_exes() {
        let script = "\"C:\\Windows\\System32\\cmd.exe\" /c \"C:\\does\\not\\exist.exe\"";
        let exists = |p: &str| p.contains("cmd.exe");
        assert_eq!(resolve_shim_from_script(script, &exists), None);
    }

    #[test]
    fn shim_two_candidates_is_ambiguous() {
        let (a, b) = (r"C:\Tools\rider64.exe", r"C:\Tools\other.exe");
        let script = format!(
            "@echo off\r\nif exist \"{b}\" start \"\" \"{b}\" %*\r\nstart \"\" \"{a}\" %*\r\n"
        );
        let exists = |_: &str| true;
        assert_eq!(resolve_shim_from_script(&script, &exists), None);
    }

    #[test]
    fn shim_same_candidate_twice_is_still_one() {
        let a = r"C:\Tools\rider64.exe";
        let script = format!("@echo off\r\nif exist \"{a}\" start \"\" \"{a}\" %*\r\n");
        let exists = |_: &str| true;
        assert_eq!(
            resolve_shim_from_script(&script, &exists).as_deref(),
            Some(a)
        );
        // a case difference is the same program
        let script2 = format!("\"{a}\" \"{}\"", a.to_uppercase());
        assert_eq!(
            resolve_shim_from_script(&script2, &exists).as_deref(),
            Some(a)
        );
    }

    #[test]
    fn script_candidate_scanner_edges() {
        assert!(script_exe_candidates("\"C:\\a.exe").is_empty()); // unterminated
        assert!(script_exe_candidates("\"x:\\a.txt\"").is_empty());
        assert!(script_exe_candidates("\"relative\\a.exe\"").is_empty());
        assert!(script_exe_candidates("\"C:\\.exe\"").is_empty());
        assert_eq!(
            script_exe_candidates("echo \"foo\" \"C:\\x\\a.EXE\""),
            [r"C:\x\a.EXE"]
        );
        let _ = script_exe_candidates("\"é\"\"\"\"");
    }

    #[test]
    fn non_batch_is_not_a_shim() {
        assert!(!is_batch(EXE));
        assert!(is_batch(r"C:\x\launch.CMD"));
        assert!(is_batch(r"C:\x\launch.bat"));
    }

    // ── Import review ──────────────────────────────────────────────────────────────────

    #[test]
    fn describe_for_review_shows_program_and_every_template() {
        let list = vec![OpenWithEntry {
            label: "VS Code".into(),
            executable: r"C:\Tools\evil.exe".into(),
            local_args: Some("--run {folder}".into()),
            ssh_args: Some("x {host}".into()),
            is_default: true,
            ..OpenWithEntry::default()
        }];
        let text = describe_for_review(&list);
        assert!(text.contains("★ VS Code"));
        assert!(text.contains(r"local: C:\Tools\evil.exe --run {folder}"));
        assert!(text.contains(r"SSH: C:\Tools\evil.exe x {host}"));
        assert!(!text.contains("WSL"));
    }

    #[test]
    fn same_commands_detects_any_launch_relevant_change() {
        let a = vec![OpenWithEntry::vscode("VS Code", r"C:\a\Code.exe")];
        let mut b = vec![OpenWithEntry::vscode("VS Code", r"C:\A\code.exe")];
        assert!(same_commands(&a, &b)); // program compare ignores case
        b[0].local_args = Some("--x {folder}".into());
        assert!(!same_commands(&a, &b));
        assert!(!same_commands(&a, &[]));
    }

    #[test]
    fn describe_shows_resolved_command_for_the_session() {
        let e = OpenWithEntry {
            label: "VS Code".into(),
            executable: r"C:\Tools\evil.exe".into(),
            local_args: Some("--run {folder}".into()),
            ..OpenWithEntry::default()
        };
        assert_eq!(
            describe(&e, &local(r"C:\repo")),
            r"C:\Tools\evil.exe --run C:\repo"
        );
        // an unbuildable command is described with its reason instead
        let msg = describe(&e, &local("relative"));
        assert!(msg.starts_with(r"C:\Tools\evil.exe — "), "{msg}");
    }

    #[test]
    fn escape_data_string_matches_uri_escape_data_string() {
        assert_eq!(escape_data_string("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(escape_data_string(" /@!*'()"), "%20%2F%40%21%2A%27%28%29");
        assert_eq!(escape_data_string("é"), "%C3%A9");
        assert_eq!(escape_data_string("😀"), "%F0%9F%98%80");
        assert_eq!(escape_data_string(""), "");
        // '/' is kept only by the path placeholders
        assert_eq!(encode("linuxPath", "/a b/", true), "/a%20b/");
        assert_eq!(encode("distro", "a/b", true), "a%2Fb");
        assert_eq!(encode("distro", "a/b", false), "a/b");
    }
}

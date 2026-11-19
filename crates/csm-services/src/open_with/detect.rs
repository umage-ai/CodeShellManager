//! Finding installed editors and IDEs to seed the "Open with" list (port of `OpenWithDetector`,
//! plus macOS and Linux). Pure: every question about the machine (environment, files, folders,
//! the registry's App Paths, vswhere's JSON) goes through [`DetectEnv`], so the rules are tested
//! against fake layouts. The real environment does disk, registry and process I/O: run it on a
//! worker thread, never the UI or engine thread.
//!
//! - **Windows:** the VS Code family (PATH shim → the real exe, never the `.cmd`; App Paths;
//!   `%LOCALAPPDATA%\Programs`, Program Files, Scoop), Visual Studio via vswhere, JetBrains
//!   (standalone, Toolbox 1.x `apps\…\ch-N\<build>\bin` and 2.x `Programs\<IDE>\bin`; the
//!   `*64.exe`, newest per product), Zed, Sublime Text and Notepad++.
//! - **macOS:** `/Applications` and `~/Applications` app bundles, opened with
//!   `/usr/bin/open -a "<bundle>" {folder}` (the VS Code family also gets the SSH remote form,
//!   passed with `--args`).
//! - **Linux:** `.desktop` files (the user's, the system's, snap's and flatpak's exports), whose
//!   `Exec=` program is used when it is an absolute or PATH-resolved binary; the VS Code family's
//!   `bin/<name>` shell wrapper is swapped for the Electron binary beside it; JetBrains Toolbox's
//!   native launchers; then PATH for Zed and Sublime Text.
//!
//! Every candidate must pass the platform's program rules ([`super::validate_executable_for`]);
//! the list is deduplicated by program and ends with exactly one default (VS Code first when
//! present, as before).

use super::{OpenWithEntry, Rules, VSCODE_LOCAL_ARGS, VSCODE_SSH_ARGS, VSCODE_WSL_ARGS, ensure_single_default, resolve_shim_from_script, validate_executable_for};
use crate::winpath;

/// The platform whose install layout is searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Mac,
    Linux,
}

impl Platform {
    #[must_use]
    pub fn native() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Linux
        }
    }

    fn rules(self) -> Rules {
        if self == Platform::Windows { Rules::Windows } else { Rules::Posix }
    }

    fn sep(self) -> char {
        if self == Platform::Windows { '\\' } else { '/' }
    }
}

/// Everything detection asks the machine. Paths are strings in the platform's own form.
pub trait DetectEnv {
    fn platform(&self) -> Platform;
    /// An environment variable (`PATH`, `LOCALAPPDATA`, `ProgramFiles`, `ProgramFiles(x86)`,
    /// `USERPROFILE`, `HOME`, `XDG_DATA_HOME`, `XDG_DATA_DIRS`).
    fn var(&self, name: &str) -> Option<String>;
    fn is_file(&self, path: &str) -> bool;
    fn is_dir(&self, path: &str) -> bool;
    /// Full paths of the entries directly inside `dir` (files and folders), any order.
    fn list(&self, dir: &str) -> Vec<String>;
    /// A small text file (a `.cmd` shim, a `.desktop` file); `None` when unreadable or large.
    fn read_text(&self, path: &str) -> Option<String>;
    /// Last-write time, any monotonic unit (newest JetBrains build wins).
    fn modified(&self, path: &str) -> Option<u64>;
    /// Windows App Paths: exe file name → full path (HKCU, then HKLM). `None` elsewhere.
    fn app_path(&self, exe_name: &str) -> Option<String>;
    /// vswhere's `-format json` output, or `None` (no vswhere, or not Windows).
    fn vswhere_json(&self) -> Option<String>;
    /// The real path behind a link (Linux `/usr/bin/code` → `/usr/share/code/bin/code`).
    fn real_path(&self, path: &str) -> Option<String>;
}

fn join(p: Platform, a: &str, b: &str) -> String {
    let sep = p.sep();
    if a.is_empty() {
        return b.to_string();
    }
    if a.ends_with(sep) || (p == Platform::Windows && a.ends_with('/')) {
        format!("{a}{b}")
    } else {
        format!("{a}{sep}{b}")
    }
}

fn leaf(p: Platform, path: &str) -> &str {
    let t = path.trim_end_matches(p.sep());
    let i = if p == Platform::Windows { t.rfind(['\\', '/']) } else { t.rfind('/') };
    match i {
        Some(i) => t.get(i + 1..).unwrap_or(""),
        None => t,
    }
}

fn parent(p: Platform, path: &str) -> Option<String> {
    let t = path.trim_end_matches(p.sep());
    let i = if p == Platform::Windows { t.rfind(['\\', '/']) } else { t.rfind('/') }?;
    let up = t.get(..i)?;
    if up.is_empty() { (p != Platform::Windows).then(|| "/".to_string()) } else { Some(up.to_string()) }
}

/// The VS Code family: label, Windows shim / exe, Windows install folder names, Scoop app names,
/// macOS bundle names, Linux `.desktop` ids and binary names.
struct Family {
    label: &'static str,
    shim: &'static str,
    exe: &'static str,
    install_dirs: &'static [&'static str],
    scoop: &'static [&'static str],
    mac_apps: &'static [&'static str],
    desktop_ids: &'static [&'static str],
    linux_bin: &'static str,
}

const FAMILIES: &[Family] = &[
    Family {
        label: "VS Code",
        shim: "code.cmd",
        exe: "Code.exe",
        install_dirs: &["Microsoft VS Code"],
        scoop: &["vscode"],
        mac_apps: &["Visual Studio Code.app"],
        desktop_ids: &["code.desktop", "code_code.desktop", "visual-studio-code.desktop", "com.visualstudio.code.desktop"],
        linux_bin: "code",
    },
    Family {
        label: "VS Code Insiders",
        shim: "code-insiders.cmd",
        exe: "Code - Insiders.exe",
        install_dirs: &["Microsoft VS Code Insiders"],
        scoop: &["vscode-insiders"],
        mac_apps: &["Visual Studio Code - Insiders.app"],
        desktop_ids: &["code-insiders.desktop", "code-insiders_code-insiders.desktop"],
        linux_bin: "code-insiders",
    },
    Family {
        label: "Cursor",
        shim: "cursor.cmd",
        exe: "Cursor.exe",
        install_dirs: &["cursor", "Cursor"],
        scoop: &["cursor"],
        mac_apps: &["Cursor.app"],
        desktop_ids: &["cursor.desktop"],
        linux_bin: "cursor",
    },
    Family {
        label: "Windsurf",
        shim: "windsurf.cmd",
        exe: "Windsurf.exe",
        install_dirs: &["Windsurf", "windsurf"],
        scoop: &["windsurf"],
        mac_apps: &["Windsurf.app"],
        desktop_ids: &["windsurf.desktop"],
        linux_bin: "windsurf",
    },
    Family {
        label: "VSCodium",
        shim: "codium.cmd",
        exe: "VSCodium.exe",
        install_dirs: &["VSCodium"],
        scoop: &["vscodium"],
        mac_apps: &["VSCodium.app"],
        desktop_ids: &["codium.desktop", "vscodium.desktop", "com.vscodium.codium.desktop"],
        linux_bin: "codium",
    },
];

/// JetBrains: Windows launcher, label, Linux launcher, macOS bundle names.
const JETBRAINS: &[(&str, &str, &str, &[&str])] = &[
    ("rider64.exe", "Rider", "rider", &["Rider.app"]),
    ("idea64.exe", "IntelliJ IDEA", "idea", &["IntelliJ IDEA.app", "IntelliJ IDEA Ultimate.app", "IntelliJ IDEA CE.app", "IntelliJ IDEA Community Edition.app"]),
    ("pycharm64.exe", "PyCharm", "pycharm", &["PyCharm.app", "PyCharm Professional Edition.app", "PyCharm CE.app", "PyCharm Community Edition.app"]),
    ("webstorm64.exe", "WebStorm", "webstorm", &["WebStorm.app"]),
    ("goland64.exe", "GoLand", "goland", &["GoLand.app"]),
    ("clion64.exe", "CLion", "clion", &["CLion.app"]),
    ("phpstorm64.exe", "PhpStorm", "phpstorm", &["PhpStorm.app"]),
    ("rubymine64.exe", "RubyMine", "rubymine", &["RubyMine.app"]),
    ("rustrover64.exe", "RustRover", "rustrover", &["RustRover.app"]),
    ("datagrip64.exe", "DataGrip", "datagrip", &["DataGrip.app"]),
    ("dataspell64.exe", "DataSpell", "dataspell", &["DataSpell.app"]),
];

/// Finds the installed editors. Never fails: a probe that finds nothing adds nothing.
#[must_use]
pub fn detect(env: &dyn DetectEnv) -> Vec<OpenWithEntry> {
    let platform = env.platform();
    let mut found: Vec<OpenWithEntry> = Vec::new();
    let candidates = match platform {
        Platform::Windows => windows(env),
        Platform::Mac => mac(env),
        Platform::Linux => linux(env),
    };
    for e in candidates {
        add(&mut found, e, platform);
    }
    ensure_single_default(&mut found);
    found
}

/// Adds `e` unless its program is unacceptable or already listed (the program decides
/// identity: macOS entries all run `/usr/bin/open`, so there the bundle argument does).
fn add(found: &mut Vec<OpenWithEntry>, e: OpenWithEntry, platform: Platform) {
    if validate_executable_for(&e.executable, false, platform.rules()).is_some() {
        return;
    }
    let key = |x: &OpenWithEntry| format!("{}\u{1}{}", x.executable.to_lowercase(), x.local_args.clone().unwrap_or_default().to_lowercase());
    let k = key(&e);
    if !found.iter().any(|f| key(f) == k) {
        found.push(e);
    }
}

fn entry(label: &str, exe: &str, local: Option<&str>, wsl: Option<&str>, ssh: Option<&str>) -> OpenWithEntry {
    OpenWithEntry {
        label: label.to_string(),
        executable: exe.to_string(),
        local_args: local.map(str::to_string),
        wsl_args: wsl.map(str::to_string),
        ssh_args: ssh.map(str::to_string),
        ..OpenWithEntry::default()
    }
}

fn first_file(env: &dyn DetectEnv, candidates: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    candidates.into_iter().flatten().find(|c| !c.is_empty() && env.is_file(c))
}

// ── Windows ──────────────────────────────────────────────────────────────────────────────

/// PATH entries that are absolute, local folders (relative entries and shares are skipped: not
/// a place to pick up a program to launch on a click).
fn path_dirs(env: &dyn DetectEnv) -> Vec<String> {
    let platform = env.platform();
    let path = env.var("PATH").unwrap_or_default();
    let sep = if platform == Platform::Windows { ';' } else { ':' };
    path.split(sep)
        .map(|d| d.trim().trim_matches('"').to_string())
        .filter(|d| match platform {
            Platform::Windows => !d.starts_with(r"\\") && winpath::is_fully_qualified(d),
            _ => d.starts_with('/'),
        })
        .collect()
}

fn windows(env: &dyn DetectEnv) -> Vec<OpenWithEntry> {
    let p = Platform::Windows;
    let local = env.var("LOCALAPPDATA").unwrap_or_default();
    let pf = env.var("ProgramFiles").unwrap_or_default();
    let pf86 = env.var("ProgramFiles(x86)").unwrap_or_default();
    let profile = env.var("USERPROFILE").unwrap_or_default();
    let mut out = Vec::new();

    for fam in FAMILIES {
        if let Some(exe) = windows_family(env, fam, &local, &pf, &pf86, &profile) {
            out.push(entry(fam.label, &exe, Some(VSCODE_LOCAL_ARGS), Some(VSCODE_WSL_ARGS), Some(VSCODE_SSH_ARGS)));
        }
    }

    if let Some(json) = env.vswhere_json() {
        out.extend(parse_vswhere(&json));
    }

    // JetBrains: newest launcher per product across every layout.
    let mut bins: Vec<String> = Vec::new();
    for root in [join(p, &local, "Programs"), join(p, &pf, "JetBrains")] {
        if !root.is_empty() && env.is_dir(&root) {
            bins.extend(env.list(&root).into_iter().filter(|d| env.is_dir(d)).map(|d| join(p, &d, "bin")));
        }
    }
    let toolbox = join(p, &join(p, &join(p, &local, "JetBrains"), "Toolbox"), "apps");
    if env.is_dir(&toolbox) {
        for product in env.list(&toolbox).into_iter().filter(|d| env.is_dir(d)) {
            for channel in env.list(&product).into_iter().filter(|d| env.is_dir(d)) {
                for build in env.list(&channel).into_iter().filter(|d| env.is_dir(d)) {
                    bins.push(join(p, &build, "bin"));
                }
            }
        }
    }
    for (exe_name, label, _, _) in JETBRAINS {
        let best = bins
            .iter()
            .map(|b| join(p, b, exe_name))
            .filter(|e| env.is_file(e))
            .max_by_key(|e| env.modified(e).unwrap_or(0));
        if let Some(exe) = best {
            // JetBrains IDEs open a \\wsl$ folder natively, so the WSL form is the UNC mirror.
            out.push(entry(label, &exe, Some("{folder}"), Some("{folder}"), None));
        }
    }

    if let Some(zed) = first_file(env, [env.app_path("Zed.exe"), Some(join(p, &join(p, &join(p, &local, "Programs"), "Zed"), "Zed.exe"))]) {
        out.push(entry("Zed", &zed, Some("{folder}"), Some("{folder}"), None));
    }
    if let Some(subl) = first_file(
        env,
        [
            Some(join(p, &join(p, &pf, "Sublime Text"), "subl.exe")),
            Some(join(p, &join(p, &pf, "Sublime Text 3"), "subl.exe")),
            env.app_path("sublime_text.exe"),
        ],
    ) {
        out.push(entry("Sublime Text", &subl, Some("{folder}"), Some("{folder}"), None));
    }
    if let Some(npp) = first_file(
        env,
        [env.app_path("notepad++.exe"), Some(join(p, &join(p, &pf, "Notepad++"), "notepad++.exe")), Some(join(p, &join(p, &pf86, "Notepad++"), "notepad++.exe"))],
    ) {
        out.push(entry("Notepad++", &npp, Some("-openFoldersAsWorkspace {folder}"), Some("-openFoldersAsWorkspace {folder}"), None));
    }
    out
}

/// PATH shim first (the install the user's shell runs, resolved to its exe — the `.cmd` itself is
/// never used), then App Paths, then the standard folders and Scoop.
fn windows_family(env: &dyn DetectEnv, fam: &Family, local: &str, pf: &str, pf86: &str, profile: &str) -> Option<String> {
    let p = Platform::Windows;
    for dir in path_dirs(env) {
        let shim = join(p, &dir, fam.shim);
        if !env.is_file(&shim) {
            continue;
        }
        let known = super::known_shim_candidates(&shim).into_iter().find(|c| env.is_file(c));
        let resolved = known.or_else(|| env.read_text(&shim).and_then(|t| resolve_shim_from_script(&t, &|x| env.is_file(x))));
        if let Some(exe) = resolved {
            return Some(exe);
        }
    }
    if let Some(a) = env.app_path(fam.exe).filter(|a| env.is_file(a)) {
        return Some(a);
    }
    for dir in fam.install_dirs {
        for root in [join(p, local, "Programs"), pf.to_string(), pf86.to_string()] {
            if root.is_empty() {
                continue;
            }
            let exe = join(p, &join(p, &root, dir), fam.exe);
            if env.is_file(&exe) {
                return Some(exe);
            }
        }
    }
    if !profile.is_empty() {
        for app in fam.scoop {
            let exe = join(p, &join(p, &join(p, &join(p, &join(p, profile, "scoop"), "apps"), app), "current"), fam.exe);
            if env.is_file(&exe) {
                return Some(exe);
            }
        }
    }
    None
}

/// vswhere `-format json` → one entry per instance with a `devenv.exe`. Visual Studio opens a
/// folder with `devenv.exe <folder>` (Open Folder mode); local only.
#[must_use]
pub fn parse_vswhere(json: &str) -> Vec<OpenWithEntry> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|inst| {
            let path = inst.get("productPath")?.as_str()?;
            if !winpath::file_name(path).eq_ignore_ascii_case("devenv.exe") {
                return None;
            }
            let label = inst.get("displayName").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()).unwrap_or("Visual Studio");
            let label: String = label.chars().filter(|c| !c.is_control()).take(80).collect();
            Some(entry(&label, path, Some("{folder}"), None, None))
        })
        .collect()
}

// ── macOS ────────────────────────────────────────────────────────────────────────────────

/// The program every macOS entry runs: `open -a <bundle>` hands the folder to the app.
pub const MAC_OPEN: &str = "/usr/bin/open";

fn mac(env: &dyn DetectEnv) -> Vec<OpenWithEntry> {
    let p = Platform::Mac;
    let mut roots = vec!["/Applications".to_string()];
    if let Some(home) = env.var("HOME").filter(|h| h.starts_with('/')) {
        roots.push(join(p, &home, "Applications"));
        // JetBrains Toolbox 2.x puts its IDEs in ~/Applications too; 1.x in a nested folder.
        roots.push(join(p, &join(p, &home, "Applications"), "JetBrains Toolbox"));
    }
    let bundles: Vec<String> = roots.iter().filter(|r| env.is_dir(r)).flat_map(|r| env.list(r)).filter(|b| b.ends_with(".app") && env.is_dir(b)).collect();
    let find = |names: &[&str]| bundles.iter().find(|b| names.iter().any(|n| leaf(p, b).eq_ignore_ascii_case(n))).cloned();
    // The bundle path becomes one quoted template token: it must not need escaping.
    let token_safe = |b: &str| !b.contains('"') && !b.contains('{') && !b.contains('}') && !b.chars().any(char::is_control);
    let mut out = Vec::new();
    let open_args = |bundle: &str, rest: &str| format!("-a \"{bundle}\" {rest}");
    for fam in FAMILIES {
        if let Some(b) = find(fam.mac_apps).filter(|b| token_safe(b)) {
            let ssh = format!("-a \"{b}\" --args {VSCODE_SSH_ARGS}");
            out.push(entry(fam.label, MAC_OPEN, Some(&open_args(&b, "{folder}")), None, Some(&ssh)));
        }
    }
    for (_, label, _, apps) in JETBRAINS {
        if let Some(b) = find(apps).filter(|b| token_safe(b)) {
            out.push(entry(label, MAC_OPEN, Some(&open_args(&b, "{folder}")), None, None));
        }
    }
    for (label, apps) in [("Zed", &["Zed.app"][..]), ("Sublime Text", &["Sublime Text.app"][..])] {
        if let Some(b) = find(apps).filter(|b| token_safe(b)) {
            out.push(entry(label, MAC_OPEN, Some(&open_args(&b, "{folder}")), None, None));
        }
    }
    out
}

// ── Linux ────────────────────────────────────────────────────────────────────────────────

/// The program a `.desktop` file's `Exec=` runs: its first token, when that is a plain program
/// (absolute, or a bare name found on PATH). Field codes (`%F`) are ignored; a launcher such as
/// `env`, `flatpak run` or `sh -c` gives `None` (its arguments would be the real program).
#[must_use]
pub fn desktop_exec_program(text: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_entry = l == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(v) = l.strip_prefix("Exec=") {
            let v = v.trim();
            let first = if let Some(rest) = v.strip_prefix('"') { rest.split('"').next()? } else { v.split_whitespace().next()? };
            if first.is_empty() || first.contains('%') || first.contains('=') {
                return None;
            }
            return Some(first.to_string());
        }
    }
    None
}

fn linux_app_dirs(env: &dyn DetectEnv) -> Vec<String> {
    let p = Platform::Linux;
    let mut dirs = Vec::new();
    let home = env.var("HOME").filter(|h| h.starts_with('/'));
    match env.var("XDG_DATA_HOME").filter(|d| d.starts_with('/')) {
        Some(d) => dirs.push(join(p, &d, "applications")),
        None => {
            if let Some(h) = &home {
                dirs.push(join(p, h, ".local/share/applications"));
            }
        }
    }
    let data_dirs = env.var("XDG_DATA_DIRS").filter(|d| !d.trim().is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for d in data_dirs.split(':').filter(|d| d.starts_with('/')) {
        dirs.push(join(p, d, "applications"));
    }
    dirs.push("/var/lib/snapd/desktop/applications".to_string());
    dirs.push("/var/lib/flatpak/exports/share/applications".to_string());
    if let Some(h) = &home {
        dirs.push(join(p, h, ".local/share/flatpak/exports/share/applications"));
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let new = !seen.contains(d);
        seen.push(d.clone());
        new
    });
    dirs
}

fn on_path(env: &dyn DetectEnv, name: &str) -> Option<String> {
    path_dirs(env).into_iter().map(|d| join(Platform::Linux, &d, name)).find(|c| env.is_file(c))
}

/// A `.desktop` file's program, resolved to a real binary.
fn desktop_program(env: &dyn DetectEnv, file: &str) -> Option<String> {
    let prog = desktop_exec_program(&env.read_text(file)?)?;
    let abs = if prog.starts_with('/') { Some(prog) } else if prog.contains('/') { None } else { on_path(env, &prog) }?;
    Some(env.real_path(&abs).unwrap_or(abs))
}

/// The VS Code family's `…/bin/code` is a shell wrapper around the Electron binary one folder up
/// (`/usr/share/code/bin/code` → `/usr/share/code/code`): use the binary.
fn electron_beside_wrapper(env: &dyn DetectEnv, path: &str, bin: &str) -> String {
    let p = Platform::Linux;
    if let Some(dir) = parent(p, path)
        && leaf(p, &dir) == "bin"
        && let Some(up) = parent(p, &dir)
    {
        let cand = join(p, &up, bin);
        if env.is_file(&cand) {
            return cand;
        }
    }
    path.to_string()
}

fn linux(env: &dyn DetectEnv) -> Vec<OpenWithEntry> {
    let p = Platform::Linux;
    let dirs = linux_app_dirs(env);
    let desktop = |ids: &[&str]| -> Option<String> {
        dirs.iter().flat_map(|d| ids.iter().map(move |id| join(p, d, id))).filter(|f| env.is_file(f)).find_map(|f| desktop_program(env, &f))
    };
    let mut out = Vec::new();
    for fam in FAMILIES {
        let found = desktop(fam.desktop_ids).or_else(|| on_path(env, fam.linux_bin).map(|x| env.real_path(&x).unwrap_or(x)));
        if let Some(exe) = found {
            let exe = electron_beside_wrapper(env, &exe, fam.linux_bin);
            out.push(entry(fam.label, &exe, Some(VSCODE_LOCAL_ARGS), None, Some(VSCODE_SSH_ARGS)));
        }
    }
    // JetBrains: Toolbox's generated .desktop files (jetbrains-<product>.desktop), then the
    // Toolbox apps folder's native launchers (`bin/<name>`, not `bin/<name>.sh`).
    let toolbox = env.var("HOME").filter(|h| h.starts_with('/')).map(|h| join(p, &h, ".local/share/JetBrains/Toolbox/apps"));
    for (_, label, bin, _) in JETBRAINS {
        let ids = [format!("jetbrains-{bin}.desktop"), format!("jetbrains-{bin}-ce.desktop")];
        let via_desktop = dirs.iter().flat_map(|d| ids.iter().map(move |id| join(p, d, id))).filter(|f| env.is_file(f)).find_map(|f| desktop_program(env, &f));
        let via_toolbox = || {
            let root = toolbox.as_ref().filter(|t| env.is_dir(t))?;
            env.list(root)
                .into_iter()
                .filter(|d| env.is_dir(d))
                .map(|d| join(p, &join(p, &d, "bin"), bin))
                .filter(|e| env.is_file(e))
                .max_by_key(|e| env.modified(e).unwrap_or(0))
        };
        if let Some(exe) = via_desktop.or_else(via_toolbox) {
            out.push(entry(label, &exe, Some("{folder}"), None, None));
        }
    }
    if let Some(zed) = desktop(&["dev.zed.Zed.desktop", "zed.desktop"]).or_else(|| on_path(env, "zeditor")).or_else(|| on_path(env, "zed")) {
        out.push(entry("Zed", &zed, Some("{folder}"), None, None));
    }
    if let Some(subl) = desktop(&["sublime_text.desktop"]).or_else(|| on_path(env, "subl")) {
        out.push(entry("Sublime Text", &subl, Some("{folder}"), None, None));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fake machine: files (with text and mtime), folders, env vars, App Paths, vswhere.
    struct Fake {
        platform: Platform,
        vars: HashMap<String, String>,
        files: HashMap<String, (String, String, u64)>,
        dirs: HashMap<String, String>,
        app_paths: HashMap<String, String>,
        vswhere: Option<String>,
        links: HashMap<String, String>,
    }

    impl Fake {
        fn new(platform: Platform) -> Self {
            Fake { platform, vars: HashMap::new(), files: HashMap::new(), dirs: HashMap::new(), app_paths: HashMap::new(), vswhere: None, links: HashMap::new() }
        }
        fn var(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.into(), v.into());
            self
        }
        fn file(mut self, path: &str, text: &str, mtime: u64) -> Self {
            self.add_parents(path);
            self.files.insert(path.to_lowercase(), (path.into(), text.into(), mtime));
            self
        }
        fn dir(mut self, path: &str) -> Self {
            self.add_parents(path);
            self.dirs.insert(path.to_lowercase(), path.into());
            self
        }
        fn add_parents(&mut self, path: &str) {
            let mut cur = path.to_string();
            while let Some(p) = parent(self.platform, &cur) {
                self.dirs.insert(p.to_lowercase(), p.clone());
                if p == cur {
                    break;
                }
                cur = p;
            }
        }
        fn key(&self, p: &str) -> String {
            p.to_lowercase()
        }
    }

    impl DetectEnv for Fake {
        fn platform(&self) -> Platform {
            self.platform
        }
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }
        fn is_file(&self, path: &str) -> bool {
            self.files.contains_key(&self.key(path))
        }
        fn is_dir(&self, path: &str) -> bool {
            self.dirs.contains_key(&self.key(path))
        }
        fn list(&self, dir: &str) -> Vec<String> {
            let d = self.key(dir);
            let sep = self.platform.sep();
            let all = self.files.iter().map(|(k, v)| (k, &v.0)).chain(self.dirs.iter());
            let mut out: Vec<String> = all
                .filter(|(p, _)| p.len() > d.len() + 1 && p.starts_with(&d) && p[d.len()..].starts_with(sep) && !p[d.len() + 1..].contains(sep))
                .map(|(_, orig)| orig.clone())
                .collect();
            out.sort();
            out.dedup();
            out
        }
        fn read_text(&self, path: &str) -> Option<String> {
            self.files.get(&self.key(path)).map(|(_, t, _)| t.clone())
        }
        fn modified(&self, path: &str) -> Option<u64> {
            self.files.get(&self.key(path)).map(|(_, _, m)| *m)
        }
        fn app_path(&self, exe_name: &str) -> Option<String> {
            self.app_paths.get(&exe_name.to_lowercase()).cloned()
        }
        fn vswhere_json(&self) -> Option<String> {
            self.vswhere.clone()
        }
        fn real_path(&self, path: &str) -> Option<String> {
            self.links.get(path).cloned()
        }
    }

    fn win() -> Fake {
        Fake::new(Platform::Windows)
            .var("LOCALAPPDATA", r"C:\Users\u\AppData\Local")
            .var("ProgramFiles", r"C:\Program Files")
            .var("ProgramFiles(x86)", r"C:\Program Files (x86)")
            .var("USERPROFILE", r"C:\Users\u")
    }

    fn labels(v: &[OpenWithEntry]) -> Vec<String> {
        v.iter().map(|e| e.label.clone()).collect()
    }

    #[test]
    fn nothing_installed_finds_nothing() {
        assert!(detect(&win()).is_empty());
        assert!(detect(&Fake::new(Platform::Mac)).is_empty());
        assert!(detect(&Fake::new(Platform::Linux)).is_empty());
    }

    #[test]
    fn vscode_from_the_path_shim_resolves_to_the_exe_never_the_cmd() {
        let f = win()
            .var("PATH", r"relative\bin;\\share\bin;C:\Users\u\AppData\Local\Programs\Microsoft VS Code\bin")
            .file(r"C:\Users\u\AppData\Local\Programs\Microsoft VS Code\bin\code.cmd", "@echo off", 1)
            .file(r"C:\Users\u\AppData\Local\Programs\Microsoft VS Code\Code.exe", "", 1);
        let found = detect(&f);
        assert_eq!(labels(&found), ["VS Code"]);
        let e = &found[0];
        assert!(e.executable.ends_with(r"Microsoft VS Code\Code.exe"), "{}", e.executable);
        assert!(e.is_default);
        assert_eq!(e.wsl_args.as_deref(), Some(VSCODE_WSL_ARGS));
        assert_eq!(e.ssh_args.as_deref(), Some(VSCODE_SSH_ARGS));
    }

    #[test]
    fn an_unknown_shim_resolves_only_when_its_script_names_one_exe() {
        let one = win()
            .var("PATH", r"C:\tools\bin")
            .file(r"C:\tools\bin\cursor.cmd", "@\"C:\\tools\\Cursor\\Cursor.exe\" %*", 1)
            .file(r"C:\tools\Cursor\Cursor.exe", "", 1);
        assert_eq!(labels(&detect(&one)), ["Cursor"]);
        let two = win()
            .var("PATH", r"C:\tools\bin")
            .file(r"C:\tools\bin\cursor.cmd", "\"C:\\a\\Cursor.exe\" \"C:\\b\\Other.exe\"", 1)
            .file(r"C:\a\Cursor.exe", "", 1)
            .file(r"C:\b\Other.exe", "", 1);
        assert!(detect(&two).is_empty(), "ambiguous script → no guess");
    }

    #[test]
    fn app_paths_program_files_and_scoop() {
        let mut f = win()
            .file(r"C:\Program Files\Windsurf\Windsurf.exe", "", 1)
            .file(r"C:\Users\u\scoop\apps\vscodium\current\VSCodium.exe", "", 1)
            .file(r"D:\Apps\Code - Insiders.exe", "", 1);
        f.app_paths.insert("code - insiders.exe".into(), r"D:\Apps\Code - Insiders.exe".into());
        let got = labels(&detect(&f));
        assert_eq!(got, ["VS Code Insiders", "Windsurf", "VSCodium"]);
    }

    #[test]
    fn visual_studio_from_vswhere_local_only() {
        let mut f = win();
        f.vswhere = Some(
            r#"[{"displayName":"Visual Studio Community 2026","productPath":"C:\\Program Files\\Microsoft Visual Studio\\18\\Community\\Common7\\IDE\\devenv.exe"},
                {"displayName":"Build Tools","productPath":"C:\\x\\vs_installer.exe"},
                {"productPath":"C:\\VS\\devenv.exe"}]"#
                .into(),
        );
        let got = detect(&f);
        assert_eq!(labels(&got), ["Visual Studio Community 2026", "Visual Studio"]);
        assert_eq!(got[0].local_args.as_deref(), Some("{folder}"));
        assert_eq!(got[0].wsl_args, None);
        assert!(parse_vswhere("not json").is_empty());
        assert!(parse_vswhere("{}").is_empty());
    }

    #[test]
    fn jetbrains_newest_build_wins_across_layouts_and_never_the_cmd_scripts() {
        let f = win()
            .file(r"C:\Users\u\AppData\Local\JetBrains\Toolbox\apps\Rider\ch-0\241.1\bin\rider64.exe", "", 10)
            .file(r"C:\Users\u\AppData\Local\JetBrains\Toolbox\apps\Rider\ch-0\243.2\bin\rider64.exe", "", 30)
            .file(r"C:\Users\u\AppData\Local\Programs\Rider\bin\rider64.exe", "", 20)
            .file(r"C:\Program Files\JetBrains\IntelliJ IDEA 2025.3\bin\idea64.exe", "", 5)
            .file(r"C:\Users\u\AppData\Local\JetBrains\Toolbox\scripts\pycharm.cmd", "", 5);
        let got = detect(&f);
        assert_eq!(labels(&got), ["Rider", "IntelliJ IDEA"]);
        assert!(got[0].executable.contains(r"243.2\bin\rider64.exe"), "{}", got[0].executable);
        assert_eq!(got[0].wsl_args.as_deref(), Some("{folder}"));
    }

    #[test]
    fn plain_editors_and_notepad_plus_plus_workspace_flag() {
        let f = win()
            .file(r"C:\Users\u\AppData\Local\Programs\Zed\Zed.exe", "", 1)
            .file(r"C:\Program Files\Sublime Text\subl.exe", "", 1)
            .file(r"C:\Program Files (x86)\Notepad++\notepad++.exe", "", 1);
        let got = detect(&f);
        assert_eq!(labels(&got), ["Zed", "Sublime Text", "Notepad++"]);
        assert_eq!(got[2].local_args.as_deref(), Some("-openFoldersAsWorkspace {folder}"));
        assert!(got[0].is_default && !got[1].is_default);
    }

    #[test]
    fn a_program_under_system_root_or_a_shell_is_never_added() {
        let mut f = win().file(r"C:\Windows\System32\cmd.exe", "", 1).file(r"C:\Tools\pwsh.exe", "", 1);
        f.app_paths.insert("zed.exe".into(), r"C:\Windows\System32\cmd.exe".into());
        f.app_paths.insert("sublime_text.exe".into(), r"C:\Tools\pwsh.exe".into());
        assert!(detect(&f).is_empty());
    }

    #[test]
    fn mac_bundles_open_with_open_dash_a() {
        let f = Fake::new(Platform::Mac)
            .var("HOME", "/Users/u")
            .dir("/Applications/Visual Studio Code.app")
            .dir("/Applications/Zed.app")
            .dir("/Users/u/Applications/Rider.app")
            .dir("/Applications/Notes.app")
            .dir("/Applications/We\"ird.app");
        let got = detect(&f);
        assert_eq!(labels(&got), ["VS Code", "Rider", "Zed"]);
        let code = &got[0];
        assert_eq!(code.executable, MAC_OPEN);
        assert_eq!(code.local_args.as_deref(), Some("-a \"/Applications/Visual Studio Code.app\" {folder}"));
        assert!(code.ssh_args.as_deref().is_some_and(|a| a.contains("--args --folder-uri")));
        // The template tokenises to the bundle as ONE argument.
        let toks = super::super::tokenize(code.local_args.as_deref().unwrap_or("")).unwrap();
        assert_eq!(toks, ["-a", "/Applications/Visual Studio Code.app", "{folder}"]);
    }

    #[test]
    fn linux_desktop_files_wrappers_and_toolbox() {
        let f = Fake::new(Platform::Linux)
            .var("HOME", "/home/u")
            .var("PATH", "/usr/local/bin:/usr/bin:relative")
            .file("/usr/share/applications/code.desktop", "[Desktop Entry]\nName=Visual Studio Code\nExec=/usr/share/code/code --unity-launch %F\n[Desktop Action new]\nExec=/bin/sh -c x\n", 1)
            .file("/usr/share/code/code", "", 1)
            .file("/usr/share/applications/cursor.desktop", "[Desktop Entry]\nExec=cursor %F\n", 1)
            .file("/usr/bin/cursor", "", 1)
            .file("/opt/cursor/bin/cursor", "#!/bin/sh", 1)
            .file("/opt/cursor/cursor", "", 1)
            .file("/home/u/.local/share/JetBrains/Toolbox/apps/rider/bin/rider", "", 3)
            .file("/home/u/.local/share/JetBrains/Toolbox/apps/rider/bin/rider.sh", "", 3)
            .file("/usr/share/applications/flatpak-thing.desktop", "[Desktop Entry]\nExec=flatpak run com.x\n", 1)
            .file("/usr/bin/subl", "", 1);
        let mut f = f;
        f.links.insert("/usr/bin/cursor".into(), "/opt/cursor/bin/cursor".into());
        let got = detect(&f);
        assert_eq!(labels(&got), ["VS Code", "Cursor", "Rider", "Sublime Text"]);
        assert_eq!(got[0].executable, "/usr/share/code/code");
        assert_eq!(got[1].executable, "/opt/cursor/cursor", "the bin/ wrapper is swapped for the Electron binary");
        assert_eq!(got[2].executable, "/home/u/.local/share/JetBrains/Toolbox/apps/rider/bin/rider");
        assert_eq!(got[1].wsl_args, None);
    }

    #[test]
    fn posix_rules_keep_the_shape_without_the_windows_fences() {
        use super::super::{OpenWithContext, build_for, validate_executable_posix};
        use crate::launch::SessionKind;
        assert!(validate_executable_posix("/usr/bin/open").is_none());
        for bad in ["", "open", "relative/x", "/bin/bash", "/usr/bin/env", "/opt/x/run.sh", "/opt/../bin/x", "/usr/bin/python3", "/x\n"] {
            assert!(validate_executable_posix(bad).is_some(), "{bad:?}");
        }
        let e = entry("Zed", "/usr/bin/zed", Some("{folder}"), None, None);
        let ctx = OpenWithContext { folder: "/home/u/a b; $(id)".into(), ..OpenWithContext::new(SessionKind::Local) };
        let cmd = build_for(&e, &ctx, Rules::Posix).unwrap();
        assert_eq!(cmd.arguments, ["/home/u/a b; $(id)"], "a POSIX folder may start an argument with /");
        // Option-shaped values are still refused, and relative folders are not full paths.
        let dash = OpenWithContext { folder: "-rf".into(), ..OpenWithContext::new(SessionKind::Local) };
        assert!(build_for(&e, &dash, Rules::Posix).is_err());
        let rel = OpenWithContext { folder: "src/x".into(), ..OpenWithContext::new(SessionKind::Local) };
        assert!(build_for(&e, &rel, Rules::Posix).is_err());
        // The Windows rules still refuse a /-led argument and a non-.exe program.
        assert!(build_for(&e, &ctx, Rules::Windows).is_err());
    }

    #[test]
    fn desktop_exec_parsing() {
        assert_eq!(desktop_exec_program("[Desktop Entry]\nExec=\"/opt/My App/app\" %U").as_deref(), Some("/opt/My App/app"));
        assert_eq!(desktop_exec_program("[Desktop Entry]\nExec=env FOO=1 /x"), Some("env".into()));
        assert_eq!(desktop_exec_program("[Desktop Entry]\nExec=FOO=1 /x"), None);
        assert_eq!(desktop_exec_program("[Other]\nExec=/x"), None);
        assert_eq!(desktop_exec_program(""), None);
    }
}

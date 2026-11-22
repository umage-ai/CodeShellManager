//! Run-command templates by project type (`RunCommandTemplatesService`): a top-level scan of the
//! session's folder, first match wins: dotnet → cargo → node → python → make. Templates are
//! COPIED onto a new session (fresh ids); later edits never flow back.
//!
//! A Node project gets one command per `scripts` entry of its top-level `package.json` (through
//! the package manager its lock file names), falling back to a fixed Start/Test/Build list when
//! the file is missing, unreadable, too big or has no usable script.
//!
//! [`seed_for_names`] and [`seed_for_project`] are pure; [`seed_for_folder`] lists the folder and
//! reads `package.json` (disk I/O, and `\\wsl$` can boot a distro): worker threads only.

use std::io::Read;

use csm_core::models::{RunCommandItem, RunMode};
use serde::Deserialize;

use super::MAX_RUN_COMMANDS;

/// Largest `package.json` read; a bigger one is ignored (the fixed Node list is used).
pub const MAX_PACKAGE_JSON: u64 = 1024 * 1024;
/// Longest script name taken from `package.json`.
const MAX_SCRIPT_NAME: usize = 64;

/// A project type's seed list.
#[derive(Debug, Clone, PartialEq)]
pub struct Seed {
    /// `dotnet`, `cargo`, `node`, `python` or `make`.
    pub kind: &'static str,
    pub items: Vec<RunCommandItem>,
    /// Usable `package.json` scripts left out because of [`MAX_RUN_COMMANDS`].
    pub left_out: usize,
}

/// The project type and its seed list, or `None` (unknown type, empty or missing folder). A Node
/// project gets the fixed list; [`seed_for_project`] reads its scripts.
pub fn seed_for_names(names: &[String]) -> Option<Seed> {
    seed_for_project(names, None)
}

/// [`seed_for_names`], with the text of the folder's `package.json` (if it was read): a Node
/// project then gets one command per script.
pub fn seed_for_project(names: &[String], package_json: Option<&str>) -> Option<Seed> {
    let has = |n: &str| names.iter().any(|f| f.eq_ignore_ascii_case(n));
    let has_ext = |e: &str| names.iter().any(|f| f.len() > e.len() && f.get(f.len() - e.len()..).is_some_and(|x| x.eq_ignore_ascii_case(e)));
    let fixed = |kind, items: &[(&str, &str)]| Some(Seed { kind, items: build(items), left_out: 0 });
    if has_ext(".sln") || has_ext(".slnx") || has_ext(".csproj") {
        return fixed("dotnet", &[("Run", "dotnet run"), ("Build", "dotnet build"), ("Test", "dotnet test")]);
    }
    if has("Cargo.toml") {
        return fixed("cargo", &[("Run", "cargo run"), ("Build", "cargo build"), ("Test", "cargo test"), ("Clippy", "cargo clippy")]);
    }
    if has("package.json") {
        let pm = if has("pnpm-lock.yaml") {
            "pnpm"
        } else if has("yarn.lock") {
            "yarn"
        } else if has("bun.lockb") || has("bun.lock") {
            "bun"
        } else {
            "npm"
        };
        if let Some((items, left_out)) = package_json.and_then(|text| node_scripts(pm, text)) {
            return Some(Seed { kind: "node", items, left_out });
        }
        // yarn's convention is `yarn build` (no `run`).
        let build_cmd = if pm == "yarn" { "yarn build".to_string() } else { format!("{pm} run build") };
        let start = format!("{pm} start");
        let test = format!("{pm} test");
        return fixed("node", &[("Start", &start), ("Test", &test), ("Build", &build_cmd)]);
    }
    if has("pyproject.toml") || has("requirements.txt") {
        return fixed("python", &[("Run", "python main.py"), ("Test", "python -m pytest")]);
    }
    if has("Makefile") || has("makefile") || has("GNUmakefile") {
        return fixed("make", &[("Run", "make"), ("Test", "make test"), ("Clean", "make clean")]);
    }
    None
}

/// `package.json`'s `scripts`, in file order (serde_json's map would sort them).
#[derive(Deserialize)]
struct PackageJson {
    #[serde(default, deserialize_with = "ordered_scripts")]
    scripts: Vec<(String, serde_json::Value)>,
}

fn ordered_scripts<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<(String, serde_json::Value)>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<(String, serde_json::Value)>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object of scripts")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some((k, v)) = map.next_entry::<String, serde_json::Value>()? {
                out.push((k, v));
            }
            Ok(out)
        }
        // `"scripts": null` is "no scripts", not a broken file.
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
    }
    d.deserialize_any(V)
}

/// npm's lifecycle hooks: run by npm itself around install/publish/version, not by a person.
const LIFECYCLE: [&str; 17] = [
    "preinstall",
    "install",
    "postinstall",
    "prepare",
    "prepublish",
    "prepublishOnly",
    "publish",
    "postpublish",
    "prepack",
    "postpack",
    "preversion",
    "version",
    "postversion",
    "dependencies",
    "preuninstall",
    "uninstall",
    "postuninstall",
];

/// A script name that is safe on a `cmd.exe` (or POSIX shell) command line unquoted: letters,
/// digits and `:_.-/@`, not starting with `-` (it would read as an option), and not too long.
/// Names come from an untrusted file; anything else is skipped, never quoted.
pub fn safe_script_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SCRIPT_NAME
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '-' | '/' | '@'))
}

/// One run command per usable script of `package_json`, in file order, run through `pm`, and how
/// many usable scripts were left out. The default is `dev`, else `start`, else the first; past
/// [`MAX_RUN_COMMANDS`] the default is still kept and the rest are the first ones in file order.
/// `None` when the text is not a JSON object or no script is usable.
pub fn node_scripts(pm: &str, package_json: &str) -> Option<(Vec<RunCommandItem>, usize)> {
    let pkg: PackageJson = serde_json::from_str(package_json).ok()?;
    let all: Vec<&str> = pkg.scripts.iter().filter(|(_, v)| v.is_string()).map(|(k, _)| k.as_str()).collect();
    let hook = |n: &str| {
        LIFECYCLE.contains(&n) || n.strip_prefix("pre").or_else(|| n.strip_prefix("post")).is_some_and(|base| !base.is_empty() && all.contains(&base))
    };
    let mut names: Vec<&str> = Vec::new();
    for n in &all {
        if safe_script_name(n) && !hook(n) && !names.contains(n) {
            names.push(n);
        }
    }
    if names.is_empty() {
        return None;
    }
    let default = names.iter().position(|n| *n == "dev").or_else(|| names.iter().position(|n| *n == "start")).unwrap_or(0);
    let left_out = names.len().saturating_sub(MAX_RUN_COMMANDS);
    // Keep the default plus the first others, in file order.
    let mut room = MAX_RUN_COMMANDS.saturating_sub(1);
    let items = names
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            if *i == default {
                return true;
            }
            let keep = room > 0;
            room = room.saturating_sub(1);
            keep
        })
        .map(|(i, n)| RunCommandItem {
            id: crate::lifecycle::new_id(),
            label: (*n).to_owned(),
            command_line: script_command(pm, n),
            is_default: i == default,
            mode: RunMode::Process,
            post_run_url: None,
        })
        .collect();
    Some((items, left_out))
}

/// The command line that runs script `name` with package manager `pm`. `bun test` is bun's own
/// test runner, so bun always says `run`.
fn script_command(pm: &str, name: &str) -> String {
    match (pm, name) {
        ("npm" | "pnpm", "start" | "test") => format!("{pm} {name}"),
        ("yarn", _) => format!("yarn {name}"),
        _ => format!("{pm} run {name}"),
    }
}

fn build(items: &[(&str, &str)]) -> Vec<RunCommandItem> {
    items
        .iter()
        .enumerate()
        .map(|(i, (label, cmd))| RunCommandItem {
            id: crate::lifecycle::new_id(),
            label: (*label).to_owned(),
            command_line: (*cmd).to_owned(),
            is_default: i == 0,
            mode: RunMode::Process,
            post_run_url: None,
        })
        .collect()
}

/// Lists `folder`'s top-level FILES (one enumeration; repeated existence checks are slow on a
/// share) and seeds from them, reading `package.json` (at most [`MAX_PACKAGE_JSON`]) when there
/// is one. Blocking: worker threads only.
pub fn seed_for_folder(folder: &str) -> Option<Seed> {
    let folder = folder.trim();
    if folder.is_empty() {
        return None;
    }
    let names: Vec<String> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .take(10_000)
        .collect();
    let pkg = names.iter().find(|n| n.eq_ignore_ascii_case("package.json")).and_then(|n| read_capped(&std::path::Path::new(folder).join(n)));
    seed_for_project(&names, pkg.as_deref())
}

/// A small UTF-8 text file, or `None` (missing, unreadable, not UTF-8, over [`MAX_PACKAGE_JSON`]).
fn read_capped(path: &std::path::Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(MAX_PACKAGE_JSON + 1).read_to_end(&mut buf).ok()?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > MAX_PACKAGE_JSON {
        return None;
    }
    String::from_utf8(buf).ok()
}

/// A monorepo-style `package.json` for tests: 91 usable scripts with colons, pre/post hooks, a
/// duplicate key, unsafe names, and `dev` far past [`MAX_RUN_COMMANDS`].
#[doc(hidden)]
pub fn monorepo_package_json() -> String {
    let mut s: Vec<String> = vec![
        r#""prebuild":"rimraf dist""#.into(),
        r#""build":"turbo run build""#.into(),
        r#""postbuild":"node scripts/size.js""#.into(),
        r#""prepare":"husky install""#.into(),
        r#""postinstall":"patch-package""#.into(),
        r#""build:web":"turbo run build --filter=web""#.into(),
        r#""test:e2e:ci":"playwright test""#.into(),
        r#""clean && calc":"x""#.into(),
        r#""lint fix":"x""#.into(),
    ];
    for i in 0..85 {
        s.push(format!(r#""pkg{i}:build":"turbo run build --filter=pkg{i}""#));
    }
    s.push(r#""build:web":"duplicate key""#.into());
    s.push(r#""start":"node server.js""#.into());
    s.push(r#""dev":"turbo run dev --parallel""#.into());
    s.push(r#""release":"changeset publish""#.into());
    format!("{{\"name\":\"mono\",\"private\":true,\"workspaces\":[\"packages/*\"],\"scripts\":{{{}}}}}", s.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn cmds(v: &[RunCommandItem]) -> Vec<&str> {
        v.iter().map(|i| i.command_line.as_str()).collect()
    }

    #[test]
    fn priority_is_dotnet_cargo_node_python_make() {
        let s = seed_for_names(&names(&["Cargo.toml", "App.csproj", "package.json"])).unwrap();
        assert_eq!(s.kind, "dotnet");
        assert_eq!(cmds(&s.items), ["dotnet run", "dotnet build", "dotnet test"]);
        assert_eq!(seed_for_names(&names(&["cargo.TOML", "Makefile"])).unwrap().kind, "cargo");
        assert_eq!(seed_for_names(&names(&["requirements.txt", "Makefile"])).unwrap().kind, "python");
        assert_eq!(seed_for_names(&names(&["makefile"])).unwrap().kind, "make");
        assert!(seed_for_names(&names(&["README.md"])).is_none());
        assert!(seed_for_names(&[]).is_none());
    }

    #[test]
    fn node_follows_the_lock_file() {
        let pnpm = seed_for_names(&names(&["package.json", "pnpm-lock.yaml"])).unwrap().items;
        assert_eq!(cmds(&pnpm), ["pnpm start", "pnpm test", "pnpm run build"]);
        let yarn = seed_for_names(&names(&["package.json", "yarn.lock"])).unwrap().items;
        assert_eq!(cmds(&yarn), ["yarn start", "yarn test", "yarn build"]);
        let npm = seed_for_names(&names(&["package.json"])).unwrap().items;
        assert_eq!(cmds(&npm), ["npm start", "npm test", "npm run build"]);
    }

    #[test]
    fn exactly_one_default_and_fresh_ids() {
        let a = seed_for_names(&names(&["x.sln"])).unwrap().items;
        let b = seed_for_names(&names(&["x.sln"])).unwrap().items;
        assert_eq!(a.iter().filter(|i| i.is_default).count(), 1);
        assert!(a[0].is_default);
        assert_ne!(a[0].id, b[0].id);
    }

    #[test]
    fn a_real_folder_is_scanned_top_level_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("Cargo.toml"), "").unwrap();
        assert!(seed_for_folder(&dir.path().display().to_string()).is_none(), "nested files don't count");
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let s = seed_for_folder(&dir.path().display().to_string()).unwrap();
        assert_eq!((s.kind, cmds(&s.items)), ("node", vec!["npm start", "npm test", "npm run build"]), "no scripts: the fixed list");
        assert!(seed_for_folder(&dir.path().join("missing").display().to_string()).is_none());
        assert!(seed_for_folder("  ").is_none());
    }

    fn node(pm: &str, text: &str) -> Vec<(String, String, bool)> {
        node_scripts(pm, text).unwrap().0.into_iter().map(|i| (i.label, i.command_line, i.is_default)).collect()
    }

    #[test]
    fn package_json_scripts_become_commands_in_file_order() {
        let text = r#"{"name":"x","scripts":{"start":"node .","dev":"vite","build":"vite build","test":"vitest","lint:fix":"eslint --fix"}}"#;
        assert_eq!(
            node("npm", text),
            [
                ("start".into(), "npm start".into(), false),
                ("dev".into(), "npm run dev".into(), true),
                ("build".into(), "npm run build".into(), false),
                ("test".into(), "npm test".into(), false),
                ("lint:fix".into(), "npm run lint:fix".into(), false),
            ]
        );
        let cmds_of = |pm| node(pm, text).into_iter().map(|x| x.1).collect::<Vec<_>>();
        assert_eq!(cmds_of("pnpm"), ["pnpm start", "pnpm run dev", "pnpm run build", "pnpm test", "pnpm run lint:fix"]);
        assert_eq!(cmds_of("yarn"), ["yarn start", "yarn dev", "yarn build", "yarn test", "yarn lint:fix"]);
        assert_eq!(cmds_of("bun"), ["bun run start", "bun run dev", "bun run build", "bun run test", "bun run lint:fix"]);
    }

    #[test]
    fn the_default_is_dev_then_start_then_the_first() {
        let d = |t: &str| node("npm", t).into_iter().find(|x| x.2).map(|x| x.0);
        assert_eq!(d(r#"{"scripts":{"build":"b","start":"s"}}"#).as_deref(), Some("start"));
        assert_eq!(d(r#"{"scripts":{"build":"b","lint":"l"}}"#).as_deref(), Some("build"));
        assert_eq!(d(r#"{"scripts":{"start":"s","dev":"d"}}"#).as_deref(), Some("dev"));
    }

    #[test]
    fn lifecycle_hooks_and_unsafe_names_are_skipped() {
        let text = r#"{"scripts":{
            "prebuild":"x","build":"b","postbuild":"y","prepare":"husky","postinstall":"p","prepublishOnly":"q",
            "prettier":"prettier .","postcss":"postcss","pre":"z",
            "a&calc":"x","b c":"x","-x":"x","%PATH%":"x","q\"x":"x","x|y":"x","ok/@scope.v-1_2":"x","n":5
        }}"#;
        let labels: Vec<String> = node("npm", text).into_iter().map(|x| x.0).collect();
        assert_eq!(labels, ["build", "prettier", "postcss", "pre", "ok/@scope.v-1_2"]);
        assert!(!safe_script_name(&"a".repeat(65)));
        assert!(safe_script_name(&"a".repeat(64)));
    }

    #[test]
    fn odd_package_json_falls_back_without_crashing() {
        let ns = names(&["package.json"]);
        let fixed = ["npm start", "npm test", "npm run build"];
        for text in [
            "",
            "not json",
            "[]",
            "null",
            "42",
            r#"{"scripts":"x"}"#,
            r#"{"scripts":[1,2]}"#,
            r#"{"scripts":null}"#,
            r#"{"scripts":{}}"#,
            r#"{"scripts":{"preinstall":"x"}}"#,
            r#"{"scripts":{"a b":"x"}}"#,
        ] {
            let s = seed_for_project(&ns, Some(text)).unwrap();
            assert_eq!((s.kind, cmds(&s.items)), ("node", fixed.to_vec()), "{text:?}");
        }
        // Another project type wins over a package.json that happens to be there.
        assert_eq!(seed_for_project(&names(&["Cargo.toml", "package.json"]), Some(r#"{"scripts":{"dev":"x"}}"#)).unwrap().kind, "cargo");
    }

    #[test]
    fn a_big_monorepo_package_json_is_capped_keeping_order_and_the_default() {
        let (items, left_out) = node_scripts("pnpm", &monorepo_package_json()).unwrap();
        // build, build:web, test:e2e:ci, 85 pkg scripts, start, dev, release = 91 usable.
        assert_eq!(left_out, 91 - MAX_RUN_COMMANDS);
        assert_eq!(items.len(), MAX_RUN_COMMANDS);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(&labels[..4], ["build", "build:web", "test:e2e:ci", "pkg0:build"]);
        assert_eq!(labels.last().copied(), Some("dev"), "the default is kept although it comes after the cap");
        assert_eq!(items.iter().filter(|i| i.is_default).map(|i| i.command_line.as_str()).collect::<Vec<_>>(), ["pnpm run dev"]);
        let mut uniq = labels.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), labels.len(), "no duplicates");
        assert!(!labels.iter().any(|l| l.starts_with("pre") || l.starts_with("post") || l.contains(' ')), "{labels:?}");
        let s = seed_for_project(&names(&["package.json", "pnpm-lock.yaml"]), Some(&monorepo_package_json())).unwrap();
        assert_eq!((s.items.len(), s.left_out), (MAX_RUN_COMMANDS, 41));
    }

    #[test]
    fn a_real_package_json_is_read_and_a_huge_one_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().display().to_string();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(dir.path().join("Package.JSON"), r#"{"scripts":{"dev":"vite","build":"vite build"}}"#).unwrap();
        assert_eq!(cmds(&seed_for_folder(&folder).unwrap().items), ["pnpm run dev", "pnpm run build"]);
        let pad = " ".repeat(usize::try_from(MAX_PACKAGE_JSON).unwrap());
        std::fs::write(dir.path().join("Package.JSON"), format!(r#"{{"scripts":{{"dev":"vite"}}}}{pad}"#)).unwrap();
        assert_eq!(cmds(&seed_for_folder(&folder).unwrap().items), ["pnpm start", "pnpm test", "pnpm run build"]);
        std::fs::write(dir.path().join("Package.JSON"), [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(seed_for_folder(&folder).unwrap().kind, "node");
    }
}

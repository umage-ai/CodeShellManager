//! Run-command templates by project type (`RunCommandTemplatesService`): a top-level scan of the
//! session's folder, first match wins: dotnet → cargo → node → python → make. Templates are
//! COPIED onto a new session (fresh ids); later edits never flow back.
//!
//! [`seed_for_names`] is pure; [`seed_for_folder`] lists the folder (disk I/O, and `\\wsl$` can
//! boot a distro): worker threads only.

use csm_core::models::{RunCommandItem, RunMode};

/// The project type and its seed list, or `None` (unknown type, empty or missing folder).
pub fn seed_for_names(names: &[String]) -> Option<(&'static str, Vec<RunCommandItem>)> {
    let has = |n: &str| names.iter().any(|f| f.eq_ignore_ascii_case(n));
    let has_ext = |e: &str| names.iter().any(|f| f.len() > e.len() && f.get(f.len() - e.len()..).is_some_and(|x| x.eq_ignore_ascii_case(e)));
    if has_ext(".sln") || has_ext(".slnx") || has_ext(".csproj") {
        return Some(("dotnet", build(&[("Run", "dotnet run"), ("Build", "dotnet build"), ("Test", "dotnet test")])));
    }
    if has("Cargo.toml") {
        return Some(("cargo", build(&[("Run", "cargo run"), ("Build", "cargo build"), ("Test", "cargo test"), ("Clippy", "cargo clippy")])));
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
        // yarn's convention is `yarn build` (no `run`).
        let build_cmd = if pm == "yarn" { "yarn build".to_string() } else { format!("{pm} run build") };
        let start = format!("{pm} start");
        let test = format!("{pm} test");
        return Some(("node", build(&[("Start", &start), ("Test", &test), ("Build", &build_cmd)])));
    }
    if has("pyproject.toml") || has("requirements.txt") {
        return Some(("python", build(&[("Run", "python main.py"), ("Test", "python -m pytest")])));
    }
    if has("Makefile") || has("makefile") || has("GNUmakefile") {
        return Some(("make", build(&[("Run", "make"), ("Test", "make test"), ("Clean", "make clean")])));
    }
    None
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
/// share) and seeds from them. Blocking: worker threads only.
pub fn seed_for_folder(folder: &str) -> Option<(&'static str, Vec<RunCommandItem>)> {
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
    seed_for_names(&names)
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
        let (t, items) = seed_for_names(&names(&["Cargo.toml", "App.csproj", "package.json"])).unwrap();
        assert_eq!(t, "dotnet");
        assert_eq!(cmds(&items), ["dotnet run", "dotnet build", "dotnet test"]);
        assert_eq!(seed_for_names(&names(&["cargo.TOML", "Makefile"])).unwrap().0, "cargo");
        assert_eq!(seed_for_names(&names(&["requirements.txt", "Makefile"])).unwrap().0, "python");
        assert_eq!(seed_for_names(&names(&["makefile"])).unwrap().0, "make");
        assert!(seed_for_names(&names(&["README.md"])).is_none());
        assert!(seed_for_names(&[]).is_none());
    }

    #[test]
    fn node_follows_the_lock_file() {
        let pnpm = seed_for_names(&names(&["package.json", "pnpm-lock.yaml"])).unwrap().1;
        assert_eq!(cmds(&pnpm), ["pnpm start", "pnpm test", "pnpm run build"]);
        let yarn = seed_for_names(&names(&["package.json", "yarn.lock"])).unwrap().1;
        assert_eq!(cmds(&yarn), ["yarn start", "yarn test", "yarn build"]);
        let npm = seed_for_names(&names(&["package.json"])).unwrap().1;
        assert_eq!(cmds(&npm), ["npm start", "npm test", "npm run build"]);
    }

    #[test]
    fn exactly_one_default_and_fresh_ids() {
        let a = seed_for_names(&names(&["x.sln"])).unwrap().1;
        let b = seed_for_names(&names(&["x.sln"])).unwrap().1;
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
        assert_eq!(seed_for_folder(&dir.path().display().to_string()).unwrap().0, "node");
        assert!(seed_for_folder(&dir.path().join("missing").display().to_string()).is_none());
        assert!(seed_for_folder("  ").is_none());
    }
}

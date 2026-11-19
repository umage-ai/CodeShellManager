//! Pure worktree rules behind the git menu's worktree items (port of `GitWorktrees`, minus the
//! disk-touching `Canonical` and `NestedRepositories`).
//!
//! The comparison rules matter: Windows paths are case-insensitive, but inside a WSL distro
//! they are NOT (`/home/u/wt` and `/home/u/WT` are different folders; treating them as one would
//! close, and delete the sleeping sessions of, the wrong worktree). The distro name itself is
//! case-insensitive, and the two share spellings (`\\wsl$\` and `\\wsl.localhost\`) are one
//! place.

use super::status::GitRepoStatus;
use super::text::sync_text;
use crate::winpath::{eq_ic, starts_with_ic};

/// One worktree as reported by `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: Option<String>,
    pub is_bare: bool,
    pub is_detached: bool,
    pub is_locked: bool,
    pub is_prunable: bool,
}

/// Parses `git worktree list --porcelain`: blank-line separated stanzas of
/// `worktree <path>`, `HEAD <sha>`, `branch refs/heads/<name>` (or `detached` / `bare`),
/// `locked [reason]`, `prunable [reason]`.
#[must_use]
pub fn parse_worktree_list(raw: &str) -> Vec<WorktreeInfo> {
    let mut results = Vec::new();
    let mut cur = WorktreeInfo { path: String::new(), branch: None, is_bare: false, is_detached: false, is_locked: false, is_prunable: false };
    let empty = |path: &str| path.is_empty();
    let flush = |cur: &mut WorktreeInfo, results: &mut Vec<WorktreeInfo>| {
        if !empty(&cur.path) {
            results.push(cur.clone());
        }
        *cur = WorktreeInfo { path: String::new(), branch: None, is_bare: false, is_detached: false, is_locked: false, is_prunable: false };
    };
    for line in raw.replace('\r', "").split('\n') {
        if line.is_empty() {
            flush(&mut cur, &mut results);
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            cur.path = p.trim().to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            cur.branch = Some(b.trim().replace("refs/heads/", ""));
        } else if line == "bare" {
            cur.is_bare = true;
        } else if line == "detached" {
            cur.is_detached = true;
        } else if line.starts_with("locked") {
            cur.is_locked = true;
        } else if line.starts_with("prunable") {
            cur.is_prunable = true;
        }
    }
    flush(&mut cur, &mut results);
    results
}

/// Forward slashes, no trailing separator: the form git prints and compares. The two spellings
/// of a WSL share are folded to `//wsl$/`.
#[must_use]
pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let n = path.replace('\\', "/").trim_end_matches('/').to_string();
    const LOCALHOST: &str = "//wsl.localhost/";
    if n.get(..LOCALHOST.len()).is_some_and(|h| h.eq_ignore_ascii_case(LOCALHOST)) {
        return format!("//wsl$/{}", n.get(LOCALHOST.len()..).unwrap_or(""));
    }
    n
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Insensitive,
    Sensitive,
}

/// The comparable form of a path and how to compare it.
fn key(path: &str) -> (String, Case) {
    let n = normalize(path);
    const WSL: &str = "//wsl$/";
    if !starts_with_ic(&n, WSL) {
        return (n, Case::Insensitive);
    }
    let after = n.get(WSL.len()..).unwrap_or("");
    let (distro, rest) = match after.find('/') {
        Some(i) => (after.get(..i).unwrap_or(""), after.get(i..).unwrap_or("")),
        None => (after, ""),
    };
    (format!("{WSL}{}{rest}", distro.to_lowercase()), Case::Sensitive)
}

fn eq_case(a: &str, b: &str, c: Case) -> bool {
    match c {
        Case::Sensitive => a == b,
        Case::Insensitive => eq_ic(a, b),
    }
}

fn starts_case(a: &str, prefix: &str, c: Case) -> bool {
    match c {
        Case::Sensitive => a.starts_with(prefix),
        Case::Insensitive => starts_with_ic(a, prefix),
    }
}

/// Same folder, by the key rules: case per Windows / WSL, share spellings folded.
#[must_use]
pub fn same_path(a: &str, b: &str) -> bool {
    let (ka, c) = key(a);
    let (kb, _) = key(b);
    !ka.is_empty() && eq_case(&ka, &kb, c)
}

/// The key a git operation's busy slot is held under: the repo root (the MAIN working tree, so
/// every worktree of one repository shares it) when known, else the folder. Case-folded so one
/// set with ordinal comparison can hold Windows and WSL keys side by side.
#[must_use]
pub fn busy_key(repo_root: Option<&str>, working_folder: Option<&str>) -> String {
    let source = match repo_root {
        Some(r) if !r.is_empty() => r,
        _ => working_folder.unwrap_or(""),
    };
    let (k, c) = key(source);
    if c == Case::Insensitive { k.to_lowercase() } else { k }
}

/// True when `folder` is `root` or below it. A plain prefix test is wrong: `C:/repo-wt` starts
/// with `C:/repo` but is not inside it.
#[must_use]
pub fn is_inside(folder: &str, root: &str) -> bool {
    let (f, c) = key(folder);
    let (r, _) = key(root);
    if f.is_empty() || r.is_empty() {
        return false;
    }
    eq_case(&f, &r, c) || starts_case(&f, &format!("{r}/"), c)
}

/// The worktree containing `folder`: the deepest match, since a worktree can legally live inside
/// another one's folder. A bare entry has no working tree and never matches.
#[must_use]
pub fn containing<'a>(worktrees: &'a [WorktreeInfo], folder: &str) -> Option<&'a WorktreeInfo> {
    let mut best: Option<(&WorktreeInfo, usize)> = None;
    for w in worktrees.iter().filter(|w| !w.is_bare && is_inside(folder, &w.path)) {
        let len = normalize(&w.path).chars().count();
        // strictly longer wins: ties keep the first, like a stable OrderByDescending
        if best.is_none_or(|(_, l)| len > l) {
            best = Some((w, len));
        }
    }
    best.map(|(w, _)| w)
}

/// git lists the main worktree first; every other entry is a linked one.
#[must_use]
pub fn is_main(worktrees: &[WorktreeInfo], wt: &WorktreeInfo) -> bool {
    worktrees.first().is_some_and(|first| same_path(&first.path, &wt.path))
}

/// Branches checked out in a worktree OTHER than `current`, mapped to that worktree, in listing
/// order (first wins on a duplicate). git allows a branch in only one worktree at a time, so
/// switching to one of these always fails ("already used by worktree at ..."); the menu offers
/// to go there instead.
#[must_use]
pub fn branches_elsewhere<'a>(worktrees: &'a [WorktreeInfo], current: Option<&WorktreeInfo>) -> Vec<(&'a str, &'a WorktreeInfo)> {
    let mut map: Vec<(&str, &WorktreeInfo)> = Vec::new();
    for w in worktrees {
        let Some(branch) = w.branch.as_deref().filter(|b| !b.is_empty()) else {
            continue;
        };
        if w.is_bare {
            continue;
        }
        if current.is_some_and(|c| same_path(&w.path, &c.path)) {
            continue;
        }
        if !map.iter().any(|(b, _)| *b == branch) {
            map.push((branch, w));
        }
    }
    map
}

/// Other worktrees that live INSIDE `target`'s folder. Removing the target would delete them
/// too: git's clean check does not look inside an ignored folder, and a nested worktree usually
/// sits in one (`.worktrees/` in `.gitignore`). Reproduced with git 2.54: `worktree remove` on
/// the outer one exited 0 and deleted the inner one with its uncommitted file. So the menu
/// refuses while any are listed.
#[must_use]
pub fn nested_inside<'a>(worktrees: &'a [WorktreeInfo], target: &WorktreeInfo) -> Vec<&'a WorktreeInfo> {
    worktrees.iter().filter(|w| !w.is_bare && !same_path(&w.path, &target.path) && is_inside(&w.path, &target.path)).collect()
}

/// The worktree's folder name, which is what the menus show.
#[must_use]
pub fn name(path: &str) -> String {
    let n = normalize(path);
    match n.rfind('/') {
        Some(i) => n.get(i + 1..).unwrap_or("").to_string(),
        None => n,
    }
}

/// `wt-feature  ⎇ feature ● ↑1 ↓2`, `wt-old — folder missing`, `... 🔒`. The status is optional:
/// it is fetched lazily and may not be there (or may have failed).
#[must_use]
pub fn label(w: &WorktreeInfo, status: Option<&GitRepoStatus>) -> String {
    let n = name(&w.path);
    if w.is_prunable {
        return format!("{n} — folder missing");
    }
    let branch = match w.branch.as_deref() {
        Some(b) if !w.is_detached && !b.is_empty() => b,
        _ => "detached",
    };
    let mut label = format!("{n}  ⎇ {branch}");
    if status.is_some_and(GitRepoStatus::is_dirty) {
        label.push_str(" ●");
    }
    let sync = sync_text(status.and_then(|s| s.ahead), status.and_then(|s| s.behind));
    if !sync.is_empty() {
        label.push(' ');
        label.push_str(&sync);
    }
    if w.is_locked {
        label.push_str(" 🔒");
    }
    label
}

/// Which main repo a folder belongs to, from the text of its `.git` FILE (pure; the caller reads
/// the file). A linked worktree's `.git` is a file `gitdir: <common>/worktrees/<name>`, but a
/// submodule's is a file too (`gitdir: .../.git/modules/<name>`), so the file alone is not
/// enough: the `/worktrees/` segment is what tells them apart.
///
/// Returns the main repo's name for a linked worktree, `None` for a submodule or anything that
/// is not a `gitdir:` pointer. `repo_root` is the main working tree when known; otherwise the
/// folder holding `<common>/.git` is used, and `"another checkout"` as a last resort.
#[must_use]
pub fn linked_worktree_of(dot_git_file_text: &str, repo_root: Option<&str>) -> Option<String> {
    let text = dot_git_file_text.trim();
    let head = text.get(.."gitdir:".len())?;
    if !head.eq_ignore_ascii_case("gitdir:") {
        return None;
    }
    let gitdir = normalize(text.get("gitdir:".len()..).unwrap_or("").trim());
    if !gitdir.to_lowercase().contains("/worktrees/") {
        return None;
    }
    if let Some(root) = repo_root.filter(|r| !r.is_empty()) {
        return Some(name(root));
    }
    let lower = gitdir.to_lowercase();
    match lower.find("/.git/worktrees/") {
        Some(i) if i > 0 => Some(name(gitdir.get(..i).unwrap_or(""))),
        _ => Some("another checkout".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(path: &str, branch: Option<&str>) -> WorktreeInfo {
        WorktreeInfo {
            path: path.into(),
            branch: branch.map(str::to_string),
            is_bare: false,
            is_detached: false,
            is_locked: false,
            is_prunable: false,
        }
    }

    // ── git busy-slot key (#181) ───────────────────────────────────────────────────────

    #[test]
    fn busy_key_prefers_the_repo_root_over_the_folder() {
        // Two worktrees of one repo: different folders, one RepoRoot (the main working tree).
        assert_eq!(busy_key(Some("C:/src/app"), Some(r"C:\src\app-wt-feature")), busy_key(Some("C:/src/app"), Some(r"C:\src\app\sub")));
        assert_eq!(busy_key(None, Some(r"C:\src\app")), busy_key(Some(""), Some("C:/src/app/")));
    }

    #[test]
    fn busy_key_folds_spelling_and_case_on_windows() {
        assert_eq!(busy_key(Some(r"C:\Src\App\"), None), busy_key(Some("c:/src/app"), None));
    }

    #[test]
    fn busy_key_is_case_sensitive_inside_wsl_but_not_for_the_distro() {
        assert_eq!(busy_key(Some(r"\\wsl.localhost\Ubuntu\home\u\repo"), None), busy_key(Some(r"\\wsl$\ubuntu\home\u\repo"), None));
        assert_ne!(busy_key(Some(r"\\wsl$\Ubuntu\home\u\Repo"), None), busy_key(Some(r"\\wsl$\Ubuntu\home\u\repo"), None));
    }

    #[test]
    fn is_inside_is_a_path_test_not_a_string_prefix() {
        for (folder, root, expected) in [
            ("C:/repo", "C:/repo", true),
            ("C:/repo/src/app", "C:/repo", true),
            (r"C:\repo\src", "C:/repo", true), // mixed separators
            ("c:/REPO/src", "C:/repo/", true), // case, trailing slash
            ("C:/repo-wt", "C:/repo", false),  // a prefix is not "inside"
            ("C:/repo2/x", "C:/repo", false),
            ("", "C:/repo", false),
        ] {
            assert_eq!(is_inside(folder, root), expected, "{folder} in {root}");
        }
    }

    #[test]
    fn wsl_paths_follow_linux_case_rules_and_both_share_spellings() {
        // Inside a distro, Linux paths are case-sensitive: /home/u/WT is a different folder,
        // and matching it would close (and delete the sleeping) sessions of the wrong worktree.
        for (folder, root, expected) in [
            (r"\\wsl$\Ubuntu\home\u\WT", r"\\wsl$\Ubuntu\home\u\wt", false),
            (r"\\wsl$\Ubuntu\home\u\wt\src", r"\\wsl$\Ubuntu\home\u\wt", true),
            // ...but the distro name is not, and the two share spellings are the same place.
            (r"\\wsl$\ubuntu\home\u\wt", r"\\wsl$\Ubuntu\home\u\wt", true),
            (r"\\wsl.localhost\Ubuntu\home\u\wt", r"\\wsl$\Ubuntu\home\u\wt", true),
            (r"\\WSL.LOCALHOST\Ubuntu\home\u\wt\x", r"\\wsl$\Ubuntu\home\u\wt", true),
        ] {
            assert_eq!(is_inside(folder, root), expected, "{folder} in {root}");
            let strictly_below = folder.ends_with(r"\src") || folder.ends_with(r"\x");
            assert_eq!(same_path(folder, root), expected && !strictly_below, "{folder}");
        }
    }

    #[test]
    fn nested_inside_finds_worktrees_living_in_the_target_folder_only() {
        let outer = wt("C:/w/a", Some("a"));
        let wts = vec![wt("C:/w/main", Some("main")), outer.clone(), wt("C:/w/a/.worktrees/b", Some("b")), wt("C:/w/a-sibling", Some("s"))];
        let nested: Vec<_> = nested_inside(&wts, &outer).iter().map(|w| w.branch.clone()).collect();
        assert_eq!(nested, [Some("b".to_string())]);
        assert!(nested_inside(&wts, &wts[2]).is_empty());
    }

    #[test]
    fn containing_picks_the_deepest_worktree() {
        // A worktree may legally live inside another one's folder.
        let wts = vec![wt("C:/repo", Some("main")), wt("C:/repo/.wt/feat", Some("feat")), wt("C:/repo-other", Some("x"))];
        assert_eq!(containing(&wts, "C:/repo/.wt/feat/src").unwrap().branch.as_deref(), Some("feat"));
        assert_eq!(containing(&wts, "C:/repo/src").unwrap().branch.as_deref(), Some("main"));
        assert!(containing(&wts, "D:/elsewhere").is_none());
    }

    #[test]
    fn containing_ignores_the_bare_entry() {
        let mut bare = wt("C:/repo.git", None);
        bare.is_bare = true;
        let wts = vec![bare, wt("C:/repo.git/wt", Some("feat"))];
        assert_eq!(containing(&wts, "C:/repo.git/wt").unwrap().branch.as_deref(), Some("feat"));
        assert!(containing(&wts[..1], "C:/repo.git/sub").is_none());
    }

    #[test]
    fn branches_elsewhere_maps_every_other_worktrees_branch_but_not_this_ones() {
        let main = wt("C:/repo", Some("main"));
        let mut detached = wt("C:/wt-d", None);
        detached.is_detached = true;
        let mut bare = wt("C:/b.git", Some("x"));
        bare.is_bare = true;
        let wts = vec![main.clone(), wt("C:/wt-a", Some("feat-a")), detached, bare];
        let map = branches_elsewhere(&wts, Some(&main));
        assert_eq!(map.len(), 1);
        assert_eq!(map[0].0, "feat-a");
        assert_eq!(map[0].1.path, "C:/wt-a");
    }

    #[test]
    fn branches_elsewhere_first_listing_wins_on_a_duplicate() {
        let wts = vec![wt("C:/a", Some("dup")), wt("C:/b", Some("dup"))];
        let map = branches_elsewhere(&wts, None);
        assert_eq!(map.len(), 1);
        assert_eq!(map[0].1.path, "C:/a");
    }

    #[test]
    fn is_main_is_the_first_entry_git_lists() {
        let wts = vec![wt("C:/repo", Some("main")), wt("C:/wt", Some("feat"))];
        assert!(is_main(&wts, &wts[0]));
        assert!(!is_main(&wts, &wts[1]));
        assert!(!is_main(&[], &wts[0]));
    }

    #[test]
    fn label_shows_branch_state_and_problems() {
        use super::super::status::GitFileChange;
        let dirty_ahead = GitRepoStatus {
            branch: Some("feat".into()),
            is_detached: false,
            upstream: Some("origin/feat".into()),
            ahead: Some(1),
            behind: Some(2),
            changes: vec![GitFileChange { code: ".M".into(), path: "a.txt".into() }],
        };
        assert_eq!(label(&wt("C:/x/wt-feat", Some("feat")), Some(&dirty_ahead)), "wt-feat  ⎇ feat ● ↑1 ↓2");
        let mut detached = wt("C:/wt", None);
        detached.is_detached = true;
        assert_eq!(label(&detached, None), "wt  ⎇ detached");
        let mut locked = wt("C:/wt", Some("feat"));
        locked.is_locked = true;
        assert_eq!(label(&locked, None), "wt  ⎇ feat 🔒");
        let mut gone = wt("C:/gone", Some("feat"));
        gone.is_prunable = true;
        assert_eq!(label(&gone, None), "gone — folder missing");
    }

    #[test]
    fn name_is_the_last_segment() {
        assert_eq!(name(r"C:\a\b\wt-x\"), "wt-x");
        assert_eq!(name("plain"), "plain");
        assert_eq!(name(""), "");
    }

    // ── worktree list parsing ──────────────────────────────────────────────────────────

    #[test]
    fn parses_worktree_list_porcelain() {
        let raw = "worktree C:/repo\nHEAD abc\nbranch refs/heads/main\n\n\
                   worktree C:/wt feat\nHEAD def\nbranch refs/heads/feat/x\nlocked because\n\n\
                   worktree C:/wt-d\nHEAD 111\ndetached\nprunable gitdir file points to non-existent location\n\n\
                   worktree C:/bare.git\nbare\n";
        let list = parse_worktree_list(raw);
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].path, "C:/repo");
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert_eq!(list[1].path, "C:/wt feat");
        assert_eq!(list[1].branch.as_deref(), Some("feat/x"));
        assert!(list[1].is_locked);
        assert!(list[2].is_detached && list[2].is_prunable);
        assert_eq!(list[2].branch, None);
        assert!(list[3].is_bare);
    }

    #[test]
    fn worktree_list_tolerates_crlf_missing_trailing_blank_and_garbage() {
        let raw = "worktree C:/a\r\nbranch refs/heads/m\r\n";
        let list = parse_worktree_list(raw);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, "C:/a");
        assert!(parse_worktree_list("").is_empty());
        assert!(parse_worktree_list("HEAD abc\nbranch refs/heads/x\n").is_empty());
    }

    // ── gitdir pointer ─────────────────────────────────────────────────────────────────

    #[test]
    fn linked_worktree_is_told_apart_from_a_submodule() {
        assert_eq!(linked_worktree_of("gitdir: C:/src/main/.git/worktrees/wt-feat\n", Some("C:/src/main")).as_deref(), Some("main"));
        // no repo root: derived from the path in the pointer
        assert_eq!(linked_worktree_of("gitdir: C:/src/CodeShellManager/.git/worktrees/wt\n", None).as_deref(), Some("CodeShellManager"));
        assert_eq!(linked_worktree_of("gitdir: /x/worktrees/y", None).as_deref(), Some("another checkout"));
        // A submodule's .git is also a file, but points into .git/modules, not worktrees.
        assert_eq!(linked_worktree_of("gitdir: ../main/.git/modules/sub\n", Some("C:/x")), None);
        assert_eq!(linked_worktree_of("not a pointer", None), None);
        assert_eq!(linked_worktree_of("", None), None);
        assert_eq!(linked_worktree_of("é", None), None);
        assert_eq!(linked_worktree_of("GITDIR: C:/a/.git/worktrees/b", Some("C:/a")).as_deref(), Some("a"));
    }

    #[test]
    fn normalize_folds_the_wsl_spellings() {
        assert_eq!(normalize(r"\\wsl.localhost\Ubuntu\home\u\"), "//wsl$/Ubuntu/home/u");
        assert_eq!(normalize(r"C:\x\"), "C:/x");
        assert_eq!(normalize(""), "");
    }
}

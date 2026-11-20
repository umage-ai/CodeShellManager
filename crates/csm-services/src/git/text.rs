//! Wording for the terminal toolbar's git chip and menu: most of what the user actually sees of
//! the git feature, and therefore unit-tested. Port of `GitStatusText`.

/// `"↑2 ↓5"`, `"↑2"`, `"↓5"`, or `""` when level with (or not tracking) an upstream.
#[must_use]
pub fn sync_text(ahead: Option<i32>, behind: Option<i32>) -> String {
    let up = match ahead {
        Some(a) if a > 0 => format!("↑{a}"),
        _ => String::new(),
    };
    let down = match behind {
        Some(b) if b > 0 => format!("↓{b}"),
        _ => String::new(),
    };
    [up, down].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ")
}

/// The disabled first line of the menu, e.g. `main → origin/main · ↑2 ↓5 · 3 changed` or
/// `main · no upstream · clean`. In a linked worktree, `worktree_of` (the main repo's name) adds
/// ` · worktree of X`, since otherwise two panes on one repo differ only by folder path.
#[must_use]
pub fn menu_header(
    branch: Option<&str>,
    upstream: Option<&str>,
    ahead: Option<i32>,
    behind: Option<i32>,
    change_count: usize,
    worktree_of: Option<&str>,
) -> String {
    let head = match branch {
        Some(b) if !b.is_empty() => b,
        _ => "detached HEAD",
    };
    let upstream = upstream.filter(|u| !u.is_empty());
    let track = match upstream {
        None => " · no upstream".to_string(),
        Some(u) => format!(" → {u}"),
    };
    let mut sync = sync_text(ahead, behind);
    // git omits "# branch.ab" when the upstream is configured but its remote branch no longer
    // exists (deleted after merge, say). That is not "up to date".
    if upstream.is_some() && sync.is_empty() {
        sync = if ahead.is_none() { "upstream gone".to_string() } else { "up to date".to_string() };
    }
    let changes = match change_count {
        0 => "clean".to_string(),
        1 => "1 changed".to_string(),
        n => format!("{n} changed"),
    };
    let mut out = format!("{head}{track}");
    if !sync.is_empty() {
        out.push_str(&format!(" · {sync}"));
    }
    out.push_str(&format!(" · {changes}"));
    if let Some(w) = worktree_of.filter(|w| !w.is_empty()) {
        out.push_str(&format!(" · worktree of {w}"));
    }
    out
}

/// Toast text after a successful fetch, from the freshly re-read status.
#[must_use]
pub fn fetch_summary(branch: Option<&str>, upstream: Option<&str>, ahead: Option<i32>, behind: Option<i32>) -> String {
    let (Some(branch), Some(upstream), Some(behind)) = (branch.filter(|b| !b.is_empty()), upstream.filter(|u| !u.is_empty()), behind) else {
        return "Fetched.".to_string();
    };
    // C#: `behind == 0 && ahead is null or 0`, i.e. behind == 0 && (ahead is null or 0)
    if behind == 0 && matches!(ahead, None | Some(0)) {
        return format!("{branch} is up to date with {upstream}.");
    }
    if behind == 0 {
        return format!("Nothing new — {branch} is {} ahead of {upstream}.", ahead.unwrap_or(0));
    }
    let commits = if behind == 1 { "1 new commit".to_string() } else { format!("{behind} new commits") };
    match ahead {
        Some(a) if a > 0 => format!("{commits} on {upstream}; {branch} has diverged ({a} ahead)."),
        _ => format!("{commits} on {upstream} — Pull to fast-forward."),
    }
}

/// A short, actionable reason for a failed fetch/pull/switch, recognised from git's own message;
/// `None` when the message is not one we know (the panel then shows git's text).
#[must_use]
pub fn failure_hint(output: &str) -> Option<&'static str> {
    let o = output.to_lowercase();
    let has = |s: &str| o.contains(&s.to_lowercase());

    // Worktree refusals first: their wording overlaps the generic cases below. Each was
    // captured from git 2.54.
    if has("is already used by worktree at") || has("is already checked out at") {
        return Some("That branch is checked out in another worktree. Open that worktree instead (Worktrees ▸).");
    }
    if has("contains modified or untracked files") {
        return Some("The worktree has uncommitted or untracked files. Commit or stash them first.");
    }
    if has("cannot remove a locked working tree") {
        return Some("The worktree is locked. Unlock it with 'git worktree unlock' first.");
    }
    if has("is a main working tree") {
        return Some("That is the repository's main checkout, which can't be removed as a worktree.");
    }
    if has("is not fully merged") {
        return Some("The branch has commits that aren't merged anywhere, so it was kept.");
    }
    if has("failed to delete") && has("Permission denied") {
        return Some(
            "Something still has files open in that folder — an editor, a terminal or a dev \
             server. Close it, then finish in a terminal with 'git worktree remove --force <folder>'. \
             Everything in it was committed.",
        );
    }

    if has("Not possible to fast-forward") || has("diverging branches") || has("have diverged") {
        return Some("The branch has diverged from its upstream, so it can't fast-forward. Merge or rebase in a terminal.");
    }
    if has("would be overwritten") {
        return Some("Local changes would be overwritten. Commit or stash them first.");
    }
    if has("terminal prompts disabled") || has("could not read Username") || has("Authentication failed") {
        return Some("The remote needs credentials. Run git fetch once in a terminal to sign in.");
    }
    if has("Permission denied (publickey)") || has("Host key verification failed") {
        return Some("SSH authentication failed. Check your key or ssh-agent.");
    }
    if has("no tracking information") || has("There is no tracking information") {
        return Some("This branch has no upstream to pull from.");
    }
    if has("Could not resolve host") || has("unable to access") {
        return Some("Couldn't reach the remote. Check the network.");
    }
    if has("index.lock") {
        return Some("Another git command is running in this repo. Try again in a moment.");
    }
    None
}

/// One line for a toast after a successful pull: `Already up to date.` or the diffstat summary.
#[must_use]
pub fn summarize_pull(output: &str) -> String {
    let cleaned = output.replace('\r', "");
    let lines: Vec<&str> = cleaned.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.iter().any(|l| l.to_lowercase().starts_with("already up to date")) {
        return "Already up to date.".to_string();
    }
    if let Some(stat) = lines.iter().rev().find(|l| is_diffstat_line(l)) {
        return format!("Fast-forwarded: {stat}");
    }
    if lines.iter().any(|l| l.eq_ignore_ascii_case("Fast-forward")) { "Fast-forwarded.".to_string() } else { "Pull finished.".to_string() }
}

/// `^\d+ files? changed`
fn is_diffstat_line(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    let rest = line.get(digits..).unwrap_or("");
    let rest = match rest.strip_prefix(" file") {
        Some(r) => r,
        None => return false,
    };
    let rest = rest.strip_prefix('s').unwrap_or(rest);
    rest.starts_with(" changed")
}

/// git writes progress and most of its prose to stderr, even on success: join the non-empty
/// trimmed streams with a newline.
#[must_use]
pub fn combine_output(stdout: &str, stderr: &str) -> String {
    [stdout, stderr].iter().map(|s| s.replace("\r\n", "\n").trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_cases() {
        for (a, b, expected) in
            [(None, None, ""), (Some(0), Some(0), ""), (Some(2), Some(0), "↑2"), (Some(0), Some(5), "↓5"), (Some(2), Some(5), "↑2 ↓5")]
        {
            assert_eq!(sync_text(a, b), expected);
        }
    }

    #[test]
    fn menu_header_variants() {
        assert_eq!(menu_header(Some("main"), Some("origin/main"), Some(2), Some(5), 3, None), "main → origin/main · ↑2 ↓5 · 3 changed");
        assert_eq!(menu_header(Some("main"), Some("origin/main"), Some(0), Some(0), 0, None), "main → origin/main · up to date · clean");
        assert_eq!(menu_header(Some("main"), Some("origin/main"), None, None, 1, None), "main → origin/main · upstream gone · 1 changed");
        assert_eq!(menu_header(Some("wip"), None, None, None, 0, None), "wip · no upstream · clean");
        assert_eq!(menu_header(None, None, None, None, 0, None), "detached HEAD · no upstream · clean");
    }

    #[test]
    fn header_names_the_main_repo_only_in_a_linked_worktree() {
        assert!(menu_header(Some("feat"), None, None, None, 0, Some("CodeShellManager")).ends_with(" · worktree of CodeShellManager"));
        assert!(!menu_header(Some("main"), None, None, None, 0, None).contains("worktree"));
    }

    #[test]
    fn fetch_summary_variants() {
        assert_eq!(fetch_summary(Some("main"), None, None, None), "Fetched.");
        assert_eq!(fetch_summary(Some("main"), Some("origin/main"), Some(0), Some(0)), "main is up to date with origin/main.");
        assert_eq!(fetch_summary(Some("main"), Some("origin/main"), Some(2), Some(0)), "Nothing new — main is 2 ahead of origin/main.");
        assert_eq!(fetch_summary(Some("main"), Some("origin/main"), Some(0), Some(1)), "1 new commit on origin/main — Pull to fast-forward.");
        assert_eq!(fetch_summary(Some("main"), Some("origin/main"), Some(2), Some(4)), "4 new commits on origin/main; main has diverged (2 ahead).");
    }

    #[test]
    fn fetch_summary_edges() {
        // no ahead count (upstream without branch.ab) behaves like level
        assert_eq!(fetch_summary(Some("main"), Some("origin/main"), None, Some(0)), "main is up to date with origin/main.");
        assert_eq!(fetch_summary(None, Some("o/m"), Some(0), Some(0)), "Fetched.");
        assert_eq!(fetch_summary(Some("m"), Some("o/m"), Some(0), None), "Fetched.");
    }

    #[test]
    fn failure_hint_recognises_common_errors() {
        for (output, fragment) in [
            ("fatal: Not possible to fast-forward, aborting.", "diverged"),
            ("error: Your local changes to the following files would be overwritten by merge:", "Commit or stash"),
            ("fatal: could not read Username for 'https://github.com': terminal prompts disabled", "credentials"),
            ("git@github.com: Permission denied (publickey).", "SSH"),
            ("There is no tracking information for the current branch.", "no upstream"),
            ("fatal: unable to access 'https://x/': Could not resolve host: x", "network"),
            ("fatal: Unable to create '.git/index.lock': File exists.", "Another git command"),
        ] {
            let hint = failure_hint(output).unwrap_or_else(|| panic!("no hint for {output}"));
            assert!(hint.contains(fragment), "{output}: {hint}");
        }
    }

    #[test]
    fn failure_hint_is_none_for_unknown_errors() {
        assert_eq!(failure_hint("fatal: something nobody anticipated"), None);
        assert_eq!(failure_hint(""), None);
    }

    #[test]
    fn worktree_refusals_get_a_plain_hint() {
        // Each message captured verbatim from git 2.54.
        for (output, fragment) in [
            ("fatal: 'feat' is already used by worktree at 'C:/tmp/wt-feat'", "another worktree"),
            ("fatal: '../wt-feat' contains modified or untracked files, use --force to delete it", "uncommitted or untracked"),
            ("fatal: cannot remove a locked working tree;\nuse 'remove -f -f' to override or unlock first", "locked"),
            ("fatal: '.' is a main working tree", "main checkout"),
            ("error: the branch 'feat' is not fully merged\nhint: If you are sure you want to delete it, run 'git branch -D feat'", "aren't merged"),
            ("error: failed to delete 'C:/tmp/wt-self': Permission denied", "files open"),
        ] {
            let hint = failure_hint(output).unwrap_or_else(|| panic!("no hint for {output}"));
            assert!(hint.contains(fragment), "{output}: {hint}");
        }
    }

    #[test]
    fn summarize_pull_cases() {
        for (output, expected) in [
            ("Already up to date.", "Already up to date."),
            (
                "Updating 1a2b..3c4d\nFast-forward\n a.txt | 2 +-\n 3 files changed, 10 insertions(+), 2 deletions(-)",
                "Fast-forwarded: 3 files changed, 10 insertions(+), 2 deletions(-)",
            ),
            ("Updating 1a2b..3c4d\nFast-forward", "Fast-forwarded."),
            ("something else", "Pull finished."),
            ("1 file changed, 1 insertion(+)", "Fast-forwarded: 1 file changed, 1 insertion(+)"),
        ] {
            assert_eq!(summarize_pull(output), expected);
        }
        assert!(!is_diffstat_line("files changed"));
        assert!(!is_diffstat_line("3 filesx changed"));
    }

    #[test]
    fn combine_output_joins_trimmed_non_empty_streams() {
        assert_eq!(combine_output("a\r\n", "\n b \n"), "a\nb");
        assert_eq!(combine_output("", "err"), "err");
        assert_eq!(combine_output("out", ""), "out");
        assert_eq!(combine_output("", ""), "");
    }
}

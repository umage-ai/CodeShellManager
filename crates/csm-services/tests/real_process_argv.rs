//! Validates our quoting and our `split_command_line` oracle against a REAL Windows process.
//!
//! The unit tests prove that `quote_for_cmd` and `split_command_line` agree with each other. That
//! alone could be two copies of one mistake, so here the command line is handed, verbatim
//! (`raw_arg`, no re-quoting by `std`), to `csm-argv-echo`, a separate process whose argv is
//! produced by the Windows C runtime rules (Rust's std parses `GetCommandLineW` with the
//! MSVCRT/`CommandLineToArgvW` argument rules every ordinary Windows program uses, including
//! `wsl.exe`, `ssh.exe` and `git.exe`). What that process received must equal what we meant.
//!
//! Windows only: other platforms have no such command-line model.
#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use csm_services::argv::{join_argv, quote_for_cmd, split_command_line};
use csm_services::git::{WSL_GIT_SCRIPT, build_local_git_command_line, build_wsl_git_command_line};
use csm_services::launch::{LaunchSpec, SessionKind, build_ssh_args, build_wsl_args};
use std::os::windows::process::CommandExt;
use std::process::Command;

/// What a real process receives for this raw command-line tail.
fn real_argv(raw_tail: &str) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_csm-argv-echo"))
        .raw_arg(raw_tail)
        .output()
        .expect("spawn csm-argv-echo");
    assert!(out.status.success(), "echo helper failed for: {raw_tail}");
    let text = String::from_utf8(out.stdout).expect("utf-8 argv");
    let mut parts: Vec<String> = text.split('\0').map(str::to_string).collect();
    // output is NUL-terminated per argument, so the split leaves one empty tail
    assert_eq!(parts.pop().as_deref(), Some(""));
    parts
}

const VALUES: &[&str] = &[
    "plain",
    "has space",
    "tab\there",
    "say \"hi\"",
    "trailing\\",
    "trailing\\\\",
    "back\\\"slash-quote",
    "two\\\\\"bs",
    "sed -i 's/\\\"//g' f.txt",
    "grep -r \"\\\"foo\\\"\" .",
    "cp -r /src /dst\\",
    "",
    "$(id > /tmp/pwned)",
    "`id`",
    "a;id",
    "a|id",
    "a&&id",
    "a\"b",
    "a\\b",
    "$IFS",
    "a\nb",
    "%PATH%",
    "100%",
    "caret^pipe|lt<gt>",
    "uni\u{e7}\u{f8}d\u{e9} \u{2713}",
    "C:\\Program Files\\x y\\",
    "\"",
    "\"\"",
    "\\\"",
    "\\\\\"",
    " ",
    "  lead and trail  ",
    "--format=%(refname:short)",
    "-oProxyCommand=calc",
];

#[test]
fn a_real_process_receives_each_forced_quoted_value_unchanged() {
    for v in VALUES {
        let got = real_argv(&quote_for_cmd(v, true));
        assert_eq!(got, vec![(*v).to_string()], "forced: {v:?}");
    }
}

#[test]
fn a_real_process_receives_each_conditionally_quoted_value_unchanged() {
    for v in VALUES {
        let got = real_argv(&quote_for_cmd(v, false));
        assert_eq!(got, vec![(*v).to_string()], "conditional: {v:?}");
    }
}

#[test]
fn a_real_process_receives_a_joined_argv_element_for_element() {
    let argv: Vec<&str> = VALUES.to_vec();
    assert_eq!(real_argv(&join_argv(&argv)), argv);
}

#[test]
fn our_splitter_agrees_with_the_real_process_on_arbitrary_command_lines() {
    // Raw, hand-written lines, including shapes our own quoting never emits.
    for raw in [
        r#"a "b c" d"#,
        r#"a\"b"#,
        r#"a\\"b c""#,
        r#"a\\\"b"#,
        r"a\\b",
        r#""""#,
        "  a \t b  ",
        "a\nb",
        r#""a""b""#,
        r#"a "b c"#,
        r#"-p 2222 -t "u@h" "cd '/p' && bash""#,
        r#"x"y z"w v"#,
        r#""\"" """#,
        r#"\\\\"#,
        r#"a""#,
        r#""a b"c d"#,
    ] {
        assert_eq!(split_command_line(raw), real_argv(raw), "raw: {raw}");
    }
}

fn wsl_spec() -> LaunchSpec {
    LaunchSpec {
        kind: SessionKind::Wsl,
        wsl_distro: "My Distro".into(),
        wsl_user: "al ice".into(),
        wsl_working_folder: "/home/a b/\"q\"/".into(),
        command: "claude".into(),
        args: "--prompt \"fix the \\\"foo\\\" bug\"".into(),
        ..LaunchSpec::default()
    }
}

#[test]
fn the_wsl_launch_line_reaches_wsl_exe_as_the_intended_argv() {
    let line = build_wsl_args(&wsl_spec(), None).unwrap();
    assert_eq!(
        real_argv(&line),
        vec![
            "-d",
            "My Distro",
            "-u",
            "al ice",
            "--cd",
            "/home/a b/\"q\"/",
            "-e",
            "bash",
            "-lc",
            "claude --prompt \"fix the \\\"foo\\\" bug\"",
        ]
    );
}

#[test]
fn the_ssh_launch_line_keeps_hostile_values_out_of_ssh_options() {
    for folder in [
        "/t\" -oProxyCommand=calc \"x",
        "/it's/here",
        "/trailing\\",
        "/a'b\"c\\",
    ] {
        let spec = LaunchSpec {
            kind: SessionKind::Ssh,
            ssh_host: "h -oProxyCommand=calc".into(),
            ssh_remote_folder: folder.into(),
            command: "bash".into(),
            ..LaunchSpec::default()
        };
        let argv = real_argv(&build_ssh_args(&spec).unwrap());
        assert_eq!(argv.len(), 3, "{folder}");
        assert_eq!(argv[0], "-t");
        assert_eq!(argv[1], "h -oProxyCommand=calc");
        assert!(argv[2].starts_with("cd "));
    }
}

#[test]
fn hostile_git_arguments_reach_git_as_single_elements() {
    for payload in [
        "$(id)",
        "`id`",
        "a;id",
        "a b",
        "a\"b",
        "trailing\\\\",
        "a\nb",
    ] {
        let local = build_local_git_command_line(
            r"C:\repo",
            &["worktree", "add", "-b", payload, "--", r"C:\wt"],
        );
        assert_eq!(
            real_argv(&local),
            vec![
                "-C", r"C:\repo", "worktree", "add", "-b", payload, "--", r"C:\wt"
            ],
            "{payload:?}"
        );

        let wsl = build_wsl_git_command_line(
            "Ubuntu",
            "/home/a/repo",
            &["switch", "--no-guess", "--", payload],
        );
        let argv = real_argv(&wsl);
        let g = argv.iter().position(|a| a == "git").unwrap();
        assert_eq!(
            argv[..g],
            ["-d", "Ubuntu", "-e", "sh", "-lc", WSL_GIT_SCRIPT]
        );
        assert_eq!(
            argv[g..],
            [
                "git",
                "-C",
                "/home/a/repo",
                "switch",
                "--no-guess",
                "--",
                payload
            ]
        );
    }
}

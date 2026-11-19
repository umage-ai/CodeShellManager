//! Windows argv quoting and splitting (MSVCRT / `CommandLineToArgvW` rules).
//!
//! Every value that reaches `wsl.exe`, `ssh` or `git` goes through [`quote_for_cmd`] as one
//! argv element. There is no ad-hoc `replace`. This is a security boundary, not style: see
//! "Never interpolate a value into a command line" in the WPF repo's CLAUDE.md.

/// Win32 (MSVCRT / `CommandLineToArgvW`) argument quoting.
///
/// A non-empty value with no space, tab or `"` is returned unchanged unless `force` is set.
/// Otherwise it is wrapped in quotes; inside, a run of n backslashes followed by `"` becomes
/// 2n+1 backslashes and a quote, and a trailing run of n backslashes becomes 2n so it cannot
/// eat the closing quote.
///
/// Only space, tab and `"` trigger quoting. A newline alone does not (the Win32 tokenizer
/// does not split on it), which the C# original relied on and the tests pin.
#[must_use]
pub fn quote_for_cmd(value: &str, force: bool) -> String {
    if !force && !value.is_empty() && !value.contains([' ', '\t', '"']) {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in value.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                push_n(&mut out, '\\', backslashes * 2 + 1);
                out.push('"');
                backslashes = 0;
            }
            other => {
                push_n(&mut out, '\\', backslashes);
                out.push(other);
                backslashes = 0;
            }
        }
    }
    push_n(&mut out, '\\', backslashes * 2);
    out.push('"');
    out
}

fn push_n(s: &mut String, c: char, n: usize) {
    for _ in 0..n {
        s.push(c);
    }
}

/// Joins argv into a Windows command line, quoting each element with [`quote_for_cmd`].
///
/// Every git invocation is built this way rather than by interpolation: argv in, argv out,
/// so there is no string for a metacharacter to be a metacharacter in.
#[must_use]
pub fn join_argv<S: AsRef<str>>(args: &[S]) -> String {
    args.iter().map(|a| quote_for_cmd(a.as_ref(), false)).collect::<Vec<_>>().join(" ")
}

/// Wraps a value in POSIX single quotes, escaping an embedded `'` as `'\''`.
///
/// For values interpolated into a *remote shell* command line (ssh), where Windows argv
/// quoting is the wrong tool. It escapes `'` and nothing else: a `"` is a Windows-layer
/// problem, handled by quoting the whole remote command with [`quote_for_cmd`].
#[must_use]
pub fn posix_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Splits a command line into arguments with the MSVCRT / `CommandLineToArgvW` rules,
/// treating *every* token as an argument (there is no program-name token, which those APIs
/// parse with different rules; callers that have one pass the rest).
///
/// * Space and tab separate arguments outside quotes; nothing else does (not `\n`).
/// * 2n backslashes then `"` give n backslashes and toggle quote mode; 2n+1 give n backslashes
///   and a literal quote; backslashes not followed by `"` are literal.
/// * Inside quote mode, `""` is one literal `"` and quote mode continues (the post-2008
///   runtime rule; our own quoting never emits it).
///
/// This is the test oracle for the quoting above. It is validated against a real process in
/// `tests/real_process_argv.rs`.
#[must_use]
pub fn split_command_line(command_line: &str) -> Vec<String> {
    let chars: Vec<char> = command_line.chars().collect();
    let mut args = Vec::new();
    let mut i = 0usize;
    loop {
        while matches!(chars.get(i), Some(' ' | '\t')) {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut arg = String::new();
        let mut in_quotes = false;
        while let Some(&c) = chars.get(i) {
            match c {
                '\\' => {
                    let mut n = 0usize;
                    while chars.get(i) == Some(&'\\') {
                        n += 1;
                        i += 1;
                    }
                    if chars.get(i) == Some(&'"') {
                        push_n(&mut arg, '\\', n / 2);
                        if n % 2 == 1 {
                            arg.push('"');
                            i += 1;
                        }
                        // even: the quote is handled by the next loop turn as a toggle
                    } else {
                        push_n(&mut arg, '\\', n);
                    }
                }
                '"' => {
                    if in_quotes && chars.get(i + 1) == Some(&'"') {
                        arg.push('"');
                        i += 2;
                    } else {
                        in_quotes = !in_quotes;
                        i += 1;
                    }
                }
                ' ' | '\t' if !in_quotes => break,
                other => {
                    arg.push(other);
                    i += 1;
                }
            }
        }
        args.push(arg);
    }
    args
}

/// Splits a Windows-style command line into `(exe, args)` with a simple quote-aware
/// single-pass scan. The exe is unquoted; the args are returned verbatim (internal quoting
/// kept). Port of `CommandLineSplitter.Split`.
#[must_use]
pub fn split_exe_and_args(command_line: &str) -> (String, String) {
    if command_line.trim().is_empty() {
        return (String::new(), String::new());
    }
    let s = command_line.trim_start();

    if let Some(after_open) = s.strip_prefix('"') {
        return match after_open.find('"') {
            // unterminated: C# returns s.Trim('"') and no args
            None => (s.trim_matches('"').to_string(), String::new()),
            Some(closing) => {
                let exe = after_open.get(..closing).unwrap_or("").to_string();
                let rest = after_open.get(closing + 1..).unwrap_or("").trim_start().to_string();
                (exe, rest)
            }
        };
    }

    match s.find(' ') {
        None => (s.to_string(), String::new()),
        Some(sp) => (s.get(..sp).unwrap_or("").to_string(), s.get(sp + 1..).unwrap_or("").trim_start().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prefix a dummy program name like the C# helper did, then drop it. With our splitter
    /// the prefix changes nothing, but it keeps the call shape of the original tests.
    fn split(cmd: &str) -> Vec<String> {
        let mut v = split_command_line(&format!("x.exe {cmd}"));
        v.remove(0);
        v
    }

    // ── Win32CommandLineTests ──────────────────────────────────────────────────────────

    #[test]
    fn quote_for_cmd_round_trips_through_the_tokenizer() {
        for value in [
            "plain",
            "has space",
            "say \"hi\"",
            "trailing\\",
            "back\\\"slash-quote",
            "two\\\\\"bs",
            "sed -i 's/\\\"//g' f.txt",
            "grep -r \"\\\"foo\\\"\" .",
            "cp -r /src /dst\\",
            "",
        ] {
            let argv = split(&quote_for_cmd(value, true));
            assert_eq!(argv, vec![value.to_string()], "value {value:?}");
        }
    }

    // ── ShellSessionTests.QuoteForCmd_QuotesWhenNeeded ─────────────────────────────────

    #[test]
    fn quote_for_cmd_quotes_when_needed() {
        for (input, expected) in [
            ("Ubuntu", "Ubuntu"),
            ("", "\"\""),
            ("/home/alice/proj", "/home/alice/proj"),
            ("/home/alice/my proj", "\"/home/alice/my proj\""),
            ("with\"quote", "\"with\\\"quote\""),
        ] {
            assert_eq!(quote_for_cmd(input, false), expected);
        }
    }

    #[test]
    fn quote_for_cmd_leaves_newline_and_shell_metacharacters_unquoted() {
        // Only space, tab and `"` need quoting for the Win32 tokenizer; the rest is data.
        assert_eq!(quote_for_cmd("a\nb", false), "a\nb");
        assert_eq!(quote_for_cmd("a;id", false), "a;id");
        assert_eq!(quote_for_cmd("a\tb", false), "\"a\tb\"");
    }

    #[test]
    fn quote_for_cmd_doubles_backslashes_before_quotes_and_at_the_end() {
        assert_eq!(quote_for_cmd("a\\\"b", true), "\"a\\\\\\\"b\"");
        assert_eq!(quote_for_cmd("trail\\\\", true), "\"trail\\\\\\\\\"");
        // backslashes elsewhere are untouched
        assert_eq!(quote_for_cmd("a\\b c", false), "\"a\\b c\"");
    }

    #[test]
    fn join_argv_quotes_each_element() {
        assert_eq!(join_argv(&["-C", "a b", "x"]), "-C \"a b\" x");
        assert_eq!(join_argv::<&str>(&[]), "");
        assert_eq!(join_argv(&[""]), "\"\"");
    }

    // ── POSIX quoting (UntrustedInputTests / RunInstanceTests) ─────────────────────────

    #[test]
    fn posix_single_quote_escapes_every_quote() {
        assert_eq!(posix_single_quote("a'b"), r"'a'\''b'");
        assert_eq!(posix_single_quote(""), "''");
        assert_eq!(posix_single_quote("/x'; id; :'"), r"'/x'\''; id; :'\'''");
    }

    #[test]
    fn single_quote_escape_cases_from_run_instance() {
        assert_eq!(posix_single_quote("dotnet test"), "'dotnet test'");
        assert_eq!(posix_single_quote("can't"), r"'can'\''t'");
        assert_eq!(posix_single_quote(""), "''");
    }

    // ── splitter oracle self-checks ────────────────────────────────────────────────────

    #[test]
    fn splitter_follows_the_documented_rules() {
        assert_eq!(split_command_line(r#"a "b c" d"#), vec!["a", "b c", "d"]);
        assert_eq!(split_command_line(r#"a\"b"#), vec![r#"a"b"#]);
        assert_eq!(split_command_line(r#"a\\"b c""#), vec![r"a\b c"]);
        assert_eq!(split_command_line(r#"a\\\"b"#), vec![r#"a\"b"#]);
        assert_eq!(split_command_line(r"a\\b"), vec![r"a\\b"]);
        assert_eq!(split_command_line(r#""""#), vec![""]);
        assert_eq!(split_command_line("  a \t b  "), vec!["a", "b"]);
        assert_eq!(split_command_line("a\nb"), vec!["a\nb"]);
        assert_eq!(split_command_line(""), Vec::<String>::new());
        assert_eq!(split_command_line(r#""a""b""#), vec![r#"a"b"#]);
        // unterminated quote runs to the end
        assert_eq!(split_command_line(r#"a "b c"#), vec!["a", "b c"]);
    }

    // ── CommandLineSplitterTests ───────────────────────────────────────────────────────

    fn pair(cmd: &str) -> (String, String) {
        split_exe_and_args(cmd)
    }

    #[test]
    fn split_unquoted_exe_with_args() {
        assert_eq!(pair("cmd.exe /k foo"), ("cmd.exe".into(), "/k foo".into()));
    }

    #[test]
    fn split_quoted_exe_with_spaces_strips_quotes() {
        assert_eq!(pair("\"C:\\Program Files\\app.exe\" -x"), ("C:\\Program Files\\app.exe".into(), "-x".into()));
    }

    #[test]
    fn split_exe_only_returns_empty_args() {
        assert_eq!(pair("pwsh"), ("pwsh".into(), String::new()));
    }

    #[test]
    fn split_wsl_with_flags() {
        assert_eq!(pair("wsl.exe -d Ubuntu"), ("wsl.exe".into(), "-d Ubuntu".into()));
    }

    #[test]
    fn split_args_containing_quotes_are_preserved_verbatim() {
        assert_eq!(pair("bash -c \"echo hello\""), ("bash".into(), "-c \"echo hello\"".into()));
    }

    #[test]
    fn split_empty_or_whitespace_returns_empty() {
        assert_eq!(pair("   "), (String::new(), String::new()));
        assert_eq!(pair(""), (String::new(), String::new()));
    }

    #[test]
    fn split_leading_whitespace_trims() {
        assert_eq!(pair("  cmd /c echo hi"), ("cmd".into(), "/c echo hi".into()));
    }

    #[test]
    fn split_unterminated_quote_returns_trimmed_exe_and_no_args() {
        assert_eq!(pair("\"C:\\x y\\app.exe -x"), ("C:\\x y\\app.exe -x".into(), String::new()));
    }
}

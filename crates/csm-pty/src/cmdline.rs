//! Windows command-line assembly from argv (MSVCRT / `CommandLineToArgvW` rules).
//!
//! CreateProcess takes ONE string; the child splits it back into argv. Every element is quoted
//! so it survives that split as exactly one argument — values are never interpolated into a
//! command line (CLAUDE.md "Never interpolate a value into a command line"). Works on UTF-16 so
//! non-UTF-8 `OsString`s round-trip unchanged. Pure; compiled on every OS so it is tested
//! everywhere, used only by the Windows backend.

use std::fmt;

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;
const NEWLINE: u16 = b'\n' as u16;
const VTAB: u16 = 0x0b;

/// Why an argv could not be turned into a command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdLineError {
    Empty,
    /// argv[0] is parsed by different rules (no backslash escaping): a `"` cannot be expressed.
    QuoteInProgram,
    /// A NUL would truncate the command line.
    Nul,
}

impl fmt::Display for CmdLineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CmdLineError::Empty => f.write_str("empty argv"),
            CmdLineError::QuoteInProgram => f.write_str("program path contains a double quote"),
            CmdLineError::Nul => f.write_str("argument contains a NUL character"),
        }
    }
}

impl std::error::Error for CmdLineError {}

/// Appends one non-program argument, quoted by the MSVCRT rules.
pub fn push_quoted_arg(out: &mut Vec<u16>, arg: &[u16]) {
    let needs_quotes = arg.is_empty()
        || arg.iter().any(|&c| c == SPACE || c == TAB || c == NEWLINE || c == VTAB || c == QUOTE);
    if !needs_quotes {
        out.extend_from_slice(arg);
        return;
    }
    out.push(QUOTE);
    let mut backslashes = 0usize;
    for &c in arg {
        if c == BACKSLASH {
            backslashes += 1;
            continue;
        }
        if c == QUOTE {
            // 2n+1 backslashes then the quote: n literal backslashes and a literal quote.
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n(BACKSLASH, backslashes));
        }
        backslashes = 0;
        out.push(c);
    }
    // Backslashes before the closing quote are doubled so they stay literal.
    out.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
    out.push(QUOTE);
}

/// Builds the full command line (without a trailing NUL) from UTF-16 argv elements.
///
/// argv[0] follows CreateProcess's own rule: it runs to the first space unless quoted, and
/// backslashes are not escapes there — so it is quoted plainly and may not contain `"`.
pub fn build_command_line(argv: &[Vec<u16>]) -> Result<Vec<u16>, CmdLineError> {
    let (program, rest) = argv.split_first().ok_or(CmdLineError::Empty)?;
    if argv.iter().any(|a| a.contains(&0)) {
        return Err(CmdLineError::Nul);
    }
    if program.contains(&QUOTE) {
        return Err(CmdLineError::QuoteInProgram);
    }
    let mut out = Vec::new();
    if program.is_empty() || program.iter().any(|&c| c == SPACE || c == TAB) {
        out.push(QUOTE);
        out.extend_from_slice(program);
        out.push(QUOTE);
    } else {
        out.extend_from_slice(program);
    }
    for arg in rest {
        out.push(SPACE);
        push_quoted_arg(&mut out, arg);
    }
    Ok(out)
}

/// A command line whose arguments are one verbatim tail: the program quoted by CreateProcess's
/// rule, then a space and `tail` exactly as given. Only for `cmd.exe /c "<the user's own
/// command>"` (run commands), because cmd.exe does not split its command line by the MSVCRT
/// rules and an argv-quoted payload would reach it with `\"` escapes it doesn't understand. The
/// payload is the user's own command line, so a shell is the point; no app value goes in it.
pub fn build_command_line_verbatim(program: &[u16], tail: &[u16]) -> Result<Vec<u16>, CmdLineError> {
    if tail.contains(&0) {
        return Err(CmdLineError::Nul);
    }
    let mut out = build_command_line(&[program.to_vec()])?;
    if !tail.is_empty() {
        out.push(SPACE);
        out.extend_from_slice(tail);
    }
    Ok(out)
}

/// The command line as text, for logging and display only (lossy for unpaired surrogates).
pub fn display_command_line(line: &[u16]) -> String {
    String::from_utf16_lossy(line)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn line(args: &[&str]) -> String {
        let argv: Vec<Vec<u16>> = args.iter().map(|a| w(a)).collect();
        String::from_utf16(&build_command_line(&argv).unwrap()).unwrap()
    }

    #[test]
    fn plain_args_are_unquoted() {
        assert_eq!(line(&["cmd.exe", "/c", "echo"]), "cmd.exe /c echo");
    }

    #[test]
    fn spaces_and_empty_are_quoted() {
        assert_eq!(line(&["C:\\Program Files\\x.exe", "a b", ""]), "\"C:\\Program Files\\x.exe\" \"a b\" \"\"");
    }

    #[test]
    fn backslashes_before_quotes_are_doubled() {
        assert_eq!(line(&["x", "a\\\"b"]), "x \"a\\\\\\\"b\"");
        assert_eq!(line(&["x", "c:\\dir with space\\"]), "x \"c:\\dir with space\\\\\"");
        // No quoting needed → backslashes untouched.
        assert_eq!(line(&["x", "c:\\dir\\"]), "x c:\\dir\\");
    }

    #[test]
    fn verbatim_tail_is_appended_unchanged() {
        let l = build_command_line_verbatim(&w(r"C:\Windows\System32\cmd.exe"), &w(r#"/c "dotnet run --project "a b"""#)).unwrap();
        assert_eq!(String::from_utf16(&l).unwrap(), r#"C:\Windows\System32\cmd.exe /c "dotnet run --project "a b"""#);
        assert_eq!(build_command_line_verbatim(&w("x"), &w("a\0b")), Err(CmdLineError::Nul));
        assert_eq!(String::from_utf16(&build_command_line_verbatim(&w("x y"), &[]).unwrap()).unwrap(), "\"x y\"");
    }

    #[test]
    fn quote_in_program_is_refused() {
        let argv = vec![w("a\"b")];
        assert_eq!(build_command_line(&argv), Err(CmdLineError::QuoteInProgram));
    }

    #[test]
    fn nul_is_refused() {
        let argv = vec![w("x"), vec![b'a' as u16, 0]];
        assert_eq!(build_command_line(&argv), Err(CmdLineError::Nul));
    }
}

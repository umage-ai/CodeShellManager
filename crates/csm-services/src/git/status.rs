//! `git status --porcelain=v2 --branch` parsing.

/// One changed path from `git status`. `code` is the two-letter XY code (`"M "`, `" M"`, `"??"`,
/// `"UU"` ...); a rename's path reads `"old → new"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileChange {
    pub code: String,
    pub path: String,
}

/// A repo snapshot from one `git status --porcelain=v2 --branch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepoStatus {
    /// `None` when detached.
    pub branch: Option<String>,
    pub is_detached: bool,
    /// e.g. `origin/main`; `None` when the branch tracks nothing.
    pub upstream: Option<String>,
    /// `None` when there is no upstream (or it is gone).
    pub ahead: Option<i32>,
    pub behind: Option<i32>,
    pub changes: Vec<GitFileChange>,
}

impl GitRepoStatus {
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        !self.changes.is_empty()
    }
}

/// Outcome of a user-initiated git operation; `output` is stdout and stderr combined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOpResult {
    pub ok: bool,
    pub output: String,
    pub timed_out: bool,
}

/// Branch and dirty, as the sidebar and the git poll consume them. A detached HEAD reports
/// `(None, false)`, exactly as the old `branch --show-current` pair did.
#[must_use]
pub fn git_info(status: Option<&GitRepoStatus>) -> (Option<String>, bool) {
    match status {
        Some(s) if !s.is_detached => match s.branch.as_deref() {
            Some(b) if !b.is_empty() => (Some(b.to_string()), s.is_dirty()),
            _ => (None, false),
        },
        _ => (None, false),
    }
}

/// Parses `git status --porcelain=v2 --branch`. Hostile or unrecognised lines are skipped,
/// never fatal.
#[must_use]
pub fn parse_status_v2(output: &str) -> GitRepoStatus {
    let mut head: Option<String> = None;
    let mut upstream: Option<String> = None;
    let mut ahead: Option<i32> = None;
    let mut behind: Option<i32> = None;
    let mut changes = Vec::new();

    for line in output.replace('\r', "").split('\n') {
        let Some(first) = line.chars().next() else {
            continue;
        };
        if let Some(v) = line.strip_prefix("# branch.head ") {
            head = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("# branch.ab ") {
            // "# branch.ab +<ahead> -<behind>"
            let parts: Vec<&str> = v.split(' ').filter(|p| !p.is_empty()).collect();
            if let [a, b] = parts.as_slice()
                && let (Ok(a), Ok(b)) = (
                    a.trim_start_matches('+').parse::<i32>(),
                    b.trim_start_matches('-').parse::<i32>(),
                )
            {
                ahead = Some(a);
                behind = Some(b);
            }
        } else if first == '#' || line.starts_with("! ") {
            // other headers; ignored files are only listed with --ignored, never by us
        } else if let Some(path) = line.strip_prefix("? ") {
            changes.push(GitFileChange {
                code: "??".to_string(),
                path: unquote_c_path(path),
            });
        } else {
            // 1 XY sub mH mI mW hH hI <path>
            // 2 XY sub mH mI mW hH hI Xscore <path>\t<origPath>
            // u XY sub m1 m2 m3 mW h1 h2 h3 <path>
            let fields_before_path = match first {
                '1' => 8,
                '2' => 9,
                'u' => 10,
                _ => continue,
            };
            let f: Vec<&str> = line.splitn(fields_before_path + 1, ' ').collect();
            let (Some(xy), Some(raw_path)) = (f.get(1), f.get(fields_before_path)) else {
                continue;
            };
            if xy.chars().count() != 2 {
                continue;
            }
            let path = if first == '2' {
                // A tab or newline INSIDE a name is always C-quoted (\t, \n), so the first
                // literal tab is the separator: split before unquoting, never after.
                match raw_path.find('\t') {
                    Some(tab) => format!(
                        "{} → {}",
                        unquote_c_path(raw_path.get(tab + 1..).unwrap_or("")),
                        unquote_c_path(raw_path.get(..tab).unwrap_or(""))
                    ),
                    None => unquote_c_path(raw_path),
                }
            } else {
                unquote_c_path(raw_path)
            };
            // Unmerged entries keep their real XY: AA, DU, UD, AU, UA, DD and UU each say
            // something different about the conflict.
            changes.push(GitFileChange {
                code: xy.replace('.', " "),
                path,
            });
        }
    }

    let detached = head.as_deref() == Some("(detached)");
    GitRepoStatus {
        branch: if detached { None } else { head },
        is_detached: detached,
        upstream,
        ahead,
        behind,
        changes,
    }
}

/// Undoes git's C-style path quoting.
///
/// `core.quotePath=false` stops git escaping non-ASCII, but a name containing `"`, `\` or a
/// control character is still wrapped in quotes with backslash escapes, and with quoting on,
/// octal escapes (`\303\246`) are raw *bytes* of a UTF-8 name, so the result is decoded as
/// bytes, not char by char. Anything not wrapped in `"` is returned unchanged; a malformed or
/// unknown escape is kept literally rather than dropped.
#[must_use]
pub fn unquote_c_path(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 2 || chars.first() != Some(&'"') || chars.last() != Some(&'"') {
        return s.to_string();
    }
    let end = chars.len() - 1; // index of the closing quote
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 1usize;
    while i < end {
        let Some(&c) = chars.get(i) else { break };
        if c != '\\' || i + 1 >= end {
            // Ordinary text (including a lone trailing backslash): keep as UTF-8 so it joins
            // any octal-escaped bytes around it.
            let mut buf = [0u8; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            i += 1;
            continue;
        }
        i += 1;
        let e = chars.get(i).copied().unwrap_or('\\');
        match e {
            'a' => bytes.push(0x07),
            'b' => bytes.push(0x08),
            't' => bytes.push(0x09),
            'n' => bytes.push(0x0A),
            'v' => bytes.push(0x0B),
            'f' => bytes.push(0x0C),
            'r' => bytes.push(0x0D),
            '"' => bytes.push(b'"'),
            '\\' => bytes.push(b'\\'),
            _ => {
                let d1 = chars.get(i + 1).and_then(|c| c.to_digit(8));
                let d2 = chars.get(i + 2).and_then(|c| c.to_digit(8));
                match (e.to_digit(8), d1, d2) {
                    (Some(d0), Some(d1), Some(d2)) if d0 <= 3 && i + 2 < end => {
                        let v = (d0 << 6) | (d1 << 3) | d2;
                        bytes.push(u8::try_from(v).unwrap_or(b'?'));
                        i += 2;
                    }
                    _ => {
                        // not an escape we know: keep the backslash, re-read `e` as text
                        bytes.push(b'\\');
                        i -= 1;
                    }
                }
            }
        }
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured verbatim from git 2.54 (`-c core.quotePath=false status --porcelain=v2 --branch`)
    // on a repo with a modified file, a path with a space, a staged rename, a staged non-ASCII
    // path inside a spaced directory, and an untracked file.
    const CAPTURED: &str = "# branch.oid d168ec5ccba03426d0799f256ccb5f08c53d24b2\n\
# branch.head master\n\
# branch.upstream origin/master\n\
# branch.ab +1 -3\n\
1 .M N... 100644 100644 100644 78981922613b2afb6025042ff6bd878ac1994e85 78981922613b2afb6025042ff6bd878ac1994e85 a.txt\n\
1 .M N... 100644 100644 100644 61780798228d17af2d34fce4cfbdf35556832472 61780798228d17af2d34fce4cfbdf35556832472 my file.txt\n\
2 R. N... 100644 100644 100644 f2ad6c76f0115a6ba5b00456a849810e7ec0af20 f2ad6c76f0115a6ba5b00456a849810e7ec0af20 R100 new.txt\told.txt\n\
1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 bca70f35318f31dd1d1d1d2d2e64c19b880899ff sub dir/æø.txt\n\
? untracked.txt\n";

    #[test]
    fn parses_branch_upstream_and_ahead_behind() {
        let s = parse_status_v2(CAPTURED);
        assert_eq!(s.branch.as_deref(), Some("master"));
        assert!(!s.is_detached);
        assert_eq!(s.upstream.as_deref(), Some("origin/master"));
        assert_eq!(s.ahead, Some(1));
        assert_eq!(s.behind, Some(3));
    }

    #[test]
    fn parses_every_change_with_its_full_path() {
        let s = parse_status_v2(CAPTURED);
        assert!(s.is_dirty());
        let paths: Vec<&str> = s.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "a.txt",
                "my file.txt",
                "old.txt → new.txt",
                "sub dir/æø.txt",
                "untracked.txt"
            ]
        );
        let codes: Vec<&str> = s.changes.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, [" M", " M", "R ", "A ", "??"]);
    }

    #[test]
    fn tolerates_crlf() {
        let s = parse_status_v2(&CAPTURED.replace('\n', "\r\n"));
        assert_eq!(s.branch.as_deref(), Some("master"));
        assert_eq!(s.changes.len(), 5);
        assert!(!s.changes.iter().any(|c| c.path.ends_with('\r')));
    }

    #[test]
    fn clean_repo_without_upstream() {
        let s = parse_status_v2("# branch.oid abc\n# branch.head feature/x\n");
        assert_eq!(s.branch.as_deref(), Some("feature/x"));
        assert_eq!(s.upstream, None);
        assert_eq!(s.ahead, None);
        assert_eq!(s.behind, None);
        assert!(!s.is_dirty());
    }

    #[test]
    fn upstream_whose_remote_branch_is_gone_has_no_counts() {
        // git omits branch.ab in this case
        let s = parse_status_v2(
            "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n",
        );
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!(s.ahead, None);
    }

    #[test]
    fn detached_head_reports_no_branch() {
        let s = parse_status_v2("# branch.oid abc\n# branch.head (detached)\n");
        assert!(s.is_detached);
        assert_eq!(s.branch, None);
    }

    #[test]
    fn unmerged_entry_is_listed_as_conflict() {
        let s = parse_status_v2(
            "# branch.head main\n\
             u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflicted file.txt\n",
        );
        assert_eq!(s.changes.len(), 1);
        assert_eq!(s.changes[0].code, "UU");
        assert_eq!(s.changes[0].path, "conflicted file.txt");
    }

    #[test]
    fn unmerged_entries_keep_their_real_code() {
        // Shape as git 2.54 prints it for a merge with every kind of conflict (#181: all of
        // these used to be shown as "UU").
        let z = "0000000000000000000000000000000000000000";
        let h = "d00491fd7e5bb6fa28c517a0bb32b8b506539d4d";
        let text = format!(
            "# branch.head main\n\
             u AA N... 000000 100644 100644 100644 {z} {h} {h} added.txt\n\
             u DU N... 100644 000000 100644 100644 {h} {z} {h} deleted by us.txt\n\
             u UD N... 100644 100644 000000 100644 {h} {h} {z} delthem.txt\n\
             u AU N... 000000 100644 100644 100644 {z} {h} {h} au.txt\n\
             u UA N... 000000 100644 100644 100644 {z} {h} {h} ua.txt\n\
             u DD N... 100644 000000 000000 000000 {h} {z} {z} dd.txt\n\
             u UU N... 100644 100644 100644 100644 {h} {h} {h} mod.txt\n"
        );
        let s = parse_status_v2(&text);
        let codes: Vec<&str> = s.changes.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, ["AA", "DU", "UD", "AU", "UA", "DD", "UU"]);
        assert_eq!(s.changes[1].path, "deleted by us.txt");
    }

    #[test]
    fn quoted_paths_are_unquoted() {
        // git C-quotes these even with core.quotePath=false: '"', '\', and control chars.
        let h = "587be6b4c3f93f93c489c0111bba5596147a26cb";
        let z = "0000000000000000000000000000000000000000";
        let text = format!(
            "# branch.head main\n\
             1 A. N... 000000 100644 100644 {z} {h} \"q\\\"uote.txt\"\n\
             1 A. N... 000000 100644 100644 {z} {h} \"tab\\there.txt\"\n\
             1 A. N... 000000 100644 100644 {z} {h} \"back\\\\slash.txt\"\n\
             1 A. N... 000000 100644 100644 {z} {h} \"new\\nline.txt\"\n\
             1 A. N... 000000 100644 100644 {z} {h} \"æø \\\"q\\\".txt\"\n\
             2 R. N... 100644 100644 100644 {h} {h} R100 \"to \\\"x\\\".txt\"\tfrom\\plain.txt\n\
             ? \"un\\ttracked\"\n"
        );
        let s = parse_status_v2(&text);
        let paths: Vec<&str> = s.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "q\"uote.txt",
                "tab\there.txt",
                "back\\slash.txt",
                "new\nline.txt",
                "æø \"q\".txt",
                "from\\plain.txt → to \"x\".txt",
                "un\ttracked",
            ]
        );
    }

    #[test]
    fn unquote_c_path_cases() {
        for (quoted, expected) in [
            ("plain.txt", "plain.txt"),
            ("\"\"", ""),
            ("\"a\\\"b\"", "a\"b"),
            ("\"a\\\\b\"", "a\\b"),
            ("\"\\a\\b\\t\\n\\v\\f\\r\"", "\u{7}\u{8}\t\n\u{b}\u{c}\r"),
            // Octal escapes are UTF-8 bytes: \303\246\303\270 is "æø" (quotePath=true).
            ("\"\\303\\246\\303\\270.txt\"", "æø.txt"),
            // Mixed: raw non-ASCII next to escaped bytes still decodes as one UTF-8 sequence.
            ("\"æ\\303\\270\"", "æø"),
            // Malformed / unknown escapes are kept, not dropped.
            ("\"a\\qb\"", "a\\qb"),
            ("\"a\\9b\"", "a\\9b"),
            ("\"trailing\\\"", "trailing\\"),
            // Only fully wrapped values are quoted.
            ("\"half", "\"half"),
            ("half\"", "half\""),
        ] {
            assert_eq!(unquote_c_path(quoted), expected, "{quoted}");
        }
    }

    #[test]
    fn unquote_c_path_edge_cases_never_panic() {
        for s in [
            "\"",
            "",
            "\"\\\"",
            "\"\\3\"",
            "\"\\30\"",
            "\"\\377\\377\"",
            "\"\\400\"",
            "\"é\\",
        ] {
            let _ = unquote_c_path(s);
        }
        // invalid UTF-8 from octal bytes is replaced, not fatal
        assert_eq!(unquote_c_path("\"\\377\""), "\u{FFFD}");
        // a four-digit-looking run only consumes three digits
        assert_eq!(unquote_c_path("\"\\1011\""), "A1");
    }

    #[test]
    fn unrecognised_lines_are_ignored_not_fatal() {
        let s = parse_status_v2("# branch.head main\nwhat is this\n1 short\n# branch.ab garbage\n");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(s.changes.is_empty());
        assert_eq!(s.ahead, None);
    }

    #[test]
    fn ignored_entries_are_skipped_and_odd_xy_is_dropped() {
        let h = "a".repeat(40);
        let s = parse_status_v2(&format!(
            "# branch.head main\n! ignored.log\n1 X N... 100644 100644 100644 {h} {h} short-xy.txt\n"
        ));
        assert!(s.changes.is_empty());
    }

    #[test]
    fn git_info_keeps_the_old_contract() {
        let s = parse_status_v2("# branch.head main\n? x\n");
        assert_eq!(git_info(Some(&s)), (Some("main".to_string()), true));
        let d = parse_status_v2("# branch.head (detached)\n? x\n");
        assert_eq!(git_info(Some(&d)), (None, false));
        assert_eq!(git_info(None), (None, false));
        let empty = parse_status_v2("");
        assert_eq!(git_info(Some(&empty)), (None, false));
    }
}

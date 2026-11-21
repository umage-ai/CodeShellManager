//! Where a comment belongs: an anchor records what the person saw (lines, the text of those lines,
//! the blob it came from, two lines of context each side), and [`locate`] finds that place again in
//! the text as it is now. Pure; the text is untrusted and every index goes through `get`.
//!
//! The order, cheapest first (GitHub's "outdated comment" behaviour, with a snippet so a thread is
//! readable even when its code is gone):
//!
//! 1. the same blob, or the same lines at the same place: **exact**;
//! 2. the snippet verbatim somewhere else (the nearest occurrence wins): **moved**;
//! 3. the snippet with whitespace ignored: **moved**;
//! 4. the context lines either side, with the lines between them edited: **moved**;
//! 5. none of them: **outdated**.

use serde::{Deserialize, Serialize};

/// Longest snippet kept (bytes); more lines are dropped and `clipped` says so.
pub const SNIPPET_CAP: usize = 4096;
/// Context lines kept either side.
const CONTEXT: usize = 2;
/// Most snippet lines matched against the text (the rest is the clipped tail).
const MAX_NEEDLE: usize = 200;

fn new_side() -> String {
    "new".into()
}

/// What a thread is anchored to. `kind` is `lines` or `file` (a comment on the whole file).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub kind: String,
    /// Repository-relative, forward slashes, never absolute.
    pub path: String,
    /// The checkout the person was looking at (worktrees share a project).
    #[serde(default)]
    pub worktree: String,
    /// `working`, `staged`, `file` or `commit`.
    #[serde(default)]
    pub source: String,
    /// HEAD when the thread was written.
    #[serde(default)]
    pub commit: String,
    /// `new`, or `old` for a deleted or changed line in a diff.
    #[serde(default = "new_side")]
    pub side: String,
    /// The git blob id of the text that was seen (empty when unknown).
    #[serde(default)]
    pub blob: String,
    /// 1-based, inclusive, in that text. 0 for a `file` anchor.
    #[serde(default)]
    pub start: u32,
    #[serde(default)]
    pub end: u32,
    /// The lines `start..=end`, at most [`SNIPPET_CAP`] bytes.
    #[serde(default)]
    pub snippet: String,
    /// The snippet is shorter than the range.
    #[serde(default)]
    pub clipped: bool,
    #[serde(default)]
    pub before: Vec<String>,
    #[serde(default)]
    pub after: Vec<String>,
}

/// Where an anchor is in some text now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Exact { start: u32, end: u32 },
    Moved { start: u32, end: u32 },
    Outdated,
}

impl Location {
    /// `exact`, `moved` or `outdated`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Location::Exact { .. } => "exact",
            Location::Moved { .. } => "moved",
            Location::Outdated => "outdated",
        }
    }

    /// The current lines, when there are some.
    #[must_use]
    pub fn range(self) -> Option<(u32, u32)> {
        match self {
            Location::Exact { start, end } | Location::Moved { start, end } => Some((start, end)),
            Location::Outdated => None,
        }
    }
}

/// The lines of `text`: split on `\n`, a trailing `\r` dropped (CRLF files).
#[must_use]
pub fn split_lines(text: &str) -> Vec<&str> {
    text.lines().map(|l| l.strip_suffix('\r').unwrap_or(l)).collect()
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An anchor on lines `start..=end` (1-based) of `text`. `None` when the range is not in the text.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn lines_anchor(path: &str, side: &str, source: &str, commit: &str, worktree: &str, blob: &str, text: &str, start: u32, end: u32) -> Option<Anchor> {
    let lines = split_lines(text);
    let (s, e) = (usize::try_from(start).ok()?, usize::try_from(end).ok()?);
    if s == 0 || e < s || e > lines.len() {
        return None;
    }
    let mut snippet = String::new();
    let mut clipped = false;
    for l in lines.get(s - 1..e)? {
        if snippet.len() + l.len() + 1 > SNIPPET_CAP {
            clipped = true;
            break;
        }
        if !snippet.is_empty() {
            snippet.push('\n');
        }
        snippet.push_str(l);
    }
    let clip = |l: &&str| -> String { l.chars().take(300).collect() };
    let before: Vec<String> = lines.get(s.saturating_sub(1 + CONTEXT)..s - 1).unwrap_or(&[]).iter().map(clip).collect();
    let after: Vec<String> = lines.get(e..(e + CONTEXT).min(lines.len())).unwrap_or(&[]).iter().map(clip).collect();
    Some(Anchor {
        kind: "lines".into(),
        path: path.to_owned(),
        worktree: worktree.to_owned(),
        source: source.to_owned(),
        commit: commit.to_owned(),
        side: side.to_owned(),
        blob: blob.to_owned(),
        start,
        end,
        snippet,
        clipped,
        before,
        after,
    })
}

/// A comment on a whole file.
#[must_use]
pub fn file_anchor(path: &str, source: &str, commit: &str, worktree: &str) -> Anchor {
    Anchor { kind: "file".into(), path: path.to_owned(), worktree: worktree.to_owned(), source: source.to_owned(), commit: commit.to_owned(), side: new_side(), ..Anchor::default() }
}

/// The 0-based index in `hay` where `needle` starts, nearest to `near`; `norm_eq` compares lines
/// with whitespace ignored. `None` when it is nowhere.
fn find_block(hay: &[&str], needle: &[String], near: usize, norm_eq: bool) -> Option<usize> {
    let first = needle.first()?;
    let first_n = norm(first);
    let mut best: Option<usize> = None;
    for i in 0..hay.len() {
        let Some(line) = hay.get(i) else { break };
        let head_ok = if norm_eq { norm(line) == first_n } else { *line == first.as_str() };
        if !head_ok || i + needle.len() > hay.len() {
            continue;
        }
        let all = needle.iter().enumerate().all(|(k, n)| hay.get(i + k).is_some_and(|h| if norm_eq { norm(h) == norm(n) } else { *h == n.as_str() }));
        if all && best.is_none_or(|b| i.abs_diff(near) < b.abs_diff(near)) {
            best = Some(i);
        }
    }
    best
}

/// Finds the anchor in `text` (whose blob id is `blob`, or empty when unknown).
#[must_use]
pub fn locate(a: &Anchor, blob: &str, text: &str) -> Location {
    if a.kind == "file" {
        return Location::Exact { start: 0, end: 0 };
    }
    let lines = split_lines(text);
    let len_u32 = u32::try_from(lines.len()).unwrap_or(u32::MAX);
    let span = a.end.saturating_sub(a.start);
    let at = |start0: usize| -> (u32, u32) {
        let s = u32::try_from(start0 + 1).unwrap_or(u32::MAX);
        (s, s.saturating_add(span).min(len_u32.max(s)))
    };
    let stored_ok = a.start >= 1 && a.end >= a.start && a.end <= len_u32;
    if !a.blob.is_empty() && a.blob == blob {
        return if a.start >= 1 && a.end >= a.start { Location::Exact { start: a.start, end: a.end } } else { Location::Outdated };
    }
    let needle: Vec<String> = a.snippet.split('\n').take(MAX_NEEDLE).map(str::to_owned).collect();
    let blank = needle.iter().all(|l| l.trim().is_empty());
    if a.snippet.is_empty() && a.before.is_empty() && a.after.is_empty() {
        // Nothing to check against (an anchor made without the text): trust the numbers.
        return if stored_ok { Location::Exact { start: a.start, end: a.end } } else { Location::Outdated };
    }
    let near = usize::try_from(a.start.saturating_sub(1)).unwrap_or(0);
    if !blank {
        for norm_eq in [false, true] {
            if let Some(i) = find_block(&lines, &needle, near, norm_eq) {
                let (s, e) = at(i);
                let same_place = s == a.start && (a.clipped || e == a.end);
                return if same_place && !norm_eq { Location::Exact { start: s, end: e } } else { Location::Moved { start: s, end: e } };
            }
        }
    }
    // The lines between the context were edited: find the context, with the same number of lines between.
    let useful = a.before.iter().chain(&a.after).filter(|l| !l.trim().is_empty()).count();
    if useful >= 2 {
        let n = usize::try_from(span).unwrap_or(0) + 1;
        let mut best: Option<usize> = None;
        for i in 0..=lines.len() {
            let before_ok = a.before.iter().enumerate().all(|(k, b)| i.checked_sub(a.before.len() - k).and_then(|j| lines.get(j)).is_some_and(|l| norm(l) == norm(b)));
            if !before_ok {
                continue;
            }
            let after_ok = a.after.iter().enumerate().all(|(k, b)| lines.get(i + n + k).is_some_and(|l| norm(l) == norm(b)));
            if after_ok && i + n <= lines.len() && best.is_none_or(|x| i.abs_diff(near) < x.abs_diff(near)) {
                best = Some(i);
            }
        }
        if let Some(i) = best {
            let (s, e) = at(i);
            return Location::Moved { start: s, end: e };
        }
    }
    Location::Outdated
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const FILE: &str = "fn a() {\n    one();\n}\n\nfn load() {\n    let x = read()?;\n    x.parse()\n}\n\nfn z() {}\n";

    fn anchor(start: u32, end: u32) -> Anchor {
        lines_anchor("src/lib.rs", "new", "working", "abc123", "C:/w", "blob1", FILE, start, end).unwrap()
    }

    #[test]
    fn an_anchor_holds_the_lines_and_their_context() {
        let a = anchor(6, 7);
        assert_eq!(a.snippet, "    let x = read()?;\n    x.parse()");
        assert_eq!(a.before, vec!["", "fn load() {"]);
        assert_eq!(a.after, vec!["}", ""]);
        assert!(lines_anchor("p", "new", "s", "c", "w", "b", FILE, 0, 2).is_none());
        assert!(lines_anchor("p", "new", "s", "c", "w", "b", FILE, 3, 2).is_none());
        assert!(lines_anchor("p", "new", "s", "c", "w", "b", FILE, 5, 99).is_none());
        let long = "x".repeat(300) + "\n";
        let text = long.repeat(40);
        let big = lines_anchor("p", "new", "s", "c", "w", "b", &text, 1, 40).unwrap();
        assert!(big.clipped && big.snippet.len() <= SNIPPET_CAP);
    }

    #[test]
    fn the_same_blob_or_the_same_place_is_exact() {
        let a = anchor(6, 7);
        assert_eq!(locate(&a, "blob1", "anything"), Location::Exact { start: 6, end: 7 });
        assert_eq!(locate(&a, "other", FILE), Location::Exact { start: 6, end: 7 });
    }

    #[test]
    fn lines_inserted_above_move_the_thread() {
        let a = anchor(6, 7);
        let edited = format!("// new header\n// second\n{FILE}");
        assert_eq!(locate(&a, "b2", &edited), Location::Moved { start: 8, end: 9 });
    }

    #[test]
    fn a_duplicate_snippet_picks_the_nearest_occurrence() {
        let text = "dup();\nx\ndup();\ny\nz\nw\ndup();\n";
        let a = lines_anchor("p", "new", "s", "c", "w", "b", text, 3, 3).unwrap();
        // The file changed (a line removed at the top): the copies are at 1, 2 and 6 now.
        let now = "dup();\ndup();\ny\nz\nw\ndup();\n";
        assert_eq!(locate(&a, "other", now), Location::Moved { start: 2, end: 2 });
    }

    #[test]
    fn a_whitespace_only_change_still_finds_it() {
        let a = anchor(6, 7);
        let edited = FILE.replace("    let x = read()?;", "\tlet   x = read()?;");
        assert_eq!(locate(&a, "b2", &edited), Location::Moved { start: 6, end: 7 });
    }

    #[test]
    fn an_edit_inside_the_range_is_found_by_its_context() {
        let a = anchor(6, 7);
        let edited = FILE.replace("let x = read()?;", "let y = fetch()?;").replace("x.parse()", "y.parse().ok()");
        assert_eq!(locate(&a, "b2", &edited), Location::Moved { start: 6, end: 7 });
    }

    #[test]
    fn deleted_lines_are_outdated_and_the_snippet_stays_readable() {
        let a = anchor(6, 7);
        let gone = "fn a() {\n    one();\n}\n\nfn z() {}\n";
        assert_eq!(locate(&a, "b2", gone), Location::Outdated);
        assert!(a.snippet.contains("read()"));
        assert_eq!(locate(&a, "b2", ""), Location::Outdated);
    }

    #[test]
    fn crlf_text_matches_lf_anchors() {
        let a = anchor(6, 7);
        let crlf = FILE.replace('\n', "\r\n");
        assert_eq!(locate(&a, "b2", &crlf), Location::Exact { start: 6, end: 7 });
    }

    #[test]
    fn file_anchors_and_unverifiable_anchors() {
        let f = file_anchor("README.md", "file", "abc", "C:/w");
        assert_eq!(locate(&f, "", ""), Location::Exact { start: 0, end: 0 });
        let bare = Anchor { kind: "lines".into(), path: "a".into(), start: 3, end: 4, ..Anchor::default() };
        assert_eq!(locate(&bare, "x", "1\n2\n3\n4\n"), Location::Exact { start: 3, end: 4 });
        assert_eq!(locate(&bare, "x", "1\n2\n"), Location::Outdated);
    }

    #[test]
    fn hostile_anchors_never_panic() {
        let weird = Anchor { kind: "lines".into(), start: u32::MAX, end: 0, snippet: "\n\n\n".into(), before: vec!["x".into(); 50], after: vec![String::new(); 50], ..Anchor::default() };
        let _ = locate(&weird, "", FILE);
        let _ = locate(&weird, "", "");
        let huge = Anchor { kind: "lines".into(), start: 1, end: 1, snippet: "a\n".repeat(5000), ..Anchor::default() };
        let _ = locate(&huge, "", &"a\n".repeat(20_000));
    }
}

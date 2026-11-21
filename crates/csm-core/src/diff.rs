//! A unified-diff parser and the split-view alignment (docs/addons.md, "Code and diff"). Pure.
//!
//! The input is hostile (a plugin's `patch`, git's output on a hostile repo): nothing here
//! indexes by a value from the text, every count is capped, a malformed hunk is shown as far as
//! it goes, and a `\r` before a line break is dropped so a CRLF file reads the same as an LF one.
//! Lines are addressed as `(side, number)`: [`Side::Old`] is the text before, [`Side::New`] the
//! text after, numbers from 1.

use serde::Serialize;

/// Files in one patch.
pub const MAX_FILES: usize = 500;
/// Hunks in one file.
pub const MAX_HUNKS: usize = 5_000;
/// Lines in one patch (all files). Past this the rest is dropped and [`Patch::truncated`] is set.
pub const MAX_LINES: usize = 200_000;
/// One line, in characters. Longer lines are cut (the cut is shown by the renderer).
pub const MAX_LINE_CHARS: usize = 20_000;
/// A path, in characters.
const MAX_PATH_CHARS: usize = 1024;

/// Which text a line number belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Old,
    New,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Old => "old",
            Side::New => "new",
        }
    }

    pub fn parse(s: Option<&str>) -> Side {
        if s == Some("old") { Side::Old } else { Side::New }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
}

/// One line of a hunk, without its `+`, `-` or space and without a trailing `\r`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    /// 1-based number in the old text (`None` for an added line).
    pub old_no: Option<u32>,
    /// 1-based number in the new text (`None` for a deleted line).
    pub new_no: Option<u32>,
    /// Git said `\ No newline at end of file` after this line.
    pub no_eol: bool,
    /// The line was longer than [`MAX_LINE_CHARS`] and was cut.
    pub cut: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// The text after the second `@@` (git puts the enclosing function there).
    pub heading: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
}

impl FileStatus {
    pub fn letter(self) -> char {
        match self {
            FileStatus::Modified => 'M',
            FileStatus::Added => 'A',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
            FileStatus::Copied => 'C',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub status: FileStatus,
    /// Git said `Binary files ... differ` or `GIT binary patch`: there are no hunks to show.
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// The path to show: the new one, or the old one for a deletion.
    pub fn path(&self) -> &str {
        self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or("")
    }

    /// `(added, deleted)` line counts.
    pub fn counts(&self) -> (usize, usize) {
        let mut a = 0;
        let mut d = 0;
        for l in self.hunks.iter().flat_map(|h| h.lines.iter()) {
            match l.kind {
                LineKind::Add => a += 1,
                LineKind::Del => d += 1,
                LineKind::Context => {}
            }
        }
        (a, d)
    }
}

/// A parsed patch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Patch {
    pub files: Vec<FileDiff>,
    /// A cap was hit and the rest of the patch was dropped.
    pub truncated: bool,
}

/// Parses a unified diff: git's output (`diff --git`, renames, binary files, `\ No newline`) or a
/// plain `---`/`+++` diff. Text that is not a diff at all gives no files.
pub fn parse_unified(patch: &str) -> Patch {
    let mut p = Parser { out: Patch::default(), cur: None, hunk: None, rem: None, next: (0, 0), lines: 0, binary_data: false };
    // A patch ends with a line break: the empty piece after it is not a line.
    let patch = patch.strip_suffix('\n').unwrap_or(patch);
    for raw in patch.split('\n') {
        if !p.line(raw.strip_suffix('\r').unwrap_or(raw)) {
            break;
        }
    }
    p.finish()
}

struct Parser {
    out: Patch,
    cur: Option<FileDiff>,
    hunk: Option<Hunk>,
    /// Lines the current hunk's header still promises: (old, new). `None`: the header was
    /// malformed, so the hunk runs until the next `@@` or `diff --git`.
    rem: Option<(u32, u32)>,
    /// The next line numbers (old, new) in the current hunk.
    next: (u32, u32),
    lines: usize,
    /// Inside a `GIT binary patch` body.
    binary_data: bool,
}

impl Parser {
    fn flush_hunk(&mut self) {
        if let (Some(h), Some(f)) = (self.hunk.take(), self.cur.as_mut())
            && f.hunks.len() < MAX_HUNKS
        {
            f.hunks.push(h);
        }
        self.rem = None;
    }

    fn flush_file(&mut self) {
        self.flush_hunk();
        if let Some(f) = self.cur.take() {
            if self.out.files.len() < MAX_FILES {
                self.out.files.push(f);
            } else {
                self.out.truncated = true;
            }
        }
    }

    fn new_file(&mut self) {
        self.flush_file();
        self.binary_data = false;
        self.cur = Some(FileDiff { old_path: None, new_path: None, status: FileStatus::Modified, binary: false, hunks: Vec::new() });
    }

    fn finish(mut self) -> Patch {
        self.flush_file();
        self.out
    }

    /// Returns false when the parse should stop (the line cap).
    fn line(&mut self, l: &str) -> bool {
        // Inside a hunk whose header promised more lines, a line is hunk content whatever it
        // looks like (`--- x` can be a deleted line that starts with `-- `).
        if self.hunk.is_some() && self.in_hunk_body(l) {
            return self.hunk_line(l);
        }
        if let Some(rest) = l.strip_prefix("diff --git ") {
            self.new_file();
            if let Some(f) = self.cur.as_mut() {
                let (a, b) = split_git_header(rest);
                f.old_path = a;
                f.new_path = b;
            }
            return true;
        }
        if l.starts_with("@@") {
            self.flush_hunk();
            if self.cur.is_none() {
                self.new_file();
            }
            let (h, rem) = parse_hunk_header(l);
            self.next = (h.old_start, h.new_start);
            self.hunk = Some(h);
            self.rem = rem;
            return true;
        }
        if self.hunk.is_some() && self.rem.is_none() {
            // A malformed header: the hunk runs until the next header.
            return self.hunk_line(l);
        }
        if l.starts_with('\\') {
            // `\ No newline at end of file`, after the hunk's counts ran out.
            if let Some(last) = self.hunk.as_mut().and_then(|h| h.lines.last_mut()) {
                last.no_eol = true;
            }
            return true;
        }
        self.flush_hunk();
        if self.binary_data {
            return true;
        }
        if let Some(rest) = l.strip_prefix("--- ") {
            // A plain unified diff has no `diff --git` line: `---` starts the file.
            if self.cur.as_ref().is_none_or(|f| !f.hunks.is_empty()) {
                self.new_file();
            }
            if let Some(f) = self.cur.as_mut() {
                match header_path(rest) {
                    Some(p) => f.old_path = Some(p),
                    None => f.status = FileStatus::Added,
                }
            }
            return true;
        }
        if let Some(rest) = l.strip_prefix("+++ ") {
            if let Some(f) = self.cur.as_mut() {
                match header_path(rest) {
                    Some(p) => f.new_path = Some(p),
                    None => f.status = FileStatus::Deleted,
                }
            }
            return true;
        }
        if let Some(f) = self.cur.as_mut() {
            if l.starts_with("new file mode") {
                f.status = FileStatus::Added;
            } else if l.starts_with("deleted file mode") {
                f.status = FileStatus::Deleted;
            } else if let Some(p) = l.strip_prefix("rename from ") {
                f.status = FileStatus::Renamed;
                f.old_path = Some(unquote(p));
            } else if let Some(p) = l.strip_prefix("rename to ") {
                f.status = FileStatus::Renamed;
                f.new_path = Some(unquote(p));
            } else if let Some(p) = l.strip_prefix("copy from ") {
                f.status = FileStatus::Copied;
                f.old_path = Some(unquote(p));
            } else if let Some(p) = l.strip_prefix("copy to ") {
                f.status = FileStatus::Copied;
                f.new_path = Some(unquote(p));
            } else if l.starts_with("Binary files ") || l.starts_with("GIT binary patch") {
                f.binary = true;
                self.binary_data = l.starts_with("GIT binary patch");
            }
        }
        true
    }

    fn in_hunk_body(&self, l: &str) -> bool {
        match self.rem {
            Some((o, n)) if o == 0 && n == 0 => l.starts_with('\\'),
            // Body lines start with ` `, `+`, `-` or `\`, or are empty (a stripped context line).
            Some(_) => l.is_empty() || l.starts_with([' ', '+', '-', '\\']),
            None => false,
        }
    }

    fn hunk_line(&mut self, l: &str) -> bool {
        if self.lines >= MAX_LINES {
            self.out.truncated = true;
            return false;
        }
        let (kind, body) = match l.as_bytes().first() {
            Some(b'+') => (LineKind::Add, l.get(1..).unwrap_or("")),
            Some(b'-') => (LineKind::Del, l.get(1..).unwrap_or("")),
            Some(b' ') => (LineKind::Context, l.get(1..).unwrap_or("")),
            // A blank line inside a hunk is an empty context line (some tools strip the space).
            None => (LineKind::Context, ""),
            Some(b'\\') => {
                if let Some(last) = self.hunk.as_mut().and_then(|h| h.lines.last_mut()) {
                    last.no_eol = true;
                }
                return true;
            }
            // Not a hunk line: the hunk ended early (a truncated or hand-edited patch).
            _ => {
                self.flush_hunk();
                return true;
            }
        };
        let Some(h) = self.hunk.as_mut() else { return true };
        let (cut, text) = cut_line(body);
        let (on, nn) = self.next;
        let line = match kind {
            LineKind::Context => DiffLine { kind, text, old_no: Some(on), new_no: Some(nn), no_eol: false, cut },
            LineKind::Del => DiffLine { kind, text, old_no: Some(on), new_no: None, no_eol: false, cut },
            LineKind::Add => DiffLine { kind, text, old_no: None, new_no: Some(nn), no_eol: false, cut },
        };
        self.next = match kind {
            LineKind::Context => (on.saturating_add(1), nn.saturating_add(1)),
            LineKind::Del => (on.saturating_add(1), nn),
            LineKind::Add => (on, nn.saturating_add(1)),
        };
        h.lines.push(line);
        self.lines += 1;
        if let Some((o, n)) = self.rem.as_mut() {
            match kind {
                LineKind::Context => {
                    *o = o.saturating_sub(1);
                    *n = n.saturating_sub(1);
                }
                LineKind::Del => *o = o.saturating_sub(1),
                LineKind::Add => *n = n.saturating_sub(1),
            }
        }
        true
    }
}

fn cut_line(s: &str) -> (bool, String) {
    match s.char_indices().nth(MAX_LINE_CHARS) {
        Some((i, _)) => (true, s.get(..i).unwrap_or("").to_owned()),
        None => (false, s.to_owned()),
    }
}

/// `@@ -a,b +c,d @@ heading`. A header that does not parse gives start 0 and no promise of lengths.
fn parse_hunk_header(l: &str) -> (Hunk, Option<(u32, u32)>) {
    let bad = || (Hunk { old_start: 0, old_len: 0, new_start: 0, new_len: 0, heading: cut_line(l).1, lines: Vec::new() }, None);
    let Some(rest) = l.strip_prefix("@@ -") else { return bad() };
    let Some((ranges, heading)) = rest.split_once(" @@") else { return bad() };
    let Some((old, new)) = ranges.split_once(" +") else { return bad() };
    let (Some((os, ol)), Some((ns, nl))) = (range(old), range(new)) else { return bad() };
    (Hunk { old_start: os, old_len: ol, new_start: ns, new_len: nl, heading: cut_line(heading.trim()).1, lines: Vec::new() }, Some((ol, nl)))
}

fn range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// A path from `--- a/x` or `+++ b/x`: the `a/` or `b/` prefix and a trailing tab with a
/// timestamp are dropped; `/dev/null` is `None`.
fn header_path(s: &str) -> Option<String> {
    let s = s.split('\t').next().unwrap_or(s);
    let p = unquote(s);
    if p == "/dev/null" {
        return None;
    }
    let p = p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(&p).to_owned();
    Some(p).filter(|p| !p.is_empty())
}

/// The two paths of a `diff --git a/x b/y` line (best effort: only used until `---`/`+++` or the
/// rename lines say better).
fn split_git_header(rest: &str) -> (Option<String>, Option<String>) {
    let strip = |s: &str| unquote(s).trim_start_matches("a/").to_owned();
    if let Some(stripped) = rest.strip_prefix('"') {
        // A quoted first path.
        if let Some(end) = stripped.find("\" ") {
            let a = unquote(&rest[..end + 2]);
            let b = unquote(&rest[end + 3..]);
            return (Some(strip_ab(&a)), Some(strip_ab(&b)));
        }
    }
    if let Some(i) = rest.find(" b/") {
        let a = &rest[..i];
        let b = &rest[i + 1..];
        return (Some(strip(a)), Some(strip_ab(&unquote(b))));
    }
    (None, None)
}

fn strip_ab(p: &str) -> String {
    p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")).unwrap_or(p).to_owned()
}

/// Undoes git's C-style quoting of a path (`"a\tb\303\251"`). An unquoted string is returned as is.
fn unquote(s: &str) -> String {
    let s = s.trim_end();
    let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) else {
        return s.chars().take(MAX_PATH_CHARS).collect();
    };
    let mut bytes: Vec<u8> = Vec::new();
    let b = inner.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            bytes.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        match b.get(i) {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'"') => bytes.push(b'"'),
            Some(b'\\') => bytes.push(b'\\'),
            Some(d) if (b'0'..=b'7').contains(d) => {
                let mut v: u32 = 0;
                let mut k = 0;
                while k < 3 && b.get(i).is_some_and(|c| (b'0'..=b'7').contains(c)) {
                    v = v * 8 + u32::from(b[i] - b'0');
                    i += 1;
                    k += 1;
                }
                bytes.push((v & 0xff) as u8);
                continue;
            }
            Some(&c) => bytes.push(c),
            None => {}
        }
        i += 1;
    }
    String::from_utf8_lossy(&bytes).chars().take(MAX_PATH_CHARS).collect()
}

// ---------------------------------------------------------------------------------------------
// Split view
// ---------------------------------------------------------------------------------------------

/// One side of a split row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitCell<'a> {
    pub no: u32,
    pub line: &'a DiffLine,
}

/// One row of the side-by-side view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitRow<'a> {
    pub old: Option<SplitCell<'a>>,
    pub new: Option<SplitCell<'a>>,
}

/// Pairs the deletions and additions of each change block (the first deletion with the first
/// addition, and so on); context lines sit on both sides. The extra lines of a longer block
/// have an empty cell opposite.
pub fn align_split(hunk: &Hunk) -> Vec<SplitRow<'_>> {
    let mut rows = Vec::with_capacity(hunk.lines.len());
    let mut dels: Vec<&DiffLine> = Vec::new();
    let mut adds: Vec<&DiffLine> = Vec::new();
    fn flush<'a>(rows: &mut Vec<SplitRow<'a>>, dels: &mut Vec<&'a DiffLine>, adds: &mut Vec<&'a DiffLine>) {
        let n = dels.len().max(adds.len());
        for i in 0..n {
            let old = dels.get(i).and_then(|l| l.old_no.map(|no| SplitCell { no, line: l }));
            let new = adds.get(i).and_then(|l| l.new_no.map(|no| SplitCell { no, line: l }));
            rows.push(SplitRow { old, new });
        }
        dels.clear();
        adds.clear();
    }
    for l in &hunk.lines {
        match l.kind {
            LineKind::Del => {
                // A deletion after an addition starts a new block.
                if !adds.is_empty() {
                    flush(&mut rows, &mut dels, &mut adds);
                }
                dels.push(l);
            }
            LineKind::Add => adds.push(l),
            LineKind::Context => {
                flush(&mut rows, &mut dels, &mut adds);
                if let (Some(o), Some(n)) = (l.old_no, l.new_no) {
                    rows.push(SplitRow { old: Some(SplitCell { no: o, line: l }), new: Some(SplitCell { no: n, line: l }) });
                }
            }
        }
    }
    flush(&mut rows, &mut dels, &mut adds);
    rows
}

#[cfg(test)]
mod tests;

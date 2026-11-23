//! Smart copy of a selection in a Claude Code pane: the text as Claude meant it, not as the
//! terminal shows it.
//!
//! Claude Code (Ink) wraps its output itself: a long line becomes several screen rows joined by
//! real newlines, each continuation indented to the message's column, and every message carries
//! markers (`⏺ `, `⎿ `, `> `) and frames (`╭─╮ │ │ ╰─╯`, `────`). Pasted as it is, a long command
//! breaks into several (PowerShell answers with `>>`). [`clean`] undoes that, as a heuristic:
//!
//! - **Markers and frames.** A leading `⏺`/`●`, `⎿` or `> ` becomes indentation; the input
//!   box's frame lines (`╭…╮`, `╰…╯`, rules of `─`) are dropped and its side bars (`│ … │`)
//!   stripped. Then the selection's common indentation goes.
//! - **Hard wraps.** A row is joined to the next when the next one's first word would not have
//!   fitted after it (`end + 1 + word > edge - SLACK`), the row is at least half the pane wide,
//!   and the next row is a continuation: not empty, indented like the block or the row's hanging
//!   indent, not a new list item, heading, table row or code fence (in a fenced code block a
//!   line starting with `-` may still be a continuation). The rows are joined with a space,
//!   except where a word was broken at the very last column (no space).
//! - **Tables** (rows with `┌┬┐├┼┤└┴┘`, and the `│` rows between them) are kept as drawn.
//!
//! Known limits: the joins are guesses from widths, so a genuine line break whose next word
//! would not have fitted either is joined too; a word broken at a right margin narrower than
//! the pane (Claude's own padding) gets a space; and a row that was soft-wrapped by the
//! terminal after a resize is measured as one long row. The raw selection is one Alt away.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Columns short of the edge that still count as "reached the edge" (Claude's right margin).
pub const SLACK: usize = 4;

/// Characters that make a row part of a drawn table.
const TABLE: [char; 9] = ['┌', '┬', '┐', '├', '┼', '┤', '└', '┴', '┘'];

/// One screen row of the selection.
#[derive(Debug, Clone)]
struct Row {
    /// Indentation (columns) after markers became spaces.
    indent: usize,
    /// The text after the indentation, trailing blanks removed.
    text: String,
    /// The screen column after its last character.
    end: usize,
    /// The column wrapped rows end at: the pane's width, or a frame's inner edge.
    edge: usize,
    table: bool,
    /// The selection starts inside it: what precedes it on the screen is not in the text.
    partial: bool,
}

impl Row {
    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn fence(&self) -> bool {
        self.text.starts_with("```") || self.text.starts_with("~~~")
    }
}

/// The width of a list marker the row starts with (`- `, `* `, `• `, `+ `, `12. `, `3) `).
fn list_marker(text: &str) -> Option<usize> {
    let mut chars = text.chars();
    match chars.next()? {
        '-' | '*' | '•' | '+' | '◦' | '▪' => (chars.next()? == ' ').then_some(2),
        c if c.is_ascii_digit() => {
            let digits = text.chars().take_while(char::is_ascii_digit).count();
            let rest = text.get(digits..)?;
            (rest.starts_with(". ") || rest.starts_with(") ")).then_some(digits + 2)
        }
        _ => None,
    }
}

fn heading(text: &str) -> bool {
    let hashes = text.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes) && text.chars().nth(hashes) == Some(' ')
}

fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// A row reduced to indentation and text: markers become spaces, frames go. `None` for a frame
/// line, which is dropped.
fn row_of(line: &str, start: usize, cols: usize, table_text: bool) -> Option<Row> {
    let line = line.trim_end();
    let lead: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let rest = line.get(lead.len()..).unwrap_or_default();
    let mut indent = start + width(&lead);
    let is_table = table_text && (rest.contains(TABLE) || rest.starts_with('│'));
    if is_table {
        let end = indent + width(rest);
        return Some(Row { indent, text: rest.to_owned(), end, edge: cols, table: true, partial: false });
    }
    // The input box's top and bottom, and the bare rules around Claude Code 2's prompt.
    if rest.starts_with(['╭', '╰']) || (!rest.is_empty() && rest.chars().all(|c| matches!(c, '─' | '━' | '═' | ' '))) {
        return None;
    }
    let mut edge = cols;
    let mut body = rest.to_owned();
    // `│ text │`: the frame's side bars; its right bar is the edge rows wrap at.
    if let Some(inner) = body.strip_prefix('│') {
        let inner = inner.to_owned();
        indent += 1;
        let (inner, closing) = match inner.trim_end().strip_suffix('│') {
            Some(i) => (i.to_owned(), true),
            None => (inner, false),
        };
        if closing {
            // The bar's column, less the space padding inside it.
            edge = (indent + width(&inner)).saturating_sub(1).max(1);
        }
        body = inner;
    }
    // Markers at the start become indentation (they may follow each other: `│ > `).
    loop {
        let lead: String = body.chars().take_while(|c| *c == ' ').collect();
        indent += lead.len();
        body = body.get(lead.len()..).unwrap_or_default().to_owned();
        let mut chars = body.chars();
        let Some(first) = chars.next() else { break };
        let marker = matches!(first, '⏺' | '●' | '⎿') || (first == '>' && matches!(chars.next(), Some(' ') | None));
        if !marker {
            break;
        }
        indent += first.width().unwrap_or(1);
        body = body.get(first.len_utf8()..).unwrap_or_default().to_owned();
    }
    let text = body.trim_end().to_owned();
    let end = indent + width(&text);
    Some(Row { indent, text, end, edge, table: false, partial: false })
}

/// The selection `text` as Claude meant it ([module docs](self)). `cols` is the pane's width in
/// columns; `first_col` the column the selection starts at (its first row may be partial).
pub fn clean(text: &str, cols: usize, first_col: usize) -> String {
    let text = text.replace("\r\n", "\n");
    let has_table = text.contains(TABLE);
    let cols = cols.max(1);
    let rows: Vec<Row> = text
        .split('\n')
        .enumerate()
        .filter_map(|(i, l)| {
            let mut r = row_of(l, if i == 0 { first_col } else { 0 }, cols, has_table)?;
            r.partial = i == 0 && first_col > 0;
            Some(r)
        })
        .collect();
    // The common indentation: of every non-empty row, but a partial first row says nothing.
    let base = rows
        .iter()
        .filter(|r| !r.is_empty() && !r.partial)
        .map(|r| r.indent)
        .min()
        .unwrap_or_else(|| rows.first().map_or(0, |r| r.indent));

    let mut out: Vec<String> = Vec::new();
    let mut in_code = false;
    // The logical line being built: its text, the last row's metrics, and its hanging indent.
    let mut cur: Option<(String, Row, usize)> = None;
    for row in rows {
        if let Some((line, last, hang)) = cur.take() {
            match join(&last, hang, base, &row, in_code) {
                Some(sep) => {
                    // The row's indentation (and an exact fit's moved space) is not text.
                    let glue = if sep == Sep::None { "" } else { " " };
                    cur = Some((format!("{line}{glue}{}", row.text), row, hang));
                    continue;
                }
                None => out.push(line),
            }
        }
        if row.fence() {
            in_code = !in_code;
        }
        let pad = if row.partial { 0 } else { row.indent.saturating_sub(base) };
        let line = format!("{}{}", " ".repeat(pad), row.text);
        let hang = row.indent + if in_code || row.table { 0 } else { list_marker(&row.text).unwrap_or(0) };
        cur = Some((line, row, hang));
    }
    if let Some((line, _, _)) = cur {
        out.push(line);
    }
    // No blank lines around it, nothing trailing on any line.
    let out: Vec<String> = out.into_iter().map(|l| l.trim_end().to_owned()).collect();
    let first = out.iter().position(|l| !l.is_empty()).unwrap_or(out.len());
    let last = out.iter().rposition(|l| !l.is_empty()).map_or(first, |i| i + 1);
    out.get(first..last).unwrap_or_default().join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    Space,
    None,
}

/// Whether `next` continues `last` (a row Claude wrapped), and how they join.
fn join(last: &Row, hang: usize, base: usize, next: &Row, in_code: bool) -> Option<Sep> {
    if last.is_empty() || next.is_empty() || last.table || next.table || last.fence() || next.fence() {
        return None;
    }
    if !in_code && (list_marker(&next.text).is_some() || heading(&next.text)) {
        return None;
    }
    // A continuation sits at the block's column or the row's hanging indent; one more column when
    // the row before filled the line exactly (the space moved to the new row).
    if next.indent < base || next.indent > hang.max(base) + 1 {
        return None;
    }
    let word = width(next.text.split(' ').next().unwrap_or_default());
    let edge = last.edge;
    let full = last.end + 1 + word > edge.saturating_sub(SLACK) && last.end * 2 >= edge;
    if !full {
        return None;
    }
    // A word broken at the last column continues without a space.
    let exact = next.indent == hang.max(base) + 1 || next.indent == base + 1;
    if last.end >= edge && !exact { Some(Sep::None) } else { Some(Sep::Space) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ink's wrapping (wrap-ansi with `hard: true, trim: false`, as Claude Code uses it): words
    /// move to the next row whole, the space before a moved word stays at the end of the row, a
    /// word longer than the row is broken at the last column, and a row that is exactly full
    /// starts the next with the space.
    fn ink_wrap(text: &str, columns: usize) -> Vec<String> {
        let mut rows = vec![String::new()];
        for (index, word) in text.split(' ').enumerate() {
            let mut row_len = width(rows.last().unwrap());
            if index != 0 {
                if row_len >= columns {
                    rows.push(String::new());
                    row_len = 0;
                }
                rows.last_mut().unwrap().push(' ');
                row_len += 1;
            }
            let len = width(word);
            if len > columns {
                let remaining = columns.saturating_sub(row_len);
                let this_line = 1 + (len - remaining.min(len)).saturating_sub(1) / columns;
                let next_line = (len - 1) / columns;
                if next_line < this_line {
                    rows.push(String::new());
                }
                let mut visible = width(rows.last().unwrap());
                let chars: Vec<char> = word.chars().collect();
                for (i, c) in chars.iter().enumerate() {
                    rows.last_mut().unwrap().push(*c);
                    visible += 1;
                    if visible >= columns && i < chars.len() - 1 {
                        rows.push(String::new());
                        visible = 0;
                    }
                }
                continue;
            }
            if row_len + len > columns && row_len > 0 && len > 0 {
                rows.push(String::new());
            }
            rows.last_mut().unwrap().push_str(word);
        }
        rows
    }

    /// A Claude message as the screen shows it, as a selection of it copies (trailing blanks
    /// trimmed, as the terminal's copy does): `⏺ ` on the first row, every row indented 2 more
    /// than `indent`, wrapped at `cols - right_margin`. Each source line is wrapped on its own.
    fn screen(lines: &[&str], cols: usize, right_margin: usize) -> String {
        let width = cols - 2 - right_margin;
        let mut out = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            for row in ink_wrap(l, width) {
                let lead = if i == 0 && out.is_empty() { "⏺ " } else { "  " };
                out.push(format!("{lead}{row}").trim_end().to_owned());
            }
        }
        out.join("\n")
    }

    const PS: &str = "Get-ChildItem -Path C:\\work -Recurse -File | Where-Object { $_.Length -gt 1MB } | Sort-Object Length -Descending | Select-Object -First 20 FullName, Length";

    #[test]
    fn the_long_powershell_command_comes_back_as_one_line() {
        for cols in [40, 57, 60, 80, 81, 100, 120] {
            let s = screen(&["Run this in PowerShell:", "", PS, "", "It lists the biggest files."], cols, 0);
            assert!(s.lines().count() > 5, "wrapped at {cols}: {s}");
            let got = clean(&s, cols, 0);
            assert_eq!(got, format!("Run this in PowerShell:\n\n{PS}\n\nIt lists the biggest files."), "at {cols} cols:\n{s}");
        }
    }

    #[test]
    fn a_right_margin_still_joins_word_wraps() {
        for cols in [60, 80, 100] {
            let s = screen(&[PS], cols, 2);
            assert_eq!(clean(&s, cols, 0), PS, "at {cols} cols:\n{s}");
        }
    }

    #[test]
    fn the_captured_powershell_screen() {
        let s = include_str!("claude_screens/powershell_80.txt");
        assert_eq!(
            clean(s, 80, 0),
            "Here is the command:\n\n\
             Get-ChildItem -Path C:\\work -Recurse -File | Where-Object { $_.Length -gt 1MB } | Sort-Object Length -Descending | Select-Object -First 20 FullName, Length\n\n\
             Run it from the repository root."
        );
    }

    #[test]
    fn a_word_longer_than_the_row_is_rejoined_without_a_space() {
        let url = format!("https://example.com/{}", "a".repeat(150));
        let s = screen(&["See", &url], 80, 0);
        assert!(s.lines().count() >= 3, "{s}");
        assert_eq!(clean(&s, 80, 0), format!("See\n{url}"));
    }

    #[test]
    fn genuine_short_lines_in_a_code_block_stay() {
        let s = include_str!("claude_screens/code_short_80.txt");
        assert_eq!(clean(s, 80, 0), "Set the variables first:\n\n$root = 'C:\\work'\n$min = 1MB\nGet-ChildItem $root -Recurse\n\nThen run it.");
    }

    #[test]
    fn fenced_code_keeps_its_lines_and_joins_an_over_long_one() {
        let s = include_str!("claude_screens/fenced_80.txt");
        assert_eq!(
            clean(s, 80, 0),
            "```powershell\n$x = 1\n\
             Get-ChildItem -Path C:\\work -Recurse -File | Where-Object { $_.Length -gt 1MB } | Sort-Object Length -Descending\n\
             -First\n```"
        );
    }

    #[test]
    fn a_bullet_list_keeps_its_items_and_joins_their_wraps() {
        let s = include_str!("claude_screens/bullets_80.txt");
        assert_eq!(
            clean(s, 80, 0),
            "The folder has three projects:\n\n\
             - shop: the web shop, with its own build and a long description that wraps onto a second row\n\
             - blog: static pages\n\
             - tools: scripts that run nightly, also described at some length so that it wraps on the screen\n\
             \x20 - a nested item\n\
             1. first numbered step, which is also long enough to wrap around the edge of the pane\n\
             2. second"
        );
    }

    #[test]
    fn a_table_is_kept_as_drawn() {
        let s = include_str!("claude_screens/table_80.txt");
        assert_eq!(
            clean(s, 80, 0),
            "The projects:\n\n┌─────────┬─────────────┐\n│ Project │ What        │\n├─────────┼─────────────┤\n│ shop    │ web shop    │\n│ blog    │ static site │\n└─────────┴─────────────┘"
        );
    }

    #[test]
    fn the_box_drawn_prompt_input_gives_its_text() {
        let s = include_str!("claude_screens/input_box_80.txt");
        assert_eq!(clean(s, 80, 0), "please write a PowerShell command that lists the biggest files under C:\\work and sorts them");
    }

    #[test]
    fn claude_code_2_prompt_rules_and_history_markers_go() {
        let s = "────────────────────────────────────────\n> fix the build\n────────────────────────────────────────";
        assert_eq!(clean(s, 40, 0), "fix the build");
        let s = "⏺ Bash(cargo build)\n  ⎿  Compiling csm-core v2.0.0\n     Finished dev profile";
        assert_eq!(clean(s, 80, 0), "Bash(cargo build)\n   Compiling csm-core v2.0.0\n   Finished dev profile");
    }

    #[test]
    fn a_partial_first_row_is_measured_from_where_the_selection_starts() {
        let s = screen(&[PS], 80, 0);
        // Selected from the middle of the first row (after `⏺ Get-ChildItem `).
        let from = "⏺ Get-ChildItem ".chars().count();
        let partial: String = s.chars().skip(from).collect();
        assert_eq!(clean(&partial, 80, from), PS.strip_prefix("Get-ChildItem ").unwrap());
    }

    #[test]
    fn plain_text_and_hostile_input_never_panic() {
        assert_eq!(clean("", 80, 0), "");
        assert_eq!(clean("hello", 0, 0), "hello");
        assert_eq!(clean("hello", 80, 500), "hello");
        assert_eq!(clean("a\r\nb", 80, 0), "a\nb");
        let wide = "漢字".repeat(50);
        let _ = clean(&format!("⏺ {wide}\n  {wide}\n│\n│ │\n╭\n> \n⎿"), 7, 3);
        let _ = clean("\u{0}\u{1b}[31m│┌┐", 3, 0);
    }

    #[test]
    fn short_prose_lines_are_not_joined() {
        let s = "⏺ Done.\n  Two files changed.\n  Nothing else to do.";
        assert_eq!(clean(s, 80, 0), "Done.\nTwo files changed.\nNothing else to do.");
    }
}

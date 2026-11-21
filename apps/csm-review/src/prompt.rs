//! The "Send to agent" text. One formatter, shared by the screen's button and the `review.prompt`
//! tool, so an agent and the person see the same words. It is short on purpose: it names the tools
//! and lists one line per thread; it never inlines a whole thread.
//!
//! The text is pasted into a terminal, so it is plain: no control characters, and each comment is
//! cut to one line. (CSM pastes it with bracketed paste after the person confirms it; this module
//! only builds it.)

use crate::store::{Status, Thread};

/// The prompt stays well under a screen button's 8 KiB parameter cap.
pub const MAX_PROMPT: usize = 5000;
const MAX_LINES: usize = 25;
const SNIP: usize = 120;

/// One thread as the prompt lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub id: String,
    pub path: String,
    /// The current lines (re-anchored), or `None` for a whole-file or an outdated thread.
    pub lines: Option<(u32, u32)>,
    pub outdated: bool,
    pub whole_file: bool,
    pub first: String,
}

impl Line {
    #[must_use]
    pub fn of(t: &Thread, lines: Option<(u32, u32)>, outdated: bool) -> Line {
        Line {
            id: t.id.clone(),
            path: t.anchor.path.clone(),
            lines,
            outdated,
            whole_file: t.anchor.kind == "file",
            first: t.comments.first().map(|c| c.body.clone()).unwrap_or_default(),
        }
    }
}

/// A comment cut to one printable line.
#[must_use]
pub fn one_line(s: &str, cap: usize) -> String {
    let flat: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= cap {
        return flat;
    }
    let mut out: String = flat.chars().take(cap.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn place(l: &Line) -> String {
    let path: String = l.path.chars().filter(|c| !c.is_control()).collect();
    if l.whole_file {
        return format!("{path} (file)");
    }
    match (l.lines, l.outdated) {
        (Some((s, e)), _) if s == e => format!("{path}:{s}"),
        (Some((s, e)), _) => format!("{path}:{s}-{e}"),
        (None, true) => format!("{path} (outdated)"),
        (None, false) => path,
    }
}

/// The text. `what` is how the threads are described ("open", "addressed"...). `repo` names the
/// repository for the person reading it in the terminal.
#[must_use]
pub fn build(repo: &str, what: &str, lines: &[Line]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let repo: String = repo.chars().filter(|c| !c.is_control()).take(80).collect();
    let mut out = format!("Review comments are waiting in CodeShellManager for {}: {} {what}.\n", if repo.is_empty() { "this repo" } else { &repo }, lines.len());
    for (i, l) in lines.iter().take(MAX_LINES).enumerate() {
        let entry = format!("{}. {} [{}] {}\n", i + 1, place(l), l.id, one_line(&l.first, SNIP));
        if out.len() + entry.len() > MAX_PROMPT - 400 {
            break;
        }
        out.push_str(&entry);
    }
    let listed = out.lines().skip(1).count();
    if lines.len() > listed {
        out.push_str(&format!("... and {} more (review.list shows all of them).\n", lines.len() - listed));
    }
    out.push_str("Read each with review.get, fix it, then answer with review.reply (status \"addressed\").\n");
    out.push_str("If you do not see those tools, call toolset_enable with \"addons\" first.");
    out
}

/// What the screen's prompt covers: open threads, or addressed ones too when none are open.
#[must_use]
pub fn wanted(t: &Thread) -> bool {
    t.status == Status::Open
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn line(id: &str, path: &str, lines: Option<(u32, u32)>, first: &str) -> Line {
        Line { id: id.into(), path: path.into(), lines, outdated: false, whole_file: false, first: first.into() }
    }

    #[test]
    fn it_names_the_tools_and_one_line_per_thread() {
        let mut file = line("t_3", "README.md", None, "Mention --clean.");
        file.whole_file = true;
        let text = build("shop", "open", &[line("t_1", "src/lib.rs", Some((120, 134)), "This swallows\nthe error; return it."), line("t_2", "src/ui.rs", Some((88, 88)), "Name this."), file]);
        assert!(text.starts_with("Review comments are waiting in CodeShellManager for shop: 3 open.\n"), "{text}");
        assert!(text.contains("1. src/lib.rs:120-134 [t_1] This swallows the error; return it."));
        assert!(text.contains("2. src/ui.rs:88 [t_2] Name this."));
        assert!(text.contains("3. README.md (file) [t_3] Mention --clean."));
        assert!(text.contains("review.get") && text.contains("review.reply") && text.contains("toolset_enable"));
        assert_eq!(build("x", "open", &[]), "");
    }

    #[test]
    fn it_stays_short_and_plain_whatever_the_comments_say() {
        let many: Vec<Line> = (0..300).map(|i| line(&format!("t_{i}"), "a/very/long/path/to/some/file.rs", Some((1, 2)), &"word ".repeat(400))).collect();
        let text = build("repo\u{1b}[31m", "open", &many);
        assert!(text.len() <= MAX_PROMPT, "{}", text.len());
        assert!(text.contains("more (review.list shows all of them)"));
        assert!(!text.chars().any(|c| c.is_control() && c != '\n'), "no escape sequences reach the terminal");
        assert_eq!(one_line("a\u{7}b\n\tc", 10), "a b c");
        assert_eq!(one_line(&"é".repeat(300), 5).chars().count(), 5);
    }
}

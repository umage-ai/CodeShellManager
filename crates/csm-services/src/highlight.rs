//! Syntax highlighting for the add-on `code` and `diff` widgets (docs/addons.md, "Code and
//! diff"). A plugin sends text and a language (or a path); this turns each line into spans of a
//! small set of [`TokenClass`]es, and the UI maps the classes to theme tokens. No colours here.
//!
//! The engine is `syntect` with its pure-Rust regex backend (`regex-fancy`: no Oniguruma, no C
//! build), with the grammars of `two-face` (bat's set: TypeScript, TOML, Dockerfile and the
//! rest). Loading the grammars takes a moment, so the set loads on first use, on whichever
//! thread asks: **call [`highlight`] from a worker, never from a frame**.
//!
//! The input is hostile. Past [`MAX_BYTES`] or [`MAX_LINES`] nothing is highlighted
//! ([`Outcome::TooLarge`]); a line over [`MAX_LINE_BYTES`] is left plain; the whole run has a time
//! budget and stops with what it has ([`Highlighted::partial`]); a parse error ends the run the
//! same way. Every offset is clamped to the line.

use std::sync::OnceLock;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// Text over this is not highlighted ("Large file: highlighting off").
pub const MAX_BYTES: usize = 5 * 1024 * 1024;
/// More lines than this are not highlighted.
pub const MAX_LINES: usize = 100_000;
/// A line longer than this is left plain (the grammar's regexes can be slow on it).
pub const MAX_LINE_BYTES: usize = 5_000;
/// The longest a run may take; the rest of the text stays plain.
pub const TIME_BUDGET: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenClass {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
    Type,
    Function,
    Constant,
    Macro,
    Operator,
    Punctuation,
    Tag,
    Attribute,
}

/// A run of one class in a line: byte offsets, `start..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub class: TokenClass,
}

/// The spans of every line (`lines[i]` is line `i`, without its line break). Plain text has no
/// span; spans are sorted and do not overlap.
#[derive(Debug, Clone)]
pub struct Highlighted {
    pub lines: Vec<Vec<Span>>,
    /// The run stopped early (time budget or a grammar error); later lines have no spans.
    pub partial: bool,
    /// The thread that did the work (tests assert it is not the UI's).
    pub thread: ThreadId,
}

impl Highlighted {
    /// A rough size, for a cache's budget.
    pub fn bytes(&self) -> usize {
        self.lines.len() * 24 + self.lines.iter().map(|l| l.len() * std::mem::size_of::<Span>()).sum::<usize>()
    }

    /// The spans of line `i`.
    pub fn line(&self, i: usize) -> &[Span] {
        self.lines.get(i).map_or(&[], Vec::as_slice)
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Highlighted(Highlighted),
    /// No grammar for this language or path: the text stays plain.
    NoSyntax,
    /// Over [`MAX_BYTES`] or [`MAX_LINES`].
    TooLarge,
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn find<'a>(set: &'a SyntaxSet, language: &str, path: &str) -> Option<&'a SyntaxReference> {
    let language = language.trim();
    if !language.is_empty()
        && let Some(s) = set.find_syntax_by_token(language)
    {
        return Some(s);
    }
    let name = path.rsplit(['/', '\\']).next().unwrap_or("");
    if name.is_empty() {
        return None;
    }
    set.find_syntax_by_extension(name).or_else(|| name.rsplit_once('.').and_then(|(_, ext)| set.find_syntax_by_extension(ext)))
}

/// Whether a grammar exists for this language or path (loads the grammar set).
pub fn has_syntax(language: &str, path: &str) -> bool {
    find(syntaxes(), language, path).is_some()
}

/// The `(selector, class)` table. The innermost scope of a token that matches any selector
/// decides; `punctuation` only counts when nothing else does, so the quotes of a string are
/// string-coloured.
fn table() -> &'static [(Scope, TokenClass)] {
    static T: OnceLock<Vec<(Scope, TokenClass)>> = OnceLock::new();
    T.get_or_init(|| {
        use TokenClass::*;
        let rows: &[(&str, TokenClass)] = &[
            ("comment", Comment),
            ("string", String),
            ("constant.numeric", Number),
            ("constant", Constant),
            ("keyword.operator", Operator),
            ("keyword", Keyword),
            ("storage", Keyword),
            ("support.macro", Macro),
            ("entity.name.function.macro", Macro),
            ("meta.macro", Macro),
            ("entity.name.function", Function),
            ("support.function", Function),
            ("variable.function", Function),
            ("entity.name.type", Type),
            ("entity.name.class", Type),
            ("entity.name.struct", Type),
            ("entity.name.enum", Type),
            ("entity.name.trait", Type),
            ("entity.name.interface", Type),
            ("entity.other.inherited-class", Type),
            ("support.type", Type),
            ("support.class", Type),
            ("entity.name.tag", Tag),
            ("entity.other.attribute-name", Attribute),
            ("variable.language", Constant),
            ("punctuation", Punctuation),
        ];
        rows.iter().filter_map(|(s, c)| Scope::new(s).ok().map(|s| (s, *c))).collect()
    })
}

/// The class of a token with this scope stack.
fn classify(stack: &[Scope]) -> TokenClass {
    let t = table();
    let mut punct = false;
    for scope in stack.iter().rev() {
        for (sel, class) in t {
            if sel.is_prefix_of(*scope) {
                if *class == TokenClass::Punctuation {
                    punct = true;
                    continue;
                }
                return *class;
            }
        }
    }
    if punct { TokenClass::Punctuation } else { TokenClass::Plain }
}

/// Highlights `text`. `language` is a name or an extension (`rust`, `rs`, `TypeScript`); `path`
/// is a fallback (its file name, then its extension). Blocks until done: a worker's job.
pub fn highlight(text: &str, language: &str, path: &str) -> Outcome {
    highlight_with(text, language, path, TIME_BUDGET)
}

/// [`highlight`] with a chosen time budget (tests).
pub fn highlight_with(text: &str, language: &str, path: &str, budget: Duration) -> Outcome {
    if text.len() > MAX_BYTES {
        return Outcome::TooLarge;
    }
    let set = syntaxes();
    let Some(syntax) = find(set, language, path) else { return Outcome::NoSyntax };
    let started = Instant::now();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut partial = false;
    let mut buf = String::new();
    for raw in text.split_inclusive('\n') {
        if lines.len() >= MAX_LINES {
            return Outcome::TooLarge;
        }
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if partial || line.len() > MAX_LINE_BYTES {
            lines.push(Vec::new());
            continue;
        }
        if started.elapsed() >= budget {
            partial = true;
            lines.push(Vec::new());
            continue;
        }
        buf.clear();
        buf.push_str(line);
        buf.push('\n');
        let ops = match state.parse_line(&buf, set) {
            Ok(o) => o,
            Err(_) => {
                partial = true;
                lines.push(Vec::new());
                continue;
            }
        };
        let len = line.len();
        let mut spans: Vec<Span> = Vec::new();
        let mut push = |start: usize, end: usize, class: TokenClass| {
            let end = end.min(len);
            if class == TokenClass::Plain || start >= end {
                return;
            }
            if let Some(last) = spans.last_mut()
                && last.class == class
                && last.end as usize == start
            {
                last.end = end as u32;
                return;
            }
            spans.push(Span { start: start as u32, end: end as u32, class });
        };
        let mut cur = 0usize;
        for (pos, op) in &ops {
            if *pos > cur {
                push(cur, *pos, classify(stack.as_slice()));
                cur = *pos;
            }
            if stack.apply(op).is_err() {
                partial = true;
                break;
            }
        }
        if cur < len {
            push(cur, len, classify(stack.as_slice()));
        }
        lines.push(spans);
    }
    Outcome::Highlighted(Highlighted { lines, partial, thread: std::thread::current().id() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(text: &str, lang: &str) -> Vec<Vec<(String, TokenClass)>> {
        let Outcome::Highlighted(h) = highlight(text, lang, "") else { panic!("not highlighted: {lang}") };
        text.split('\n')
            .enumerate()
            .map(|(i, l)| h.line(i).iter().map(|s| (l.get(s.start as usize..s.end as usize).unwrap_or("?").to_owned(), s.class)).collect())
            .collect()
    }

    fn has(line: &[(String, TokenClass)], text: &str, class: TokenClass) -> bool {
        line.iter().any(|(t, c)| t.contains(text) && *c == class)
    }

    #[test]
    fn rust_has_keywords_strings_comments_numbers_and_types() {
        let l = classes("fn main() {\n    let s: String = \"hi\"; // note\n    let n = 42;\n}", "rust");
        assert!(has(&l[0], "fn", TokenClass::Keyword), "{l:?}");
        assert!(has(&l[0], "main", TokenClass::Function), "{l:?}");
        assert!(has(&l[1], "\"hi\"", TokenClass::String), "the quotes join the string: {l:?}");
        assert!(has(&l[1], "// note", TokenClass::Comment), "{l:?}");
        assert!(has(&l[1], "String", TokenClass::Type), "{l:?}");
        assert!(has(&l[2], "42", TokenClass::Number), "{l:?}");
    }

    #[test]
    fn a_language_by_name_by_extension_and_by_path() {
        for lang in ["rust", "Rust", "rs", "RS"] {
            assert!(has_syntax(lang, ""), "{lang}");
        }
        assert!(has_syntax("", "src/lib.rs") && has_syntax("", "C:\\x\\y\\main.py") && has_syntax("", "pkg/Dockerfile"));
        assert!(!has_syntax("klingon", "") && !has_syntax("", "noextension") && !has_syntax("", ""));
        // The language wins over the path.
        assert!(has_syntax("rust", "weird.zzz"));
        // two-face brings the grammars syntect lacks.
        for lang in ["typescript", "tsx", "toml", "dockerfile"] {
            assert!(has_syntax(lang, ""), "{lang}");
        }
        assert!(matches!(highlight("x", "klingon", ""), Outcome::NoSyntax));
    }

    #[test]
    fn other_languages_highlight_too() {
        let l = classes("def f(x):\n    return \"a\" # c\n", "python");
        assert!(has(&l[0], "def", TokenClass::Keyword) && has(&l[1], "\"a\"", TokenClass::String) && has(&l[1], "# c", TokenClass::Comment), "{l:?}");
        let l = classes("const x: number = 1;", "typescript");
        assert!(has(&l[0], "const", TokenClass::Keyword) || has(&l[0], "const", TokenClass::Constant), "{l:?}");
        assert!(has(&l[0], "1", TokenClass::Number), "{l:?}");
        let l = classes("[a]\nname = \"x\"\n", "toml");
        assert!(has(&l[1], "\"x\"", TokenClass::String), "{l:?}");
    }

    #[test]
    fn crlf_text_gives_the_same_spans_as_lf() {
        let lf = classes("let a = 1;\nlet b = \"x\";", "rust");
        let crlf = classes("let a = 1;\r\nlet b = \"x\";", "rust");
        assert_eq!(lf[1], crlf[1]);
        let Outcome::Highlighted(h) = highlight("let a = 1;\r\nlet b = 2;", "rust", "") else { panic!() };
        assert!(h.line(0).iter().all(|s| s.end <= 10), "no span reaches into the CR");
    }

    #[test]
    fn hostile_text_never_panics_and_spans_stay_inside_their_line() {
        let samples = [
            String::new(),
            "\n\n\n".into(),
            "\u{0}\u{1b}[31m\u{202e}\u{fffd}".repeat(500),
            "\"unterminated".repeat(100),
            "/* never closed\n".repeat(50),
            "{".repeat(20_000),
            "a".repeat(MAX_LINE_BYTES + 1),
            "\u{1f980}\u{4e2d}\u{6587} caf\u{e9}\n".repeat(200),
        ];
        for lang in ["rust", "python", "typescript", "toml", "html", "json", "markdown", "bash", "cpp"] {
            for s in &samples {
                if let Outcome::Highlighted(h) = highlight(s, lang, "") {
                    for (i, line) in s.split('\n').enumerate() {
                        let line = line.strip_suffix('\r').unwrap_or(line);
                        let mut prev = 0;
                        for sp in h.line(i) {
                            assert!(sp.start >= prev && sp.start < sp.end && sp.end as usize <= line.len(), "{lang} line {i}: {sp:?} in {} bytes", line.len());
                            assert!(line.is_char_boundary(sp.start as usize) && line.is_char_boundary(sp.end as usize), "{lang} {i}: offsets are char boundaries");
                            prev = sp.end;
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_long_line_is_left_plain_and_the_next_one_still_highlights() {
        let text = format!("// {}\nfn a() {{}}", "x".repeat(MAX_LINE_BYTES + 10));
        let Outcome::Highlighted(h) = highlight(&text, "rust", "") else { panic!() };
        assert!(h.line(0).is_empty());
        assert!(!h.line(1).is_empty());
    }

    #[test]
    fn past_the_caps_nothing_is_highlighted() {
        assert!(matches!(highlight(&"a".repeat(MAX_BYTES + 1), "rust", ""), Outcome::TooLarge));
        assert!(matches!(highlight(&"\n".repeat(MAX_LINES + 5), "rust", ""), Outcome::TooLarge));
    }

    #[test]
    fn the_time_budget_stops_the_run_with_what_it_has() {
        let text = "fn a() { let x = 1; }\n".repeat(3000);
        let Outcome::Highlighted(h) = highlight_with(&text, "rust", "", Duration::ZERO) else { panic!() };
        assert!(h.partial);
        assert_eq!(h.lines.len(), 3000, "every line still has an entry");
    }
}

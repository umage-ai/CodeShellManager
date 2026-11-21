//! The Markdown subset an add-on screen may show (docs/addons.md, "Screens"): paragraphs, headings,
//! emphasis, code spans and fenced code, lists, quotes, rules and links. Parsed here, off the UI
//! thread, into a small tree the renderer only walks.
//!
//! Not supported, by design: images (the alt text is shown as text), raw HTML (shown as text),
//! tables and everything else CommonMark extensions add. The parser reads hostile text: the source
//! is capped, nesting is capped, and the number of blocks is capped, so nothing here can blow up.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::ScreenError;

/// Longest Markdown source, in bytes.
pub const MAX_SOURCE: usize = 64 * 1024;
/// Deepest nesting of lists and quotes; deeper content is flattened into its parent.
pub const MAX_NESTING: usize = 8;
/// Most blocks kept from one source (the rest is dropped).
pub const MAX_BLOCKS: usize = 2_000;
/// Longest link target kept.
pub const MAX_HREF: usize = 2_048;

/// Where a link goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Href {
    /// An absolute `http` or `https` URL. A click goes through `open_url`.
    Http(String),
    /// A `csm:` link that stays inside the screen (a `link` event).
    Csm(String),
    /// Any other scheme (or a malformed one): drawn as plain text, never clickable.
    Inert,
}

/// Inline content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Link { href: Href, kids: Vec<Inline> },
    Break,
}

/// A block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Code { lang: Option<String>, text: String },
    /// `start` is `Some(first number)` for an ordered list.
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Rule,
}

/// Classifies a link target. Only absolute `http(s)` URLs and `csm:` links are live.
pub fn classify_href(dest: &str) -> Href {
    if dest.len() > MAX_HREF || dest.chars().any(|c| c.is_control() || c.is_whitespace() || super::is_invisible(c)) {
        return Href::Inert;
    }
    let lower = dest.get(..8).unwrap_or(dest).to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let rest = dest.split_once("://").map_or("", |(_, r)| r);
        if rest.is_empty() || rest.starts_with('/') {
            return Href::Inert;
        }
        return Href::Http(dest.to_owned());
    }
    if dest.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("csm:")) && dest.len() <= 512 {
        return Href::Csm(dest.to_owned());
    }
    Href::Inert
}

/// Whether a link's visible text looks like a URL that differs from where it goes (a phishing
/// shape). The renderer then shows the real target beside the text.
pub fn text_misleads(text: &str, href: &Href) -> bool {
    let Href::Http(target) = href else { return false };
    let t = text.trim();
    let looks_like_url = t.starts_with("http://") || t.starts_with("https://") || t.starts_with("www.");
    looks_like_url && t.trim_end_matches('/') != target.trim_end_matches('/')
}

enum Frame {
    Para(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Code { lang: Option<String>, text: String },
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Item { blocks: Vec<Block>, pending: Vec<Inline> },
    Quote(Vec<Block>),
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Link { href: Href, kids: Vec<Inline> },
    /// Something not supported (an image, an HTML block, an extension): its text flows into the parent.
    Flow(Vec<Inline>),
}

fn depth(stack: &[Frame]) -> usize {
    stack.iter().filter(|f| matches!(f, Frame::List { .. } | Frame::Quote(_))).count()
}

fn push_inline(stack: &mut [Frame], out: &mut Vec<Block>, i: Inline) {
    match stack.last_mut() {
        Some(Frame::Para(v) | Frame::Heading(_, v) | Frame::Emph(v) | Frame::Strong(v) | Frame::Link { kids: v, .. } | Frame::Flow(v)) => push_merged(v, i),
        Some(Frame::Item { pending, .. }) => push_merged(pending, i),
        // Text directly in a quote or at the top level (loose text): its own paragraph.
        Some(Frame::Quote(b)) => b.push(Block::Paragraph(vec![i])),
        Some(Frame::Code { text, .. }) => {
            if let Inline::Text(t) | Inline::Code(t) = i {
                text.push_str(&t);
            }
        }
        Some(Frame::List { .. }) => {}
        None => out.push(Block::Paragraph(vec![i])),
    }
}

fn push_merged(v: &mut Vec<Inline>, i: Inline) {
    if let (Inline::Text(add), Some(Inline::Text(last))) = (&i, v.last_mut()) {
        last.push_str(add);
        return;
    }
    v.push(i);
}

fn push_block(stack: &mut [Frame], out: &mut Vec<Block>, count: &mut usize, b: Block) {
    if *count >= MAX_BLOCKS {
        return;
    }
    *count += 1;
    match stack.last_mut() {
        Some(Frame::Item { blocks, pending }) => {
            if !pending.is_empty() {
                blocks.push(Block::Paragraph(std::mem::take(pending)));
            }
            blocks.push(b);
        }
        Some(Frame::Quote(blocks)) => blocks.push(b),
        Some(_) => {}
        None => out.push(b),
    }
}

/// Parses `source` into blocks. Errors only when the source is over [`MAX_SOURCE`].
pub fn parse(source: &str) -> Result<Vec<Block>, ScreenError> {
    if source.len() > MAX_SOURCE {
        return Err(ScreenError::new(format!("a Markdown text is over {MAX_SOURCE} bytes")));
    }
    let mut out: Vec<Block> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut count = 0usize;
    for ev in Parser::new_ext(source, Options::empty()) {
        if count >= MAX_BLOCKS {
            break;
        }
        match ev {
            Event::Start(tag) => {
                let nested = depth(&stack) >= MAX_NESTING;
                stack.push(match tag {
                    Tag::Paragraph => Frame::Para(Vec::new()),
                    Tag::Heading { level, .. } => Frame::Heading(level as u8, Vec::new()),
                    Tag::CodeBlock(kind) => Frame::Code {
                        lang: match kind {
                            CodeBlockKind::Fenced(l) => Some(super::clean_label(l.split_whitespace().next().unwrap_or(""), 32)).filter(|l| !l.is_empty()),
                            CodeBlockKind::Indented => None,
                        },
                        text: String::new(),
                    },
                    Tag::List(start) if !nested => Frame::List { start, items: Vec::new() },
                    Tag::Item => Frame::Item { blocks: Vec::new(), pending: Vec::new() },
                    Tag::BlockQuote(_) if !nested => Frame::Quote(Vec::new()),
                    Tag::Emphasis => Frame::Emph(Vec::new()),
                    Tag::Strong => Frame::Strong(Vec::new()),
                    Tag::Link { dest_url, .. } => Frame::Link { href: classify_href(&dest_url), kids: Vec::new() },
                    _ => Frame::Flow(Vec::new()),
                });
            }
            Event::End(end) => {
                let Some(frame) = stack.pop() else { continue };
                match frame {
                    Frame::Para(v) => push_block(&mut stack, &mut out, &mut count, Block::Paragraph(v)),
                    Frame::Heading(l, v) => push_block(&mut stack, &mut out, &mut count, Block::Heading(l, v)),
                    Frame::Code { lang, text } => push_block(&mut stack, &mut out, &mut count, Block::Code { lang, text }),
                    Frame::List { start, items } => push_block(&mut stack, &mut out, &mut count, Block::List { start, items }),
                    Frame::Item { mut blocks, pending } => {
                        if !pending.is_empty() {
                            blocks.push(Block::Paragraph(pending));
                        }
                        if let Some(Frame::List { items, .. }) = stack.last_mut() {
                            items.push(blocks);
                        } else {
                            // An item whose list was flattened: its blocks join the parent.
                            for b in blocks {
                                push_block(&mut stack, &mut out, &mut count, b);
                            }
                        }
                    }
                    Frame::Quote(b) => push_block(&mut stack, &mut out, &mut count, Block::Quote(b)),
                    Frame::Emph(v) => push_inline(&mut stack, &mut out, Inline::Emph(v)),
                    Frame::Strong(v) => push_inline(&mut stack, &mut out, Inline::Strong(v)),
                    Frame::Link { href, kids } => push_inline(&mut stack, &mut out, Inline::Link { href, kids }),
                    Frame::Flow(v) => {
                        // An HTML block ends as a paragraph of its text; an inline one (an image's
                        // alt text) flows into the parent.
                        if matches!(end, TagEnd::HtmlBlock) && !v.is_empty() {
                            push_block(&mut stack, &mut out, &mut count, Block::Paragraph(v));
                        } else {
                            for i in v {
                                push_inline(&mut stack, &mut out, i);
                            }
                        }
                    }
                }
            }
            Event::Text(t) => push_inline(&mut stack, &mut out, Inline::Text(t.into_string())),
            Event::Code(t) => push_inline(&mut stack, &mut out, Inline::Code(t.into_string())),
            Event::Html(t) | Event::InlineHtml(t) => push_inline(&mut stack, &mut out, Inline::Text(t.into_string())),
            Event::SoftBreak => push_inline(&mut stack, &mut out, Inline::Text(" ".into())),
            Event::HardBreak => push_inline(&mut stack, &mut out, Inline::Break),
            Event::Rule => push_block(&mut stack, &mut out, &mut count, Block::Rule),
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_emphasis_code_and_links() {
        let b = parse("Hello **bold** and *it* with `code` and [site](https://example.com/a).").unwrap();
        let Block::Paragraph(p) = &b[0] else { panic!("{b:?}") };
        assert!(p.contains(&Inline::Strong(vec![Inline::Text("bold".into())])));
        assert!(p.contains(&Inline::Code("code".into())));
        assert!(p.iter().any(|i| matches!(i, Inline::Link { href: Href::Http(h), .. } if h == "https://example.com/a")));
    }

    #[test]
    fn lists_quotes_headings_and_fences() {
        let b = parse("# T\n\n- a\n- b\n\n1. x\n\n> q\n\n```rust\nlet x = 1;\n```\n").unwrap();
        assert!(matches!(&b[0], Block::Heading(1, _)));
        assert!(matches!(&b[1], Block::List { start: None, items } if items.len() == 2));
        assert!(matches!(&b[2], Block::List { start: Some(1), .. }));
        assert!(matches!(&b[3], Block::Quote(_)));
        assert!(matches!(&b[4], Block::Code { lang: Some(l), text } if l == "rust" && text == "let x = 1;\n"));
    }

    #[test]
    fn images_and_html_are_text_and_odd_schemes_are_inert() {
        let b = parse("![alt text](http://x/y.png) <b>raw</b> [a](javascript:alert(1)) [b](file:///c) [c](csm:select?key=f)").unwrap();
        let Block::Paragraph(p) = &b[0] else { panic!("{b:?}") };
        assert!(!p.iter().any(|i| matches!(i, Inline::Link { href: Href::Http(_), .. })), "an image is not a link");
        assert!(p.iter().filter(|i| matches!(i, Inline::Link { href: Href::Inert, .. })).count() == 2);
        assert!(p.iter().any(|i| matches!(i, Inline::Link { href: Href::Csm(_), .. })));
        let text: String = p.iter().filter_map(|i| if let Inline::Text(t) = i { Some(t.as_str()) } else { None }).collect();
        assert!(text.contains("alt text") && text.contains("<b>"), "{text}");
    }

    #[test]
    fn href_rules() {
        assert_eq!(classify_href("https://a.b/c"), Href::Http("https://a.b/c".into()));
        assert_eq!(classify_href("HTTP://a.b"), Href::Http("HTTP://a.b".into()));
        assert_eq!(classify_href("https://"), Href::Inert);
        assert_eq!(classify_href("https:///x"), Href::Inert);
        assert_eq!(classify_href("ftp://a"), Href::Inert);
        assert_eq!(classify_href("https://a b"), Href::Inert);
        assert_eq!(classify_href("https://a\u{202E}b"), Href::Inert);
        assert!(matches!(classify_href("csm:select?key=x"), Href::Csm(_)));
        assert_eq!(classify_href(&format!("https://a/{}", "x".repeat(MAX_HREF))), Href::Inert);
    }

    #[test]
    fn a_link_text_that_names_another_url_misleads() {
        let h = Href::Http("https://evil.example/".into());
        assert!(text_misleads("https://bank.example", &h));
        assert!(text_misleads("www.bank.example", &h));
        assert!(!text_misleads("https://evil.example", &h));
        assert!(!text_misleads("click here", &h));
        assert!(!text_misleads("https://x", &Href::Inert));
    }

    #[test]
    fn hostile_input_is_bounded() {
        assert!(parse(&"a".repeat(MAX_SOURCE + 1)).is_err());
        // Deep nesting is flattened, not recursed into.
        let deep = format!("{}x", "> ".repeat(500));
        assert!(!parse(&deep).unwrap().is_empty());
        let lists = format!("{}x", "- ".repeat(300));
        assert!(!parse(&lists).unwrap().is_empty());
        // Many blocks stop at the cap.
        let rules = "---\n\n".repeat(MAX_SOURCE / 5);
        assert!(parse(&rules).unwrap().len() <= MAX_BLOCKS);
        // Garbage and control characters do not panic.
        let _ = parse("\u{0}\u{1b}[31m\u{202E}[x](<>) ![](\n```\n").unwrap();
    }
}

//! Text from a plugin is untrusted. Labels drop control, bidi and zero-width characters. Bodies of
//! text, code and comments keep them out of sight no longer: [`reveal`] turns each one into a
//! visible marker, so a review tool shows a Trojan-Source trick instead of hiding or obeying it.

use super::super::manifest::clean_text;

/// Whether `c` is a character that draws as nothing or changes how text is drawn: controls (but
/// not newline or tab), bidi overrides and isolates, zero-width characters, the soft hyphen, the
/// word joiner family, the BOM, and the line and paragraph separators.
pub fn is_invisible(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return false;
    }
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
        )
}

/// A one-line label: control, bidi and zero-width characters dropped, trimmed, at most `cap`
/// characters.
pub fn clean_label(s: &str, cap: usize) -> String {
    let kept: String = s.chars().filter(|c| !is_invisible(*c)).collect();
    clean_text(&kept, cap)
}

/// One run of [`reveal`]'s output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    /// A marker CSM drew for an invisible character (drawn in a `warning` tint).
    pub marker: bool,
}

/// The visible marker for one invisible character.
pub fn marker_for(c: char) -> String {
    match c {
        '\u{1b}' => "\u{241B}".into(),
        '\0' => "\u{2400}".into(),
        '\r' => "\u{240D}".into(),
        '\u{7f}' => "\u{2421}".into(),
        _ => format!("\u{27E8}U+{:04X}\u{27E9}", u32::from(c)),
    }
}

/// `s` as runs of ordinary text and markers. A tab becomes four spaces and a `\r\n` pair is one
/// line break; every other invisible character becomes a marker. The text is otherwise unchanged.
pub fn reveal(s: &str) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    let mut chars = s.chars().peekable();
    let push = |out: &mut Vec<Seg>, text: &str, marker: bool| {
        if let Some(last) = out.last_mut()
            && last.marker == marker
        {
            last.text.push_str(text);
            return;
        }
        out.push(Seg { text: text.to_owned(), marker });
    };
    while let Some(c) = chars.next() {
        match c {
            '\t' => push(&mut out, "    ", false),
            '\r' if chars.peek() == Some(&'\n') => {}
            c if is_invisible(c) => push(&mut out, &marker_for(c), true),
            c => {
                let mut b = [0u8; 4];
                push(&mut out, c.encode_utf8(&mut b), false);
            }
        }
    }
    out
}

/// Whether `s` has any character [`reveal`] would mark.
pub fn has_invisible(s: &str) -> bool {
    s.chars().any(is_invisible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_drop_the_invisible() {
        assert_eq!(clean_label("a\u{202E}b\u{200B}c\u{1b}[31m\td", 50), "abc[31md");
        assert_eq!(clean_label("  spaced  ", 50), "spaced");
        assert_eq!(clean_label("abcdef", 3), "abc");
        assert_eq!(clean_label("\u{2066}x\u{2069}", 50), "x");
    }

    #[test]
    fn reveal_marks_escape_bidi_and_zero_width() {
        let segs = reveal("let a = \"\u{202E}admin\u{202C}\"; \u{1b}[2J\u{200B}x");
        let markers: Vec<&str> = segs.iter().filter(|s| s.marker).map(|s| s.text.as_str()).collect();
        assert_eq!(markers, vec!["\u{27E8}U+202E\u{27E9}", "\u{27E8}U+202C\u{27E9}", "\u{241B}", "\u{27E8}U+200B\u{27E9}"]);
        let plain: String = segs.iter().filter(|s| !s.marker).map(|s| s.text.as_str()).collect();
        assert_eq!(plain, "let a = \"admin\"; [2Jx");
    }

    #[test]
    fn reveal_keeps_newlines_expands_tabs_and_folds_crlf() {
        let segs = reveal("a\tb\r\nc\rd");
        let all: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(all, "a    b\nc\u{240D}d");
        assert!(segs.iter().any(|s| s.marker && s.text == "\u{240D}"));
        assert!(reveal("").is_empty());
    }

    #[test]
    fn every_c0_c1_and_format_control_is_marked() {
        for c in (0u32..0x20).chain(0x7f..0xa0).filter_map(char::from_u32) {
            if c == '\n' || c == '\t' {
                continue;
            }
            assert!(is_invisible(c), "{c:?}");
            assert!(reveal(&c.to_string()).iter().all(|s| s.marker), "{c:?}");
        }
        for c in ['\u{200B}', '\u{200D}', '\u{200F}', '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{FEFF}', '\u{2028}', '\u{00AD}', '\u{2060}'] {
            assert!(is_invisible(c), "{c:?}");
        }
        assert!(!is_invisible('a') && !is_invisible('\u{e9}') && !is_invisible('\u{4e2d}') && !is_invisible(' '));
    }
}

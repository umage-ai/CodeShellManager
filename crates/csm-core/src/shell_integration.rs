//! Validation and normalisation for the OSC 9001 shell-integration channel (C#
//! `ShellIntegrationPayload`).
//!
//! Every value is untrusted: it arrives from whatever is printing to the terminal (a remote
//! host, a `cat` of an arbitrary file, a hook) and some of it ends up persisted in
//! `state.json`. Reject what cannot be validated, trim and cap the rest.

use crate::text::{prefix_utf16, utf16_len};

/// Upper bound on a title pushed through OSC 9001, in UTF-16 code units (C# `string.Length`).
pub const MAX_TITLE_LENGTH: usize = 80;

/// Cap for an OSC 9001 `git-branch` value, in UTF-16 code units. Bounds a hostile emitter; it
/// does not police real ref names.
pub const MAX_BRANCH_LENGTH: usize = 200;

/// Accepts `#rgb`, `#rrggbb` and `#rrggbbaa`. Returns the form a WPF-style colour converter
/// expects: 3- and 6-digit values unchanged, 8-digit values reordered from the integrator
/// convention (alpha last) to `#aarrggbb` (alpha first). Anything else is rejected.
pub fn try_normalize_color(input: &str) -> Option<String> {
    let rest = input.strip_prefix('#')?;
    if !matches!(rest.len(), 3 | 6 | 8) || !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if rest.len() == 8 {
        let (rgb, alpha) = rest.split_at(6);
        Some(format!("#{alpha}{rgb}"))
    } else {
        Some(input.to_owned())
    }
}

/// Only `1` and `true` (any case) mean dirty; everything else is clean.
pub fn parse_dirty(input: &str) -> bool {
    input == "1" || input.eq_ignore_ascii_case("true")
}

/// Strips control characters, trims, and caps at [`MAX_TITLE_LENGTH`] without splitting a
/// surrogate pair. `None` when nothing usable is left, so the caller leaves the existing name
/// alone instead of blanking it.
pub fn sanitize_title(input: &str) -> Option<String> {
    let clean = strip_controls(input)?;
    if utf16_len(&clean) <= MAX_TITLE_LENGTH {
        return Some(clean);
    }
    // C# parity: the title path trims after the cut but may return an empty string only if the
    // cut text were all whitespace, which `strip_controls` already trimmed away at the ends.
    Some(prefix_utf16(&clean, MAX_TITLE_LENGTH).trim_end().to_owned())
}

/// Strips control characters, trims, and caps length. `None` for an empty result (detached
/// HEAD, not a repo).
///
/// The cap is not cosmetic: the value is rendered into a sidebar row, and an unbounded branch
/// let any program wedge the UI with a megabyte-long "branch name".
pub fn sanitize_branch(input: &str) -> Option<String> {
    let clean = strip_controls(input)?;
    if utf16_len(&clean) <= MAX_BRANCH_LENGTH {
        return Some(clean);
    }
    let trimmed = prefix_utf16(&clean, MAX_BRANCH_LENGTH).trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn strip_controls(input: &str) -> Option<String> {
    if input.trim().is_empty() {
        return None;
    }
    let stripped: String = input.chars().filter(|c| !c.is_control()).collect();
    let result = stripped.trim();
    (!result.is_empty()).then(|| result.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- color ----

    #[test]
    fn try_normalize_color_valid_hex_returns_wpf_form() {
        for (input, expected) in [
            ("#abc", "#abc"),
            ("#a6e3a1", "#a6e3a1"),
            ("#A6E3A1", "#A6E3A1"),
            ("#a6e3a180", "#80a6e3a1"), // #rrggbbaa (integrator) -> #aarrggbb (WPF)
        ] {
            assert_eq!(
                try_normalize_color(input).as_deref(),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn try_normalize_color_invalid_returns_none() {
        for input in [
            "",
            "red",
            "rgb(1,2,3)",
            "a6e3a1",   // missing '#'
            "#a6e3a",   // 5 digits
            "#a6e3a1f", // 7 digits
            "#gggggg",  // not hex
            "#a6e3a1;title=x",
            "#",
            "#\u{ff11}\u{ff12}\u{ff13}", // full-width digits are not hex
            "#\u{e9}abcd",               // 6 bytes but not 6 hex digits
        ] {
            assert_eq!(try_normalize_color(input), None, "{input:?}");
        }
    }

    // ---- git-dirty ----

    #[test]
    fn parse_dirty_only_one_and_true_count() {
        for (input, expected) in [
            ("1", true),
            ("true", true),
            ("TRUE", true),
            ("0", false),
            ("false", false),
            ("", false),
            ("yes", false), // only 1/true count as dirty
        ] {
            assert_eq!(parse_dirty(input), expected, "{input:?}");
        }
    }

    // ---- title ----

    #[test]
    fn sanitize_title_printable_trims_and_strips_controls() {
        for (input, expected) in [
            ("my-repo", "my-repo"),
            ("  padded  ", "padded"),
            ("tab\there", "tabhere"),           // control chars stripped
            ("esc\u{1b}[31mred", "esc[31mred"), // ESC stripped, printable remainder kept
            ("multi\r\nline", "multiline"),
            ("ünïcödé ⎇ ok", "ünïcödé ⎇ ok"), // non-ASCII printable is fine
        ] {
            assert_eq!(
                sanitize_title(input).as_deref(),
                Some(expected),
                "{input:?}"
            );
        }
    }

    #[test]
    fn sanitize_title_nothing_usable_returns_none() {
        for input in ["", "   ", "\u{7}\u{1b}"] {
            assert_eq!(sanitize_title(input), None, "{input:?}");
        }
    }

    #[test]
    fn sanitize_title_long_title_is_capped_at_max_title_length() {
        let input = "x".repeat(MAX_TITLE_LENGTH + 50);
        let result = sanitize_title(&input).unwrap();
        assert_eq!(utf16_len(&result), MAX_TITLE_LENGTH);
    }

    #[test]
    fn sanitize_title_cap_does_not_split_a_surrogate_pair() {
        // One unit short of the cap, then a 2-unit emoji straddling the boundary.
        let input = format!("{}😀", "x".repeat(MAX_TITLE_LENGTH - 1));
        let result = sanitize_title(&input).unwrap();
        assert!(
            !result.contains('😀'),
            "the straddling emoji is dropped whole"
        );
        assert_eq!(utf16_len(&result), MAX_TITLE_LENGTH - 1);
    }

    // ---- git-branch ----

    #[test]
    fn sanitize_branch_cases() {
        for (input, expected) in [
            ("main", Some("main")),
            (" feat/x ", Some("feat/x")),
            ("", None),
            ("   ", None),
            ("a\u{1b}b", Some("ab")),
        ] {
            assert_eq!(sanitize_branch(input).as_deref(), expected, "{input:?}");
        }
    }

    #[test]
    fn sanitize_branch_long_branch_is_capped() {
        // Found in the v0.8.0 pre-release review: titles were capped, branches were not.
        let huge = "b".repeat(100_000);
        let result = sanitize_branch(&huge).unwrap();
        assert_eq!(utf16_len(&result), MAX_BRANCH_LENGTH);
    }

    #[test]
    fn sanitize_branch_real_branch_names_are_never_truncated() {
        let realistic = "feature/some-quite-long-but-entirely-reasonable-branch-name-v2";
        assert_eq!(sanitize_branch(realistic).as_deref(), Some(realistic));
    }

    #[test]
    fn sanitize_branch_cap_does_not_split_a_surrogate_pair() {
        let emoji = "😀".repeat(500);
        let result = sanitize_branch(&emoji).unwrap();
        // 200 units = exactly 100 whole emoji; never a trailing half.
        assert_eq!(result.chars().count(), 100);
        assert!(result.chars().all(|c| c == '😀'));
    }

    #[test]
    fn sanitize_branch_only_whitespace_after_the_cap_is_none() {
        // Cap lands inside trailing spaces that follow real text: trimmed, not empty.
        let s = format!("a{}b", " ".repeat(300));
        assert_eq!(sanitize_branch(&s).as_deref(), Some("a"));
    }
}

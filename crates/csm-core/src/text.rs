//! Small text helpers that reproduce .NET string semantics where the C# behaviour depends on
//! them. C# `string.Length` counts UTF-16 code units, not bytes or scalar values, and several
//! caps and tests are phrased in those units.

/// `string.Length` in C#: the number of UTF-16 code units.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The longest prefix of `s` that is at most `max_units` UTF-16 code units long, never
/// splitting a surrogate pair (C# `s[..n]` can leave a lone surrogate; a Rust `str` cannot).
pub(crate) fn prefix_utf16(s: &str, max_units: usize) -> &str {
    let mut units = 0usize;
    for (idx, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max_units {
            return s.get(..idx).unwrap_or("");
        }
    }
    s
}

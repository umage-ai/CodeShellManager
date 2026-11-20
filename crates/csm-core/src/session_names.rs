//! Names for sessions made from another one (C# `MainWindow.DeriveDuplicateName`).

/// "api" → "api (2)"; "api (2)" → "api (3)", skipping names already taken (compared
/// case-insensitively). Gives up after 100 tries and returns the first candidate.
pub fn duplicate_name(base: &str, taken: &[impl AsRef<str>]) -> String {
    let (stem, start) = split_counter(base).unwrap_or((base, 2));
    let is_taken = |c: &str| taken.iter().any(|t| t.as_ref().chars().flat_map(char::to_lowercase).eq(c.chars().flat_map(char::to_lowercase)));
    for n in start..start.saturating_add(100) {
        let candidate = format!("{stem} ({n})");
        if !is_taken(&candidate) {
            return candidate;
        }
    }
    format!("{stem} ({start})")
}

/// `"stem (N)"` → `(stem, N + 1)` (C# regex `^(.*) \((\d+)\)$`).
fn split_counter(s: &str) -> Option<(&str, u32)> {
    let inner = s.strip_suffix(')')?;
    let open = inner.rfind(" (")?;
    let digits = inner.get(open + 2..)?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    Some((inner.get(..open)?, n.saturating_add(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: [&str; 0] = [];

    #[test]
    fn appends_two_then_counts_up() {
        assert_eq!(duplicate_name("api", &NONE), "api (2)");
        assert_eq!(duplicate_name("api (2)", &NONE), "api (3)");
        assert_eq!(duplicate_name("api (9)", &NONE), "api (10)");
    }

    #[test]
    fn skips_taken_names_case_insensitively() {
        assert_eq!(duplicate_name("api", &["API (2)", "api (3)"]), "api (4)");
    }

    #[test]
    fn parentheses_that_are_not_a_counter_stay_in_the_stem() {
        assert_eq!(duplicate_name("Odulodo.Web (PS)", &NONE), "Odulodo.Web (PS) (2)");
        assert_eq!(duplicate_name("x ()", &NONE), "x () (2)");
        assert_eq!(duplicate_name("(3)", &NONE), "(3) (2)");
        assert_eq!(duplicate_name("", &NONE), " (2)");
    }

    #[test]
    fn a_huge_counter_does_not_overflow() {
        let s = duplicate_name("a (99999999999999999999)", &NONE);
        assert_eq!(s, "a (99999999999999999999) (2)");
        let s = duplicate_name(&format!("a ({})", u32::MAX), &NONE);
        assert_eq!(s, format!("a ({})", u32::MAX));
    }
}

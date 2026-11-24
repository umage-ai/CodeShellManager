//! Keyboard navigation of popup menus (`docs/design/keyboard.md`, section 3). Pure: the rows of a
//! menu level and the keys pressed go in, a decision comes out. The menus (`screens/menus.rs`)
//! and the Recent menu both use it, so every menu moves, activates and closes the same way.

/// The keys a menu reads this frame, already matched to their chords by the caller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keys {
    /// Up, or Shift+Tab.
    pub up: bool,
    /// Down, or Tab.
    pub down: bool,
    pub home: bool,
    pub end: bool,
    /// Enter or Space.
    pub enter: bool,
    /// Right: opens a submenu.
    pub forward: bool,
    /// Left: leaves a submenu.
    pub back: bool,
    pub escape: bool,
    /// A letter or digit typed, for access keys.
    pub letter: Option<char>,
}

/// One row of a menu level: whether the highlight can rest on it, and its access letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub selectable: bool,
    pub letter: Option<char>,
}

/// What the menu does for the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// Nothing to do.
    Nothing,
    /// Move the highlight to this row.
    Highlight(usize),
    /// Activate this row (Enter, Space, or its access letter).
    Activate(usize),
    /// Right on this row: open it when it is a submenu.
    Forward(usize),
    /// Leave the open submenu (Left, or Escape in a submenu).
    Back,
    /// Close the menu (Escape at the top level).
    Close,
}

/// The row the highlight goes to for a step (`delta` = 1 or -1) from `sel`, wrapping, skipping rows
/// the highlight cannot rest on. `None` when no row can take it.
pub fn step(sel: Option<usize>, rows: &[Row], delta: isize) -> Option<usize> {
    let n = rows.len();
    if n == 0 {
        return None;
    }
    let n_isize = isize::try_from(n).ok()?;
    let start = match sel {
        Some(s) if s < n => isize::try_from(s).ok()?,
        _ if delta > 0 => -1,
        _ => n_isize,
    };
    (1..=n_isize).find_map(|k| {
        let idx = usize::try_from((start + delta * k).rem_euclid(n_isize)).ok()?;
        rows.get(idx).filter(|r| r.selectable).map(|_| idx)
    })
}

/// The first row the highlight can rest on.
pub fn first(rows: &[Row]) -> Option<usize> {
    rows.iter().position(|r| r.selectable)
}

/// The last row the highlight can rest on.
pub fn last(rows: &[Row]) -> Option<usize> {
    rows.iter().rposition(|r| r.selectable)
}

/// The row for what the keys asked, given the highlighted row `sel` at this level. `can_back`:
/// the level is a submenu (Left and Escape go back to its parent rather than closing the menu).
pub fn navigate(keys: Keys, sel: Option<usize>, rows: &[Row], can_back: bool) -> Nav {
    if keys.escape {
        return if can_back { Nav::Back } else { Nav::Close };
    }
    if keys.up || keys.down {
        let delta = if keys.down { 1 } else { -1 };
        return step(sel, rows, delta).map_or(Nav::Nothing, Nav::Highlight);
    }
    if keys.home {
        return first(rows).map_or(Nav::Nothing, Nav::Highlight);
    }
    if keys.end {
        return last(rows).map_or(Nav::Nothing, Nav::Highlight);
    }
    if keys.enter {
        return sel.filter(|s| rows.get(*s).is_some_and(|r| r.selectable)).map_or(Nav::Nothing, Nav::Activate);
    }
    if keys.forward {
        return sel.map_or(Nav::Nothing, Nav::Forward);
    }
    if keys.back {
        return if can_back { Nav::Back } else { Nav::Nothing };
    }
    if let Some(c) = keys.letter {
        let c = c.to_ascii_lowercase();
        let hit = rows.iter().position(|r| r.selectable && r.letter.is_some_and(|l| l.to_ascii_lowercase() == c));
        return hit.map_or(Nav::Nothing, Nav::Activate);
    }
    Nav::Nothing
}

/// The access letter of each labelled row: the first letter or digit of its label that no earlier
/// row of the menu has taken. `Some((char index, letter))`, the index being where the letter sits in
/// the label (drawn underlined). `None` for a row without a label, or whose label has no free letter.
pub fn access(labels: &[Option<&str>]) -> Vec<Option<(usize, char)>> {
    let mut taken: Vec<char> = Vec::new();
    labels
        .iter()
        .map(|label| {
            let label = (*label)?;
            let pick = label.chars().enumerate().find_map(|(ci, c)| {
                let l = c.to_lowercase().next()?;
                (c.is_alphanumeric() && !taken.contains(&l)).then_some((ci, l))
            })?;
            taken.push(pick.1);
            Some(pick)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(spec: &[(bool, Option<char>)]) -> Vec<Row> {
        spec.iter().map(|(selectable, letter)| Row { selectable: *selectable, letter: *letter }).collect()
    }

    #[test]
    fn step_wraps_and_skips_unselectable_rows() {
        // Row 1 is a separator or a heading: the highlight passes over it.
        let r = rows(&[(true, None), (false, None), (true, None)]);
        assert_eq!(step(Some(0), &r, 1), Some(2));
        assert_eq!(step(Some(2), &r, 1), Some(0));
        assert_eq!(step(Some(0), &r, -1), Some(2));
    }

    #[test]
    fn step_from_nothing_takes_the_first_or_the_last() {
        let r = rows(&[(false, None), (true, None), (true, None), (false, None)]);
        assert_eq!(step(None, &r, 1), Some(1));
        assert_eq!(step(None, &r, -1), Some(2));
    }

    #[test]
    fn step_has_nothing_to_rest_on_in_an_empty_or_inert_menu() {
        assert_eq!(step(None, &[], 1), None);
        assert_eq!(step(Some(0), &rows(&[(false, None)]), 1), None);
    }

    #[test]
    fn home_and_end_jump_to_the_ends_that_can_take_the_highlight() {
        let r = rows(&[(false, None), (true, None), (true, None), (false, None)]);
        assert_eq!(navigate(Keys { home: true, ..Keys::default() }, Some(2), &r, false), Nav::Highlight(1));
        assert_eq!(navigate(Keys { end: true, ..Keys::default() }, Some(1), &r, false), Nav::Highlight(2));
    }

    #[test]
    fn enter_activates_the_highlighted_row_only() {
        let r = rows(&[(true, None), (false, None)]);
        let enter = Keys { enter: true, ..Keys::default() };
        assert_eq!(navigate(enter, Some(0), &r, false), Nav::Activate(0));
        assert_eq!(navigate(enter, None, &r, false), Nav::Nothing);
        assert_eq!(navigate(enter, Some(1), &r, false), Nav::Nothing);
    }

    #[test]
    fn escape_closes_at_the_top_and_goes_back_in_a_submenu() {
        let esc = Keys { escape: true, ..Keys::default() };
        assert_eq!(navigate(esc, Some(0), &[], false), Nav::Close);
        assert_eq!(navigate(esc, Some(0), &[], true), Nav::Back);
    }

    #[test]
    fn left_only_goes_back_from_a_submenu() {
        let left = Keys { back: true, ..Keys::default() };
        assert_eq!(navigate(left, Some(0), &[], false), Nav::Nothing);
        assert_eq!(navigate(left, Some(0), &[], true), Nav::Back);
    }

    #[test]
    fn right_forwards_the_highlighted_row() {
        let right = Keys { forward: true, ..Keys::default() };
        let r = rows(&[(true, None)]);
        assert_eq!(navigate(right, Some(0), &r, false), Nav::Forward(0));
        assert_eq!(navigate(right, None, &r, false), Nav::Nothing);
    }

    #[test]
    fn an_access_letter_activates_its_row_whatever_the_case() {
        let r = rows(&[(true, Some('o')), (true, Some('c'))]);
        let key = Keys { letter: Some('C'), ..Keys::default() };
        assert_eq!(navigate(key, None, &r, false), Nav::Activate(1));
        let none = Keys { letter: Some('x'), ..Keys::default() };
        assert_eq!(navigate(none, None, &r, false), Nav::Nothing);
    }

    #[test]
    fn access_letters_are_unique_per_menu_and_skip_unlabelled_rows() {
        let got = access(&[Some("Open"), None, Some("Copy"), Some("Open folder"), Some("...")]);
        assert_eq!(got, vec![Some((0, 'o')), None, Some((0, 'c')), Some((1, 'p')), None]);
    }

    #[test]
    fn access_letter_falls_through_to_a_later_letter_of_the_label() {
        // "Open" takes o, so "Other" takes its t (index 1); a label of only taken letters gets none.
        let got = access(&[Some("Open"), Some("Other"), Some("O")]);
        assert_eq!(got, vec![Some((0, 'o')), Some((1, 't')), None]);
    }
}

//! Keys for a list of widgets or a tab strip (`docs/design/keyboard.md`, sections 2 and 3):
//! arrows move the keyboard focus through the list, Home and End jump to its ends, and Shift
//! with an arrow tells the caller to extend a selection. Only the focused widget acts. The
//! decisions (`target`) are pure and tested here; the glue reads and writes egui's focus.

use egui::{Context, EventFilter, Id, Key, Modifiers, Response, Ui};

use crate::app::consume_exact;

/// The direction a list runs in, so the keys that move through it are the ones its arrows show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Up and Down (a column of rows).
    Vertical,
    /// Left and Right (a tab strip).
    Horizontal,
    /// All four (a vertical rail whose items are still one list).
    Both,
}

/// Where a list key moves the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Prev,
    Next,
    First,
    Last,
}

/// The index a move lands on from `at` in a list of `n` items. Clamped at both ends (no wrap),
/// so Up on the first row stays on it. `None` for an empty list.
pub fn target(at: usize, n: usize, mv: Move) -> Option<usize> {
    let last = n.checked_sub(1)?;
    let at = at.min(last);
    Some(match mv {
        Move::Prev => at.saturating_sub(1),
        Move::Next => (at + 1).min(last),
        Move::First => 0,
        Move::Last => last,
    })
}

/// Keeps the arrows for the focused widget: egui must not move the focus away from it on its own.
fn lock(ctx: &Context, id: Id) {
    let filter = EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false };
    ctx.memory_mut(|m| m.set_focus_lock_filter(id, filter));
}

/// The list key pressed this frame for `axis`, consumed, with whether Shift was held. Called
/// only by the focused widget `id`; it also keeps egui's own arrow navigation off that widget.
pub fn take(ctx: &Context, id: Id, axis: Axis) -> Option<(Move, bool)> {
    lock(ctx, id);
    let vertical = axis != Axis::Horizontal;
    let horizontal = axis != Axis::Vertical;
    let keys: Vec<(Key, Move)> = [
        (Key::ArrowUp, Move::Prev, vertical),
        (Key::ArrowLeft, Move::Prev, horizontal),
        (Key::ArrowDown, Move::Next, vertical),
        (Key::ArrowRight, Move::Next, horizontal),
        (Key::Home, Move::First, true),
        (Key::End, Move::Last, true),
    ]
    .into_iter()
    .filter(|(_, _, on)| *on)
    .map(|(k, m, _)| (k, m))
    .collect();
    ctx.input_mut(|i| {
        for (key, mv) in keys {
            if consume_exact(i, Modifiers::SHIFT, key) {
                return Some((mv, true));
            }
            if consume_exact(i, Modifiers::NONE, key) {
                return Some((mv, false));
            }
        }
        None
    })
}

/// Moves the focus from `id` through `order` (the widget ids of the list, in order) by the list
/// keys. Returns the new position in `order` and whether Shift was held, when the focus moved.
/// Does nothing unless `id` has the focus, so only the focused widget acts.
pub fn move_from(ctx: &Context, id: Id, order: &[Id], axis: Axis) -> Option<(usize, bool)> {
    if !ctx.memory(|m| m.has_focus(id)) {
        return None;
    }
    let (mv, shift) = take(ctx, id, axis)?;
    let at = order.iter().position(|x| *x == id)?;
    let to = target(at, order.len(), mv)?;
    let next = *order.get(to)?;
    ctx.memory_mut(|m| m.request_focus(next));
    ctx.data_mut(|d| d.insert_temp(follow_id(), true));
    Some((to, shift))
}

fn follow_id() -> Id {
    Id::new("list-keys-follow")
}

/// Scrolls the focused row into view, once, after the keys moved the focus to it.
pub fn follow(ui: &Ui, resp: &Response) {
    if resp.has_focus() && ui.ctx().data_mut(|d| d.remove_temp::<bool>(follow_id())).unwrap_or(false) {
        resp.scroll_to_me(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_step_and_clamp_at_both_ends() {
        assert_eq!(target(1, 4, Move::Next), Some(2));
        assert_eq!(target(3, 4, Move::Next), Some(3), "Down on the last row stays on it");
        assert_eq!(target(0, 4, Move::Prev), Some(0), "Up on the first row stays on it");
        assert_eq!(target(2, 4, Move::Prev), Some(1));
    }

    #[test]
    fn home_and_end_jump_to_the_ends() {
        assert_eq!(target(2, 5, Move::First), Some(0));
        assert_eq!(target(2, 5, Move::Last), Some(4));
    }

    #[test]
    fn an_empty_list_and_a_stale_position_are_safe() {
        assert_eq!(target(0, 0, Move::Next), None);
        assert_eq!(target(0, 0, Move::Last), None);
        // A position past the end (the list shrank) lands on the last item.
        assert_eq!(target(9, 3, Move::Next), Some(2));
        assert_eq!(target(9, 3, Move::Prev), Some(1));
        assert_eq!(target(0, 1, Move::Next), Some(0));
    }
}

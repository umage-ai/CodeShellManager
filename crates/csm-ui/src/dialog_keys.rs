//! Keys for the controls of a modal dialog (`docs/design/keyboard.md`, section 3): Tab and
//! Shift+Tab move through the controls in order, Enter presses the focused button (or the default
//! one when no control has the focus), Space toggles a focused checkbox, and Escape cancels.
//!
//! The order and the moves (`step`) are pure and tested here. `take` reads this frame's keys once,
//! before the dialog draws, and stores what it found; the dialog then asks `current` where the
//! focus is and `pressed` what was pressed. A key a control does not use (a letter typed into a
//! text field, Enter in a text field) is left alone, so the control still gets it.

use egui::{Context, Id, Key, Modifiers};

use crate::app::consume_exact;

/// What a dialog has besides its buttons. It comes first in the Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    None,
    /// A text input: Tab moves into and out of it, and the typed keys stay its own.
    Text,
    /// A checkbox: Space toggles it while it has the focus.
    Checkbox,
}

/// The controls of one dialog, in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    pub field: Field,
    /// How many buttons follow the field, in order.
    pub buttons: usize,
    /// The button Enter presses when no control has the focus. `None`: Enter is not a button.
    pub default: Option<usize>,
}

/// One control of a dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Field,
    Button(usize),
}

/// What a dialog's key pressed this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressed {
    Button(usize),
    Toggle,
    Cancel,
}

/// The index a Tab (`forward`) or Shift+Tab lands on from `at` in a list of `n` controls. It
/// wraps. With nothing focused, Tab goes to the first control and Shift+Tab to the last.
pub fn step(at: Option<usize>, n: usize, forward: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let at = at.filter(|p| *p < n);
    Some(match (at, forward) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(p), true) => (p + 1) % n,
        (Some(p), false) => (p + n - 1) % n,
    })
}

/// The controls in Tab order.
fn slots(c: &Controls) -> Vec<Slot> {
    let mut v = Vec::with_capacity(c.buttons + 1);
    if c.field != Field::None {
        v.push(Slot::Field);
    }
    v.extend((0..c.buttons).map(Slot::Button));
    v
}

/// The control a dialog starts on: a text input takes the focus when the dialog opens.
fn initial(c: &Controls) -> Option<Slot> {
    (c.field == Field::Text).then_some(Slot::Field)
}

fn focus_id(id: Id) -> Id {
    id.with("focus")
}

fn pressed_id(id: Id) -> Id {
    id.with("pressed")
}

fn opened_id(id: Id) -> Id {
    id.with("opened-at")
}

/// How long a dialog ignores Enter and clicks after it appears, so a key the person was already
/// pressing, or a click aimed at what was under it, never answers a question they have not read.
pub const ARM_DELAY: f64 = 0.4;

/// Marks dialog `id` as open (the first call starts its arm timer) and says whether `delay`
/// seconds have passed since. Escape (cancel) never waits; only the answers that do something.
pub fn armed(ctx: &Context, id: Id, delay: f64) -> bool {
    let now = std::time::Instant::now();
    let at = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(opened_id(id), || now));
    let waited = now.saturating_duration_since(at).as_secs_f64();
    if waited < delay {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((delay - waited).max(0.0) + 0.02));
        return false;
    }
    true
}

/// Drops the repeats of the keys a dialog answers with: holding Enter or Space must not press a
/// button again and again, or answer the next dialog.
fn drop_repeats(ctx: &Context) {
    ctx.input_mut(|i| {
        i.events.retain(|e| !matches!(e, egui::Event::Key { repeat: true, key: Key::Enter | Key::Escape | Key::Space | Key::Tab, .. }));
    });
}

/// Reads this frame's dialog keys for the dialog `id` with controls `c`. Tab and Shift+Tab move
/// the focus; Enter, Space and Escape return what they press (and are consumed). Returns the
/// press, which `pressed` also gives to the draw code.
pub fn take(ctx: &Context, id: Id, c: &Controls, delay: f64) -> Option<Pressed> {
    drop_repeats(ctx);
    let armed = armed(ctx, id, delay);
    let list = slots(c);
    let stored = ctx.data(|d| d.get_temp::<Option<Slot>>(focus_id(id))).unwrap_or_else(|| initial(c));
    let at = stored.and_then(|s| list.iter().position(|x| *x == s));
    let (tab, back) = ctx.input_mut(|i| (consume_exact(i, Modifiers::NONE, Key::Tab), consume_exact(i, Modifiers::SHIFT, Key::Tab)));
    let mut pressed = None;
    let mut now = stored;
    if tab || back {
        now = step(at, list.len(), tab).and_then(|p| list.get(p).copied());
    } else {
        let on = now.filter(|s| list.contains(s));
        let escape = ctx.input_mut(|i| consume_exact(i, Modifiers::NONE, Key::Escape));
        if escape {
            pressed = Some(Pressed::Cancel);
        } else if on == Some(Slot::Field) && c.field == Field::Checkbox && ctx.input_mut(|i| consume_exact(i, Modifiers::NONE, Key::Space)) {
            pressed = Some(Pressed::Toggle);
        } else {
            let target = match on {
                Some(Slot::Button(b)) => Some(b),
                None => c.default,
                Some(Slot::Field) => None,
            };
            if let Some(b) = target
                && ctx.input_mut(|i| consume_exact(i, Modifiers::NONE, Key::Enter))
            {
                // Consumed while the dialog is new, but it answers nothing.
                if armed {
                    pressed = Some(Pressed::Button(b));
                }
            }
        }
    }
    ctx.data_mut(|d| {
        d.insert_temp(focus_id(id), now);
        d.insert_temp(pressed_id(id), pressed);
    });
    pressed
}

/// The control with the keyboard focus in dialog `id` (`None`: no control has it).
pub fn current(ctx: &Context, id: Id) -> Option<Slot> {
    ctx.data(|d| d.get_temp::<Option<Slot>>(focus_id(id))).flatten()
}

/// What `take` pressed this frame in dialog `id`.
pub fn pressed(ctx: &Context, id: Id) -> Option<Pressed> {
    ctx.data(|d| d.get_temp::<Option<Pressed>>(pressed_id(id))).flatten()
}

/// Moves the keyboard focus of dialog `id` to `slot` (a click on a control does this).
pub fn set(ctx: &Context, id: Id, slot: Option<Slot>) {
    ctx.data_mut(|d| d.insert_temp(focus_id(id), slot));
}

/// Forgets what `take` pressed in dialog `id`. Used while a menu is open over it: the menu takes
/// the keys, so the dialog must not act on a press stored by an earlier frame.
pub fn clear_pressed(ctx: &Context, id: Id) {
    ctx.data_mut(|d| d.remove::<Option<Pressed>>(pressed_id(id)));
}

/// Forgets the focus of dialog `id`, so the next time it opens it starts again.
pub fn forget(ctx: &Context, id: Id) {
    ctx.data_mut(|d| {
        d.remove::<Option<Slot>>(focus_id(id));
        d.remove::<Option<Pressed>>(pressed_id(id));
        d.remove::<std::time::Instant>(opened_id(id));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_wraps_forward_and_back_over_the_controls() {
        assert_eq!(step(None, 3, true), Some(0), "the first Tab reaches the first control");
        assert_eq!(step(None, 3, false), Some(2), "the first Shift+Tab reaches the last control");
        assert_eq!(step(Some(0), 3, true), Some(1));
        assert_eq!(step(Some(2), 3, true), Some(0), "Tab wraps from the last control to the first");
        assert_eq!(step(Some(0), 3, false), Some(2), "Shift+Tab wraps from the first control to the last");
    }

    #[test]
    fn a_dialog_without_controls_has_nowhere_to_go() {
        assert_eq!(step(None, 0, true), None);
        assert_eq!(step(Some(0), 0, false), None);
    }

    #[test]
    fn a_stale_position_is_taken_as_no_position() {
        assert_eq!(step(Some(7), 3, true), Some(0), "a position past the end starts again");
    }

    #[test]
    fn the_slots_put_the_field_first() {
        let c = Controls { field: Field::Text, buttons: 2, default: None };
        assert_eq!(slots(&c), vec![Slot::Field, Slot::Button(0), Slot::Button(1)]);
        let c = Controls { field: Field::None, buttons: 3, default: Some(0) };
        assert_eq!(slots(&c), vec![Slot::Button(0), Slot::Button(1), Slot::Button(2)]);
    }

    #[test]
    fn a_text_prompt_starts_in_its_field_and_others_start_with_no_focus() {
        assert_eq!(initial(&Controls { field: Field::Text, buttons: 2, default: None }), Some(Slot::Field));
        assert_eq!(initial(&Controls { field: Field::Checkbox, buttons: 2, default: Some(0) }), None);
        assert_eq!(initial(&Controls { field: Field::None, buttons: 2, default: Some(0) }), None);
    }
}

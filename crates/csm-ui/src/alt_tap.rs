//! A clean Alt tap: Alt pressed and released with nothing else in between (`docs/design/keyboard.md`,
//! section 3). Alt is the terminal's meta prefix, so Alt+letter must reach a focused pane; only a
//! tap that stays a tap opens the main menu. Pure: the frame's input goes in, "tapped" comes out.

/// What the Alt key did in the frames so far. Feed it once per frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AltTap {
    /// Alt is down since an earlier frame.
    held: bool,
    /// Nothing but Alt has been pressed since Alt went down.
    clean: bool,
}

impl AltTap {
    /// One frame. `alt`: Alt is down now. `other`: a key, text, pointer button, scroll or another
    /// modifier (Ctrl, Shift, Command) was used this frame. `focused`: the window has the focus
    /// (losing it, e.g. Alt+Tab to another window, drops the tap). Returns `true` on the frame Alt
    /// goes up after a clean tap.
    pub fn update(&mut self, alt: bool, other: bool, focused: bool) -> bool {
        if !focused {
            *self = Self::default();
            return false;
        }
        if alt && !self.held {
            self.held = true;
            self.clean = true;
        }
        if self.held && other {
            self.clean = false;
        }
        if self.held && !alt {
            let tapped = self.clean;
            *self = Self::default();
            return tapped;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_tap_fires_on_release() {
        let mut t = AltTap::default();
        assert!(!t.update(true, false, true), "pressing the tap key does not open the menu");
        assert!(!t.update(true, false, true), "holding it does not either");
        assert!(t.update(false, false, true), "a tap fires on release when nothing else happened");
        assert!(!t.update(false, false, true), "and only once");
    }

    #[test]
    fn a_key_typed_while_alt_is_held_cancels_the_tap() {
        // Alt+F in the terminal: the F reaches the pty, Alt's release is not a tap.
        let mut t = AltTap::default();
        t.update(true, false, true);
        assert!(!t.update(true, true, true), "a key during the hold");
        assert!(!t.update(false, false, true), "the release after it is no tap");
    }

    #[test]
    fn a_key_in_the_same_frame_as_the_release_cancels_too() {
        let mut t = AltTap::default();
        t.update(true, false, true);
        assert!(!t.update(false, true, true));
    }

    #[test]
    fn a_key_pressed_in_the_frame_alt_goes_down_counts() {
        let mut t = AltTap::default();
        assert!(!t.update(true, true, true), "a key pressed with it in one frame");
        assert!(!t.update(false, false, true));
    }

    #[test]
    fn losing_the_focus_drops_the_tap() {
        // Alt+Tab switches windows: the release happens elsewhere.
        let mut t = AltTap::default();
        t.update(true, false, true);
        assert!(!t.update(true, false, false));
        assert!(!t.update(false, false, true), "a release after the focus came back is no tap");
    }

    #[test]
    fn a_tap_after_a_chord_starts_clean_again() {
        let mut t = AltTap::default();
        t.update(true, false, true);
        t.update(true, true, true);
        t.update(false, false, true);
        t.update(true, false, true);
        assert!(t.update(false, false, true), "the next tap counts");
    }
}

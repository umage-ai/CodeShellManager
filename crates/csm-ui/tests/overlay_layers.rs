//! A dialog's scrim must stay under its card. egui lifts an area to the top when it is pressed, so
//! a click on the scrim (which closes the dialog) used to leave the scrim above the card the next
//! time the dialog opened: a dark, unusable window.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::theme::ThemeKind;
use egui::{Event, Id, Modifiers, PointerButton, pos2};

type H = egui_kittest::Harness<'static, csm_ui::CsmApp>;

/// A point inside the dialog's card (it is centred, a fixed distance under the top).
const IN_CARD: egui::Pos2 = pos2(640.0, 130.0);
/// A point on the scrim, outside the card.
const ON_SCRIM: egui::Pos2 = pos2(12.0, 700.0);

fn layer_at(h: &H, at: egui::Pos2) -> Option<Id> {
    h.ctx.layer_id_at(at).map(|l| l.id)
}

fn click(h: &mut H, at: egui::Pos2) {
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
}

fn open(h: &mut H, a: Action) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, a);
    settle(h);
}

fn fresh() -> H {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

#[test]
fn the_new_session_dialog_stays_above_its_scrim_after_a_click_outside_closed_it() {
    let mut h = fresh();
    for round in 0..3 {
        open(&mut h, Action::OpenQuickOpen { group: None });
        assert_eq!(layer_at(&h, IN_CARD), Some(Id::new("quick-open")), "round {round}: the card is what a click in it reaches");
        assert_eq!(layer_at(&h, ON_SCRIM), Some(Id::new("quick-scrim")));
        click(&mut h, ON_SCRIM);
        settle(&mut h);
        assert_ne!(h.state().ui.overlay, csm_ui::app::Overlay::QuickOpen, "round {round}: the scrim click closed it");
    }
}

#[test]
fn the_palette_stays_above_its_scrim_after_a_click_outside_closed_it() {
    let mut h = fresh();
    for round in 0..3 {
        open(&mut h, Action::OpenPalette);
        assert_eq!(layer_at(&h, IN_CARD), Some(Id::new("palette")), "round {round}");
        click(&mut h, ON_SCRIM);
        settle(&mut h);
    }
}

#[test]
fn a_press_on_the_scrim_that_does_not_close_it_does_not_cover_the_dialog() {
    let mut h = fresh();
    open(&mut h, Action::OpenQuickOpen { group: None });
    h.event(Event::PointerButton { pos: ON_SCRIM, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.step();
    assert_eq!(layer_at(&h, IN_CARD), Some(Id::new("quick-open")));
}

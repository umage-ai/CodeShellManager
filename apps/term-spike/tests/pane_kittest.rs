//! egui_kittest: a real pane (cmd on a real ConPTY) driven through egui input.
//! Proves: the PTY is created with the grid measured on the first frame (the child sees that
//! width before anything else happens), click focuses, typed text + Enter reach the child, and a
//! resize reaches the PTY.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use csm_term::alacritty_terminal::grid::Dimensions;
use csm_term::alacritty_terminal::index::{Column, Line};
use csm_ui::terminal::fonts::{FontSet, default_paths, load_face};
use csm_ui::terminal::{TerminalRenderer, TerminalSource, lock};
use egui::{Event, Id, Key, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::Harness;
use term_spike::pane::{Pane, Placement};
use term_spike::view::{FrameCounters, Shared, pane_ui};

struct State {
    pane: Option<Pane>,
    shared: Shared,
}

fn fonts() -> FontSet {
    let (primary, _) = default_paths();
    let face = primary.iter().find_map(|p| load_face(p, 0).ok()).expect("a Windows monospace font");
    FontSet::new(face, Vec::new(), 13.0, true)
}

fn screen(pane: &Pane) -> String {
    let Some(term) = pane.term.terminal() else { return String::new() };
    let t = lock(&term);
    let grid = t.term().grid();
    let mut out = String::new();
    for row in 0..grid.screen_lines() {
        let line = &grid[Line(row as i32)];
        out.extend((0..grid.columns()).map(|c| line[Column(c)].c));
        out.push('\n');
    }
    out
}

fn pane(s: &State) -> &Pane {
    s.pane.as_ref().expect("pane created on the first frame")
}

fn step_until(h: &mut Harness<'_, State>, what: &str, timeout: Duration, mut done: impl FnMut(&State) -> bool) {
    let start = Instant::now();
    loop {
        h.step();
        if done(h.state()) {
            return;
        }
        assert!(start.elapsed() < timeout, "timed out waiting for {what}; screen:\n{}", screen(pane(h.state())));
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn type_line(h: &mut Harness<'_, State>, text: &str) {
    h.event(Event::Text(text.into()));
    h.key_press(Key::Enter);
    h.step();
}

#[test]
fn first_frame_size_focus_typing_and_resize() {
    let dir = std::env::temp_dir();
    let state = State { pane: None, shared: Shared { renderer: TerminalRenderer::new(fonts()), frame: FrameCounters::default() } };
    let mut h = Harness::builder().with_size(vec2(900.0, 420.0)).build_ui_state(
        move |ui, s: &mut State| {
            let rect = ui.max_rect();
            let pane = s.pane.get_or_insert_with(|| {
                Pane::new(0, "kittest".into(), vec!["cmd".into(), "/q".into(), "/k".into()], Some(dir.clone()), Placement::Grid, ui.ctx())
            });
            pane_ui(ui, pane, &mut s.shared, rect);
        },
        state,
    );

    // Frame 1 creates the terminal at the measured grid; there is no placeholder size.
    h.step();
    let first = pane(h.state()).term.first_grid().expect("grid measured on the first frame");
    assert!(first.cols > 50 && first.rows > 10, "implausible first grid {first:?}");
    step_until(&mut h, "the PTY", Duration::from_secs(15), |s| pane(s).term.pty().is_some() && pane(s).term.bytes_read() > 0);
    assert_eq!(pane(h.state()).view.grid(), Some(first), "grid changed without a resize");

    // Click to focus.
    let c = pos2(450.0, 220.0);
    h.event(Event::PointerMoved(c));
    h.event(Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    let id = Id::new(("csm-term-pane", 0usize));
    assert!(h.ctx.memory(|m| m.has_focus(id)), "click did not focus the pane");

    // The child sees the first-frame width before any resize.
    type_line(&mut h, "mode con");
    let want = format!("Columns:        {}", first.cols);
    step_until(&mut h, &want, Duration::from_secs(10), |s| screen(pane(s)).contains(&want));

    // Typed text and Enter reach the child.
    type_line(&mut h, "echo kittest-%COMPUTERNAME%-ok");
    let host = std::env::var("COMPUTERNAME").unwrap_or_default();
    let echoed = format!("kittest-{host}-ok");
    step_until(&mut h, "the echo", Duration::from_secs(10), |s| screen(pane(s)).contains(&echoed));

    // A resize reaches the terminal and the PTY.
    h.set_size(vec2(600.0, 300.0));
    h.step();
    h.step();
    let resized = pane(h.state()).view.grid().unwrap();
    assert!(resized.cols < first.cols && resized.rows < first.rows, "{resized:?} vs {first:?}");
    type_line(&mut h, "cls & mode con");
    let want = format!("Columns:        {}", resized.cols);
    step_until(&mut h, &want, Duration::from_secs(10), |s| screen(pane(s)).contains(&want));
}

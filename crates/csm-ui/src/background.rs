//! Close to tray (#149) and the work that must go on while the main window is hidden.
//!
//! With `CloseToTray` on (and a tray icon to come back from), closing the main window hides it
//! instead of quitting; sessions keep running. The tray's Show window (`window_show`) brings it
//! back; the tray's Quit, the menu's Quit and `app_quit` still quit through the shutdown board.
//!
//! **Why this module exists.** eframe runs no UI pass for a hidden window. Before this, the frame
//! polled the engine, decided OS notifications and closed the window on `QuitReady`; hidden, none
//! of that would run, so notifications would stop and a quit (from the tray or an MCP client)
//! would never finish. The approach:
//!
//! - That work is [`CsmApp::logic`], which the host calls from `eframe::App::logic`. eframe calls
//!   it before every UI pass *and* while the window is hidden, whenever a repaint was requested.
//!   [`CsmApp::frame`] runs it itself only when the host didn't this pass (the UI tests, which
//!   drive `frame` alone), so it runs once per pass either way.
//! - Requests keep coming while hidden: the engine backend's waker thread requests a repaint on
//!   every engine event and on terminal output, and while hidden [`CsmApp::logic`] also asks for
//!   one every [`HIDDEN_TICK`], so time-based decisions (the shutdown board's budget, the
//!   notifier's 5 s limit) move even when nothing else happens. On Windows eframe paints an
//!   invisible window on that schedule (it gets no `RedrawRequested`), which is what calls
//!   `logic`.
//! - egui's clock stands still while hidden (no pass, no new input time), so the notifier is
//!   given a clock that keeps running: the last pass's time plus the wall time since.
//! - A close request that arrives while hidden (Windows logoff or shutdown, `WM_CLOSE` from
//!   another program) is a real quit, never another hide, and it can't wait on the busy dialog
//!   nobody would see: `app_quit` runs straight away.

use std::time::{Duration, Instant};

use csm_core::notify_rules::{Attention, Notification};
use serde_json::json;

use crate::app::{Action, CsmApp};

/// How often the hidden window's logic runs when nothing else wakes it.
pub const HIDDEN_TICK: Duration = Duration::from_secs(1);

/// What closing the main window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    /// Hide it to the tray; sessions keep running.
    Hide,
    /// The usual quit: the busy warning, then `app_quit` and the shutdown board.
    Quit,
}

/// Hide only when the setting is on, there is a tray icon to come back from (none on Linux, and
/// none if the OS refused one), and no quit is already running.
pub fn close_choice(close_to_tray: bool, tray_available: bool, quitting: bool) -> CloseChoice {
    if close_to_tray && tray_available && !quitting { CloseChoice::Hide } else { CloseChoice::Quit }
}

/// The hidden state and the clock that runs while egui's doesn't.
#[derive(Debug, Default)]
pub struct Background {
    /// The host has a tray icon (set by the host; the UI tests set it to test the hide).
    pub tray_available: bool,
    /// The main window is hidden to the tray.
    pub hidden: bool,
    /// The pass the window was hidden in (a focused UI pass after it means it is shown again).
    hidden_at_pass: u64,
    /// The pass [`CsmApp::logic`] last ran in.
    logic_pass: Option<u64>,
    /// egui's time at the last UI pass, and when that was.
    clock: Option<(f64, Instant)>,
    /// "Still running in the tray" was said this run.
    told: bool,
}

impl Background {
    /// Seconds on egui's clock, carried on by the wall clock while no UI pass runs.
    fn now_s(&self, ctx: &egui::Context) -> f64 {
        match (self.hidden, self.clock) {
            (true, Some((t, at))) => t + at.elapsed().as_secs_f64(),
            _ => ctx.input(|i| i.time),
        }
    }
}

impl CsmApp {
    /// The work that must not stop while the window is hidden: poll the engine (replies, events,
    /// `QuitReady`), decide notifications, and handle a close request that arrives hidden. The
    /// host calls it from `eframe::App::logic`; see the module docs.
    pub fn logic(&mut self, ctx: &egui::Context) {
        self.background.logic_pass = Some(ctx.cumulative_pass_nr());
        self.poll(ctx);
        let now = self.background.now_s(ctx);
        self.watch_attention(ctx, now);
        // Here rather than in the frame, so a close that arrives while hidden is seen too.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_ready {
            self.close_requested(ctx);
        }
        if self.background.hidden {
            ctx.request_repaint_after(HIDDEN_TICK);
        }
    }

    /// Start of [`CsmApp::frame`]: the logic, unless the host already ran it this pass, and the
    /// clock and hidden state a UI pass tells us about.
    pub(crate) fn logic_in_frame(&mut self, ctx: &egui::Context) {
        let pass = ctx.cumulative_pass_nr();
        let b = &mut self.background;
        b.clock = Some((ctx.input(|i| i.time), Instant::now()));
        // Hidden, eframe runs no UI pass; a later one with the window focused means something
        // showed it without `window_show` (`csm open` raising it over IPC).
        if b.hidden && pass > b.hidden_at_pass + 1 && ctx.input(|i| i.viewport().focused == Some(true)) {
            b.hidden = false;
        }
        if self.background.logic_pass != Some(pass) {
            self.logic(ctx);
        }
    }

    /// The main window's close button (or Alt+F4): hide to the tray or ask to quit. A close
    /// that arrives while hidden (logoff, another program's `WM_CLOSE`) quits at once.
    fn close_requested(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        if self.background.hidden {
            self.flush_placements(ctx);
            self.run(ctx, Action::Tool("app_quit", json!({})));
            return;
        }
        match close_choice(self.prefs.close_to_tray, self.background.tray_available, self.model.shutdown.is_some()) {
            CloseChoice::Hide => self.hide_to_tray(ctx),
            CloseChoice::Quit => self.run(ctx, Action::AskQuit),
        }
    }

    /// Hides the main window; sessions keep running. The first time in a run, one notification
    /// says where the app went.
    pub fn hide_to_tray(&mut self, ctx: &egui::Context) {
        self.run(ctx, Action::CloseMenu);
        self.flush_placements(ctx);
        let b = &mut self.background;
        b.hidden = true;
        b.hidden_at_pass = ctx.cumulative_pass_nr();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        ctx.request_repaint_after(HIDDEN_TICK);
        if !std::mem::replace(&mut self.background.told, true) {
            self.queue_notification(Notification {
                session_id: String::new(),
                kind: Attention::Done,
                title: "CodeShellManager is still running".into(),
                body: "Your sessions keep working. Click the tray icon to show the window; its menu has Quit.".into(),
                sound: false,
            });
        }
    }

    /// The window was shown again (`window_show`, a notification click, the tray).
    pub(crate) fn shown(&mut self) {
        self.background.hidden = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_hides_only_with_the_setting_a_tray_and_no_quit_running() {
        assert_eq!(close_choice(true, true, false), CloseChoice::Hide);
        assert_eq!(close_choice(false, true, false), CloseChoice::Quit, "off by default: closing quits");
        assert_eq!(close_choice(true, false, false), CloseChoice::Quit, "no tray to come back from");
        assert_eq!(close_choice(true, true, true), CloseChoice::Quit, "already quitting");
    }

    #[test]
    fn the_clock_keeps_running_while_hidden() {
        let ctx = egui::Context::default();
        let mut b = Background { clock: Some((10.0, Instant::now() - Duration::from_secs(3))), ..Background::default() };
        assert!(b.now_s(&ctx) < 1.0, "shown: egui's own clock");
        b.hidden = true;
        let t = b.now_s(&ctx);
        assert!((13.0..14.0).contains(&t), "{t}");
    }
}

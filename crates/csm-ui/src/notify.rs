//! OS notifications and the tray's count. The UI decides, once a frame, from the snapshot it
//! already has ([`csm_core::notify_rules`]: transitions, focus, the settings, the 5 s limit);
//! the host takes what was decided ([`CsmApp::take_notifications`]) and sends it from a worker,
//! so nothing here touches the OS.

use csm_core::notify_rules::{self, Attention, Notification, NotifyContext, Notifier, Observation};

use crate::app::CsmApp;
use crate::model::Status;

/// Notifications not yet taken by the host are dropped beyond this (a host without
/// notifications, the UI tests).
const OUTBOX_MAX: usize = 32;

#[derive(Debug, Default)]
pub struct NotifyState {
    notifier: Notifier,
    outbox: Vec<Notification>,
}

/// What a session's status means to the notifier.
pub fn attention(s: Status) -> Attention {
    match s {
        Status::Approval => Attention::Approval,
        Status::Question => Attention::Question,
        Status::Done => Attention::Done,
        Status::Error => Attention::Error,
        Status::Starting | Status::Working | Status::Idle | Status::Asleep => Attention::Quiet,
    }
}

/// Brings the main window to the front, restored if minimized (`window_show`).
pub fn show_window(ctx: &egui::Context) {
    let root = egui::ViewportId::ROOT;
    ctx.send_viewport_cmd_to(root, egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd_to(root, egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd_to(root, egui::ViewportCommand::Focus);
    ctx.request_repaint_of(root);
}

/// The person is looking at the app: one of its windows has the keyboard focus and the main one
/// is not minimized.
fn window_in_front(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        let minimized = i.raw.viewports.get(&egui::ViewportId::ROOT).and_then(|v| v.minimized).unwrap_or(false);
        let focused = i.raw.viewports.values().any(|v| v.focused == Some(true)) || i.viewport().focused == Some(true);
        focused && !minimized
    })
}

impl CsmApp {
    /// Feeds this frame's statuses to the notifier; what it decides waits for the host.
    pub(crate) fn watch_attention(&mut self, ctx: &egui::Context) {
        let model = self.model.clone();
        let obs: Vec<Observation<'_>> = model
            .sessions
            .iter()
            .map(|s| Observation { session_id: &s.id, name: &s.name, attention: attention(s.status), detail: &s.detail })
            .collect();
        let cx = NotifyContext {
            enabled: self.prefs.notifications,
            sound: self.prefs.notification_sound,
            window_focused: window_in_front(ctx),
            focused_session: model.active.as_deref(),
            now_ms: (ctx.input(|i| i.time) * 1000.0) as i64,
        };
        let out = self.ui.notify.notifier.observe(&obs, &cx);
        self.queue(out);
    }

    /// `notification_test`: a sample, whatever the settings say (the sound follows its setting).
    pub(crate) fn test_notification(&mut self) {
        let n = Notification {
            session_id: String::new(),
            kind: Attention::Done,
            title: notify_rules::title_for("Test"),
            body: "Notifications reach you. Sessions that need you will show up like this.".into(),
            sound: self.prefs.notification_sound,
        };
        self.queue(vec![n]);
    }

    fn queue(&mut self, out: Vec<Notification>) {
        let q = &mut self.ui.notify.outbox;
        q.extend(out);
        if q.len() > OUTBOX_MAX {
            let extra = q.len() - OUTBOX_MAX;
            q.drain(..extra);
        }
    }

    /// The notifications decided since the last call, oldest first. The host sends them (on a
    /// worker: sending may block); a click on one should focus its `session_id` (empty for the
    /// test).
    pub fn take_notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.ui.notify.outbox)
    }

    /// Sessions waiting on an approval or a question (the tray's tooltip).
    pub fn needs_you_count(&self) -> usize {
        self.model.sessions.iter().filter(|s| s.status.needs_you()).count()
    }
}

/// The tray icon: the window's logo, with a "needs you" dot in the top-right corner when
/// `badge` (the approval colour, ringed in the logo's ink so it reads on any taskbar).
pub fn tray_icon(side: u32, badge: bool) -> egui::IconData {
    let mut icon = crate::app::app_icon(side);
    if !badge {
        return icon;
    }
    let t = crate::theme::Tokens::for_kind(crate::theme::ThemeKind::Mocha);
    let (dot, ring) = (t.st_approval, t.on_accent);
    let side = icon.width.min(icon.height);
    let n = side as f32;
    let r = n * 0.26;
    let (cx, cy) = (n - r, r);
    for y in 0..side {
        for x in 0..side {
            let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
            let cover = (r + 0.5 - d).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let inner = (r - n * 0.06 + 0.5 - d).clamp(0.0, 1.0);
            let i = ((y * icon.width + x) * 4) as usize;
            let Some(px) = icon.rgba.get_mut(i..i + 4) else { continue };
            let c = [dot.r(), dot.g(), dot.b()];
            let k = [ring.r(), ring.g(), ring.b()];
            for ch in 0..3 {
                let badge_c = f32::from(k[ch]) * (1.0 - inner) + f32::from(c[ch]) * inner;
                px[ch] = (f32::from(px[ch]) * (1.0 - cover) + badge_c * cover).round() as u8;
            }
            px[3] = (f32::from(px[3]) * (1.0 - cover) + 255.0 * cover).round() as u8;
        }
    }
    icon
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_badge_is_a_dot_in_the_corner() {
        let plain = tray_icon(32, false);
        let badged = tray_icon(32, true);
        assert_eq!(plain.rgba.len(), badged.rgba.len());
        let px = |icon: &egui::IconData, x: u32, y: u32| icon.rgba[((y * 32 + x) * 4) as usize..][..4].to_vec();
        // Bottom-left is untouched; the dot's centre is the approval colour, opaque.
        assert_eq!(px(&plain, 8, 24), px(&badged, 8, 24));
        let t = crate::theme::Tokens::for_kind(crate::theme::ThemeKind::Mocha);
        let c = t.st_approval;
        assert_eq!(px(&badged, 32 - 9, 8), vec![c.r(), c.g(), c.b(), 255]);
    }

    #[test]
    fn every_status_has_an_attention() {
        for s in Status::ALL {
            let a = attention(s);
            assert_eq!(a.needs_you(), s.needs_you(), "{s:?}");
            assert_eq!(matches!(a, Attention::Done | Attention::Error), s.unseen(), "{s:?}");
        }
    }
}

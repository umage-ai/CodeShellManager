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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_has_an_attention() {
        for s in Status::ALL {
            let a = attention(s);
            assert_eq!(a.needs_you(), s.needs_you(), "{s:?}");
            assert_eq!(matches!(a, Attention::Done | Attention::Error), s.unseen(), "{s:?}");
        }
    }
}

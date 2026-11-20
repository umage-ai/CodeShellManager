//! Sends the OS notifications the UI decided ([`csm_ui::CsmApp::take_notifications`]) from one
//! worker thread: showing one can block (WinRT, D-Bus, the macOS notification centre), and the
//! frame loop never waits on it. A click on a notification (Windows, while the app runs) comes
//! back as the session's id on a channel, and wakes the window.
//!
//! - Windows: a WinRT toast (as the WPF app's tray balloon, which Windows shows as a toast). An
//!   unpackaged app has no AppUserModelID of its own, so the toast is sent under PowerShell's
//!   (the only id Windows shows toasts for without a Start-menu shortcut): until an installer
//!   registers one, the toast's header reads "Windows PowerShell". The sound is the system's
//!   default notification sound, or none.
//! - macOS and Linux: `notify-rust` (the notification centre; the freedesktop D-Bus service).
//!   No click-through.

use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

use csm_core::notify_rules::Notification;

/// Wakes the window (a click came back).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

pub struct OsNotifier {
    tx: Option<Sender<Notification>>,
}

impl OsNotifier {
    /// Starts the worker. `clicked` receives the session id of a notification the person
    /// clicked (never an empty id).
    pub fn start(clicked: Sender<String>, wake: Wake) -> OsNotifier {
        let (tx, rx) = mpsc::channel::<Notification>();
        let worker = std::thread::Builder::new().name("os-notify".into()).spawn(move || {
            for n in rx {
                show(&n, &clicked, &wake);
            }
        });
        match worker {
            Ok(_) => OsNotifier { tx: Some(tx) },
            Err(e) => {
                log::warn!("notifications: no worker thread ({e}); none will be shown");
                OsNotifier { tx: None }
            }
        }
    }

    /// Queues one; never blocks.
    pub fn send(&self, n: Notification) {
        if let Some(tx) = &self.tx
            && tx.send(n).is_err()
        {
            log::warn!("notifications: the worker is gone");
        }
    }
}

#[cfg(windows)]
fn show(n: &Notification, clicked: &Sender<String>, wake: &Wake) {
    use tauri_winrt_notification::{Sound, Toast};
    let mut toast = Toast::new(Toast::POWERSHELL_APP_ID)
        .title(&n.title)
        .text1(&n.body)
        .sound(if n.sound { Some(Sound::Default) } else { None });
    if !n.session_id.is_empty() {
        let (id, tx, wake) = (n.session_id.clone(), clicked.clone(), wake.clone());
        toast = toast.on_activated(move |_action| {
            let _ = tx.send(id.clone());
            wake();
            Ok(())
        });
    }
    if let Err(e) = toast.show() {
        log::warn!("notifications: the toast could not be shown: {e}");
    }
}

#[cfg(not(windows))]
fn show(n: &Notification, _clicked: &Sender<String>, _wake: &Wake) {
    let mut note = notify_rust::Notification::new();
    note.appname(csm_core::notify_rules::APP_NAME).summary(&n.title).body(&n.body);
    if n.sound {
        // The platform's own default: the notification centre's, the desktop theme's message sound.
        note.sound_name(if cfg!(target_os = "macos") { "Default" } else { "message-new-instant" });
    }
    if let Err(e) = note.show() {
        log::warn!("notifications: could not be shown: {e}");
    }
}

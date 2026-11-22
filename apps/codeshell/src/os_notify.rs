//! Sends the OS notifications the UI decided ([`csm_ui::CsmApp::take_notifications`]) from one
//! worker thread: showing one can block (WinRT, D-Bus, the macOS notification centre), and the
//! frame loop never waits on it. A click on a notification (Windows, while the app runs) comes
//! back as the session's id on a channel, and wakes the window.
//!
//! - Windows: a WinRT toast (as the WPF app's tray balloon, which Windows shows as a toast).
//!   Windows shows a toast only for an AppUserModelID that a Start-menu shortcut carries. The MSI
//!   installs that shortcut and, beside the exe, `codeshell.aumid` holding the same id; when the
//!   file is there the toast is sent under it (header: the shortcut's name, "CodeShellManager",
//!   or "CodeShellManager Beta" for a beta install; docs/release.md). A portable or
//!   dev build has no shortcut, so it falls back to PowerShell's id and the header reads
//!   "Windows PowerShell". The sound is the system's default notification sound, or none.
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
            // Read on the worker, never on the UI thread (a file read).
            let app_id = installed_app_id();
            for n in rx {
                show(&n, app_id.as_deref(), &clicked, &wake);
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

/// The file the MSI installs beside the exe, holding the AppUserModelID its shortcut carries.
const AUMID_FILE: &str = "codeshell.aumid";

/// The installed app's AppUserModelID, if this exe was installed by the MSI.
fn installed_app_id() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let text = std::fs::read_to_string(exe.parent()?.join(AUMID_FILE)).ok()?;
    let id = parse_app_id(&text);
    if id.is_none() {
        log::warn!("notifications: {AUMID_FILE} does not hold a usable app id; using PowerShell's");
    }
    id
}

/// An AppUserModelID from the file's text: one line, at most 128 characters of letters, digits,
/// `.`, `-` and `_` (what an MSI shortcut can carry). Anything else is ignored.
fn parse_app_id(text: &str) -> Option<String> {
    let id = text.trim();
    let ok = !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    ok.then(|| id.to_string())
}

#[cfg(windows)]
fn show(n: &Notification, app_id: Option<&str>, clicked: &Sender<String>, wake: &Wake) {
    use tauri_winrt_notification::{Sound, Toast};
    let mut toast = Toast::new(app_id.unwrap_or(Toast::POWERSHELL_APP_ID)).title(&n.title).text1(&n.body).sound(if n.sound {
        Some(Sound::Default)
    } else {
        None
    });
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
fn show(n: &Notification, _app_id: Option<&str>, _clicked: &Sender<String>, _wake: &Wake) {
    let mut note = notify_rust::Notification::new();
    // A freedesktop server may render the body's markup; the macOS centre shows text as is.
    let body = if cfg!(target_os = "macos") { n.body.clone() } else { csm_core::notify_rules::escape_markup(&n.body) };
    note.appname(csm_core::notify_rules::APP_NAME).summary(&n.title).body(&body);
    if n.sound {
        // The platform's own default: the notification centre's, the desktop theme's message sound.
        note.sound_name(if cfg!(target_os = "macos") { "Default" } else { "message-new-instant" });
    }
    if let Err(e) = note.show() {
        log::warn!("notifications: could not be shown: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::parse_app_id;

    #[test]
    fn the_installed_app_id_is_one_plain_line_or_nothing() {
        assert_eq!(parse_app_id("UmageAI.CodeShellManager.Beta\r\n").as_deref(), Some("UmageAI.CodeShellManager.Beta"));
        assert_eq!(parse_app_id("  "), None);
        assert_eq!(parse_app_id("two\nlines"), None);
        assert_eq!(parse_app_id("has space"), None);
        assert_eq!(parse_app_id(&"a".repeat(129)), None);
    }
}

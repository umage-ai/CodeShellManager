//! Events as they travel: the engine's `Event`, with owned fields. The tag and the field names
//! are the engine's, so the JSON is the same on both sides.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Additive: new variants may be added; consumers must ignore ones they don't know.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// A session was created, changed (status, name, branch, group…) or removed.
    SessionChanged {
        session_id: String,
    },
    SessionRemoved {
        session_id: String,
    },
    /// Groups, order, view or layout changed.
    LayoutChanged,
    /// The "Needs you" inbox changed.
    InboxChanged,
    /// The UI should focus this session.
    FocusRequested {
        session_id: String,
    },
    /// A short message for a toast.
    Toast {
        message: String,
    },
    /// An MCP client wants to run a tool that needs consent; show a card.
    ConsentRequested {
        request: Value,
    },
    ConsentResolved {
        request_id: u64,
    },
    Error {
        message: String,
    },
    /// A row of the shutdown board changed: `phase` is `closing`, `clean` or `forced`.
    QuitProgress {
        session_id: String,
        phase: String,
        elapsed_ms: u64,
    },
    /// `app_quit` finished: every process is gone and the state is saved.
    QuitReady,
    /// The window should come to the front, restored if minimized.
    WindowShowRequested,
    /// Send a sample OS notification now.
    NotificationTestRequested,
    /// A session set the clipboard (OSC 52). The text is not in the event.
    ClipboardOffered {
        session_id: String,
    },
    /// An add-on screen has a new tree, phase or window state.
    ScreenChanged {
        instance: String,
    },
    /// The UI should show this add-on screen.
    ScreenFocusRequested {
        instance: String,
    },
    /// A setting was saved, from any caller.
    SettingChanged {
        key: String,
        value: Value,
    },
}

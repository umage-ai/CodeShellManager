//! Events the engine broadcasts. The UI repaints from them; the IPC bridge forwards them to
//! connected clients (an MCP client can watch for "needs you" without polling).

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use serde_json::Value;

/// Additive: new variants may be added; consumers must ignore ones they don't know.
#[derive(Debug, Clone, PartialEq, Serialize)]
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
    /// The UI should focus this session (raised by `session_focus`, `inbox_next`, …).
    FocusRequested {
        session_id: String,
    },
    /// A short message for a toast ("Started claude in Odulodo.").
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
    /// A row of the shutdown board changed: `phase` is `closing`, `clean` (exited by itself in
    /// time) or `forced` (killed: the budget ran out, or it is not a Claude session).
    QuitProgress {
        session_id: String,
        phase: String,
        elapsed_ms: u64,
    },
    /// `app_quit` finished: every process is gone and the state is saved. Call
    /// [`crate::Engine::shutdown`] and exit.
    QuitReady,
    /// The window should come to the front, restored if minimized (`window_show`).
    WindowShowRequested,
    /// Send a sample OS notification now, whatever the notification settings say
    /// (`notification_test`).
    NotificationTestRequested,
}

/// Fan-out to any number of subscribers; a dropped receiver is pruned on the next emit.
#[derive(Clone, Default)]
pub struct Events {
    subs: Arc<Mutex<Vec<Sender<Event>>>>,
}

impl Events {
    pub fn subscribe(&self) -> Receiver<Event> {
        let (tx, rx) = mpsc::channel();
        self.subs.lock().unwrap_or_else(PoisonError::into_inner).push(tx);
        rx
    }

    pub fn emit(&self, event: Event) {
        let mut subs = self.subs.lock().unwrap_or_else(PoisonError::into_inner);
        subs.retain(|s| s.send(event.clone()).is_ok());
    }

    pub fn toast(&self, message: impl Into<String>) {
        self.emit(Event::Toast { message: message.into() });
    }
}

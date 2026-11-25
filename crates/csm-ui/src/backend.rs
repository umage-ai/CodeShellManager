//! The seam between the screens and whatever owns the state.
//!
//! - Screens read a [`UiModel`] snapshot and draw [`TerminalSource`]s. They never mutate the
//!   model: every action goes through [`ToolPort::submit`] (for the engine, [`EnginePort`]:
//!   `engine.submit(Caller::Ui, "<tool_id>", params)`), and the replies are polled once a frame.
//!   The port also lists the tools the palette, menus and settings show.
//! - Widget-level input (keys into the focused pane, scroll, selection, resize) goes straight to
//!   the session's [`TerminalSource`]; it is not a tool.
//!
//! # The two backends
//!
//! - [`crate::EngineBackend`] (the real app): `model()` maps `engine.view()`, `terminal()` adapts
//!   the engine's `TerminalHandle`, `on_reply` only reports (the engine applied the effect),
//!   `on_event` turns `FocusRequested`, `Toast`, `QuitProgress` and `QuitReady` into notices.
//! - [`crate::demo::DemoBackend`] (`codeshell --demo`, screenshots, UI tests): the mock-up's data,
//!   applying each call's effect to its own model.
//!
//! Ephemeral UI state that is deliberately not a tool: the palette/quick-open query text, open
//! menus, an in-progress divider drag (the drag's end *is* a tool call: `layout_set` /
//! `inbox_panel_set`).

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use csm_engine::{Caller, Effects, Engine, Event, Registry, Reply, ToolSpec, Toolset};
use serde_json::Value;

use crate::model::{SessionId, UiModel};
use crate::terminal::TerminalSource;

/// One tool call the UI made.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: String,
    pub params: Value,
}

/// One tool as the UI sees it: everything in a [`ToolSpec`] except the function. Owned, so a port
/// can hand it out without borrowing from the engine (or from a connection).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub toolset: Toolset,
    pub effects: Effects,
    pub input_schema: Value,
}

impl ToolInfo {
    /// The owned view of a spec, with the input schema the registry gives it (a dynamic tool's
    /// own schema, else the spec's).
    pub fn from_spec(spec: &ToolSpec, input_schema: Value) -> ToolInfo {
        ToolInfo {
            id: spec.id.to_string(),
            title: spec.title.to_string(),
            summary: spec.summary.to_string(),
            toolset: spec.toolset,
            effects: spec.effects,
            input_schema,
        }
    }
}

/// Where the UI's tool calls go, and where its tool list comes from. The engine is one port
/// ([`EnginePort`]); a remote client can be another. Nothing here blocks.
pub trait ToolPort {
    /// Submits `id` with `params`; the reply is picked up by [`ToolPort::poll`].
    fn submit(&mut self, id: &str, params: Value);
    /// Finished calls and new events since the last poll.
    fn poll(&mut self) -> (Vec<(Call, Reply)>, Vec<Event>);
    /// Whether replies are still outstanding (the frame loop keeps polling while they are).
    fn busy(&self) -> bool;
    /// Calls submitted so far (most recent last), for tests and diagnostics.
    fn submitted(&self) -> &[Call];
    /// Every tool the port offers now, in registration order (built-in tools first). Shared, so
    /// holding it costs an `Arc` clone.
    fn tools(&self) -> Arc<[ToolInfo]>;
    /// One tool by id (a copy).
    fn tool(&self, id: &str) -> Option<ToolInfo> {
        self.tools().iter().find(|t| t.id == id).cloned()
    }
    /// Whether the port offers a tool with this id.
    fn has_tool(&self, id: &str) -> bool {
        self.tools().iter().any(|t| t.id == id)
    }
    /// The effects a tool declares (`None` when the port does not offer it).
    fn effects(&self, id: &str) -> Option<Effects> {
        self.tools().iter().find(|t| t.id == id).map(|t| t.effects)
    }
    /// The engine behind the port, for tests and diagnostics. A remote port has none.
    fn engine(&self) -> Option<Engine> {
        None
    }
}

/// Something the backend wants the UI to show after a reply or an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Toast(String),
    /// Bring this session into view (switch rail tab, focus its pane).
    Focus(SessionId),
    /// Show this add-on screen in the view strip (`screen_open`).
    FocusScreen(String),
    /// `app_quit` finished (every process is gone, the state is saved): close the window.
    QuitReady,
    /// A session set the clipboard (OSC 52; the setting allows it): put the text on it.
    Clipboard(String),
}

/// What owns the state the screens draw. See the module docs for the real-engine mapping.
pub trait Backend {
    /// The snapshot to draw this frame. Cheap: an `Arc` clone.
    fn model(&self) -> Arc<UiModel>;
    /// The terminal a pane shows.
    fn terminal(&self, id: &str) -> Option<Arc<dyn TerminalSource>>;
    /// A call finished. The demo applies its effect here; the real engine already has. `port` is
    /// the port the call went through, for the tool facts a reply depends on (its effects).
    fn on_reply(&mut self, call: &Call, reply: &Reply, port: &dyn ToolPort) -> Vec<Notice>;
    /// An engine event.
    fn on_event(&mut self, event: &Event) -> Vec<Notice>;
    /// Once a frame, before drawing (timers, repaint scheduling).
    fn tick(&mut self, _ctx: &egui::Context) -> Vec<Notice> {
        Vec::new()
    }
    /// Ids of the sessions a quit would cut off (running and really busy), when the backend can
    /// tell. `None` makes the quit prompt fall back to the displayed statuses.
    fn busy_sessions(&self) -> Option<Vec<String>> {
        None
    }
    /// The add-on screens open now (the view strip's tabs). Empty without a screens-capable
    /// backend.
    fn screens(&self) -> Vec<csm_engine::ScreenInfo> {
        Vec::new()
    }
    /// The screens running add-ons offer (the palette's "Open ..." rows).
    fn screen_declared(&self) -> Vec<csm_engine::DeclaredScreen> {
        Vec::new()
    }
    /// The validated view tree of an open screen: an `Arc` clone, never waits for the add-on.
    fn screen_tree(&self, _instance: &str) -> Option<Arc<csm_core::addons::screen::ScreenTree>> {
        None
    }
    /// A widget in an add-on screen raised an event for its plugin. Widget-level input like a
    /// keystroke into a pane, so not a tool. Never blocks; `Err` is a message to show.
    fn screen_event(&mut self, _instance: &str, _event: csm_core::addons::screen::ScreenEvent) -> Result<(), String> {
        Ok(())
    }
    /// The live output of a run command's last run (the Runs tab reads its tail when the
    /// generation changes). `None`: the model's `RunView::output` is all there is (the demo).
    fn run_output(&self, _session: &str, _run: &str) -> Option<csm_engine::RunOutputHandle> {
        None
    }
    /// The host and link of a remote window; `None` for a window with its own engine. Cheap.
    fn remote(&self) -> Option<crate::remote_window::RemoteStatus> {
        None
    }
    /// The panes this frame drew (every session with a visible pane, floating or torn off), and
    /// the session that has focus (`driver`: the pane that types, for a remote host's PTY size).
    /// Called once a frame, after the panes are drawn. A remote window subscribes the terminals
    /// shown here and drops the others; the engine has nothing to do.
    fn show_panes(&mut self, _visible: &[String], _driver: Option<&str>) {}
    /// Starts uploading local files into the folder of `session` on the host. Only a remote window
    /// has a host; the rest refuse. Returns the drop's id at once: the work (reading the files,
    /// sending them) runs on the link's thread, and its progress comes back through
    /// [`Backend::upload_events`]. Nothing is pasted here: the UI pastes when the drop is done.
    fn upload_files(&mut self, _session: &str, _paths: Vec<String>) -> Result<u64, String> {
        Err("Uploads go to a remote window's office; this window has none.".into())
    }
    /// Starts uploading the image on the laptop's clipboard, as a file, into the folder of
    /// `session` (remote windows only). The clipboard is read off the UI thread. A clipboard with
    /// no image reports as a failed drop.
    fn upload_clipboard_image(&mut self, _session: &str) -> Result<u64, String> {
        Err("Pasting an image needs a remote window.".into())
    }
    /// Cancels a drop: its partial files are deleted on the host and nothing is pasted.
    fn upload_cancel(&mut self, _drop: u64) {}
    /// What happened to the uploads since the last call, in order. Called once a frame.
    fn upload_events(&mut self) -> Vec<UploadEvent> {
        Vec::new()
    }
}

/// What an upload is doing, as the UI shows it. A drop is one or more files sent to one session's
/// folder on the host; the paste into the session is the UI's job once [`UploadEvent::Done`] comes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadEvent {
    /// Bytes written on the host so far, across the drop. `file` is the file now being sent.
    Progress { drop: u64, sent: u64, total: u64, files_done: usize, files_total: usize, file: String },
    /// Every file is in place on the host, in drop order. The UI pastes `paths` into `session`.
    Done { drop: u64, session: SessionId, paths: Vec<String> },
    /// Nothing was kept: `message` says why, in words for the person.
    Failed { drop: u64, message: String },
    /// Cancelled by the person; the host deleted the partial files.
    Cancelled { drop: u64 },
}

/// The engine as a [`ToolPort`]: submits to the engine as `Caller::Ui`, and polls the replies and
/// the engine's events. Never blocks.
pub struct EnginePort {
    engine: Engine,
    events: Receiver<Event>,
    pending: Vec<(Call, Receiver<Reply>)>,
    log: Vec<Call>,
    /// The last tool list and the registry it was built from. Rebuilt only when the engine swaps
    /// its registry (add-on tools came or went). Only the UI thread reads it.
    tools: RefCell<ToolCache>,
}

/// The registry a tool list was built from, and the list.
type ToolCache = Option<(Arc<Registry>, Arc<[ToolInfo]>)>;

/// How many submitted calls are remembered for [`ToolPort::submitted`].
const LOG_MAX: usize = 256;

impl EnginePort {
    pub fn new(engine: Engine) -> Self {
        let events = engine.subscribe();
        Self { engine, events, pending: Vec::new(), log: Vec::new(), tools: RefCell::new(None) }
    }
}

/// The params as they may be logged or kept in the call history: a secret parameter is replaced
/// with `"<redacted>"`. The engine itself always gets the real params.
pub fn redacted_params(id: &str, params: &Value) -> Value {
    let mut shown = params.clone();
    if id == "peer_relay_set"
        && let Some(obj) = shown.as_object_mut()
        && obj.contains_key("access")
    {
        obj.insert("access".to_owned(), Value::String("<redacted>".to_owned()));
    }
    shown
}

impl ToolPort for EnginePort {
    /// `engine.submit(Caller::Ui, id, params)`; the reply is picked up by [`ToolPort::poll`].
    fn submit(&mut self, id: &str, params: Value) {
        // The log, the call history and the debug line get the redacted params; the engine gets the real ones.
        let shown = redacted_params(id, &params);
        log::debug!("ui → {id} {shown}");
        let call = Call { id: id.to_string(), params: shown };
        let rx = self.engine.submit(Caller::Ui, &call.id, params);
        if self.log.len() >= LOG_MAX {
            self.log.remove(0);
        }
        self.log.push(call.clone());
        self.pending.push((call, rx));
    }

    fn poll(&mut self) -> (Vec<(Call, Reply)>, Vec<Event>) {
        let mut done = Vec::new();
        let mut keep = Vec::new();
        for (call, rx) in self.pending.drain(..) {
            match rx.try_recv() {
                Ok(reply) => done.push((call, reply)),
                Err(TryRecvError::Empty) => keep.push((call, rx)),
                Err(TryRecvError::Disconnected) => {
                    done.push((call, Err(csm_engine::ToolError::Failed("the engine stopped".into()))));
                }
            }
        }
        self.pending = keep;
        let mut events = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            events.push(e);
        }
        (done, events)
    }

    fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    fn submitted(&self) -> &[Call] {
        &self.log
    }

    fn engine(&self) -> Option<Engine> {
        Some(self.engine.clone())
    }

    fn tools(&self) -> Arc<[ToolInfo]> {
        let reg = self.engine.registry();
        let mut cache = self.tools.borrow_mut();
        if let Some((cached, list)) = cache.as_ref()
            && Arc::ptr_eq(cached, &reg)
        {
            return list.clone();
        }
        let list: Arc<[ToolInfo]> = reg.all().iter().map(|s| ToolInfo::from_spec(s, reg.schema(s))).collect();
        *cache = Some((reg, list.clone()));
        list
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_port_lists_the_registry_and_round_trips_a_call() {
        let engine = crate::demo_engine().expect("the demo engine starts");
        let mut port = EnginePort::new(engine.clone());
        let listed = port.tools();
        let reg = engine.registry();
        assert_eq!(listed.len(), reg.all().len());
        assert!(listed.iter().zip(reg.all()).all(|(t, s)| t.id == s.id && t.title == s.title));
        assert!(port.has_tool("session_list") && !port.has_tool("no_such_tool"));
        assert_eq!(port.effects("session_list"), reg.get("session_list").map(|s| s.effects));
        assert!(Arc::ptr_eq(&listed, &port.tools()), "an unchanged registry gives the same list");

        port.submit("session_list", serde_json::json!({}));
        assert!(port.busy());
        assert_eq!(port.submitted().last().map(|c| c.id.as_str()), Some("session_list"));
        let mut replies = Vec::new();
        for _ in 0..500 {
            replies.extend(port.poll().0);
            if !replies.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(replies.len(), 1, "the engine answers the call");
        assert!(!port.busy());
    }
}

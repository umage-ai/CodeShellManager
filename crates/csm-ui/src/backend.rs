//! The seam between the screens and whatever owns the state.
//!
//! - Screens read a [`UiModel`] snapshot and draw [`TerminalSource`]s. They never mutate the
//!   model: every action goes through [`Dispatcher::submit`], i.e.
//!   `engine.submit(Caller::Ui, "<tool_id>", params)`, and the receiver is polled once a frame.
//! - Widget-level input (keys into the focused pane, scroll, selection, resize) goes straight to
//!   the session's [`TerminalSource`]; it is not a tool.
//!
//! # Integrating the real engine
//!
//! Today [`crate::demo::DemoBackend`] implements [`Backend`]: it registers demo `ToolSpec`s for
//! the ids in `docs/tools.md` that the engine doesn't have yet (they validate their params and
//! echo them), and applies each tool's effect to its own model when the reply comes back.
//! Swapping in the real engine is a mapping job:
//!
//! 1. `Backend::model` returns `engine.view()` mapped to [`UiModel`] (or `UiModel` becomes the
//!    engine's `Snapshot`): sessions → [`SessionView`](crate::model::SessionView) (status from
//!    the 7501 records / heuristic, `accent` = index of `csm_core::color::hex_color` in the
//!    palette or the OSC 9001 override, `runs`, `parent` for worktrees), groups, `group_mode` +
//!    `rail` (`view_get`), `layouts` (`layout_get` per group), `active`, `inspector`,
//!    `inbox_panel`, restore / shutdown progress, folder suggestions, launch presets, notes and
//!    the theme setting.
//! 2. `Backend::terminal` returns `engine.terminal(id)` (the `TerminalHandle` implements
//!    [`TerminalSource`]: emulator behind a mutex, `set_grid` starts the PTY at the first
//!    measured size, `write` queues widget input).
//! 3. `Backend::on_reply` does nothing (the engine applied the effect and emitted events);
//!    `on_event` maps `SessionChanged`/`LayoutChanged`/… to "take a new snapshot".
//! 4. Delete `demo::tools()` from the registry and register the engine's modules instead; the
//!    ids, params and effects are the same, so no screen changes. `demo_set_status` is the only
//!    demo-only tool (the review's status switcher) and goes away.
//! 5. Ephemeral UI state that is *not* a tool today and should be decided on: group collapse in
//!    headers view, the palette/quick-open query text, open menus, an in-progress divider drag
//!    (the drag's end *is* a tool call: `layout_set` / `inbox_panel_set`).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use csm_engine::{Caller, Engine, Event, Registry, Reply};
use serde_json::Value;

use crate::model::{SessionId, UiModel};
use crate::terminal::TerminalSource;

/// One tool call the UI made.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub id: String,
    pub params: Value,
}

/// Something the backend wants the UI to show after a reply or an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Toast(String),
    /// Bring this session into view (switch rail tab, focus its pane).
    Focus(SessionId),
}

/// What owns the state the screens draw. See the module docs for the real-engine mapping.
pub trait Backend {
    /// The snapshot to draw this frame. Cheap: an `Arc` clone.
    fn model(&self) -> Arc<UiModel>;
    /// The terminal a pane shows.
    fn terminal(&self, id: &str) -> Option<Arc<dyn TerminalSource>>;
    /// A call finished. The demo applies its effect here; the real engine already has.
    fn on_reply(&mut self, call: &Call, reply: &Reply) -> Vec<Notice>;
    /// An engine event.
    fn on_event(&mut self, event: &Event) -> Vec<Notice>;
    /// Once a frame, before drawing (timers, repaint scheduling).
    fn tick(&mut self, _ctx: &egui::Context) {}
}

/// Submits tool calls and polls their replies and the engine's events. Never blocks.
pub struct Dispatcher {
    engine: Engine,
    events: Receiver<Event>,
    pending: Vec<(Call, Receiver<Reply>)>,
    log: Vec<Call>,
}

/// How many submitted calls are remembered for [`Dispatcher::submitted`].
const LOG_MAX: usize = 256;

impl Dispatcher {
    pub fn new(engine: Engine) -> Self {
        let events = engine.subscribe();
        Self { engine, events, pending: Vec::new(), log: Vec::new() }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn registry(&self) -> &Registry {
        self.engine.registry()
    }

    /// `engine.submit(Caller::Ui, id, params)`; the reply is picked up by [`Dispatcher::poll`].
    pub fn submit(&mut self, id: &str, params: Value) {
        log::debug!("ui → {id} {params}");
        let call = Call { id: id.to_string(), params };
        let rx = self.engine.submit(Caller::Ui, &call.id, call.params.clone());
        if self.log.len() >= LOG_MAX {
            self.log.remove(0);
        }
        self.log.push(call.clone());
        self.pending.push((call, rx));
    }

    /// Calls submitted so far (most recent last), for tests and diagnostics.
    pub fn submitted(&self) -> &[Call] {
        &self.log
    }

    /// Whether replies are still outstanding (the frame loop keeps polling while they are).
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Finished calls and new events since the last poll.
    pub fn poll(&mut self) -> (Vec<(Call, Reply)>, Vec<Event>) {
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
}

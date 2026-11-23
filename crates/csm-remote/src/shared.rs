//! One remote backend, seen through the two traits `csm_ui::CsmApp` takes as two boxes: the
//! [`ToolPort`] (calls go out, replies come back) and the [`Backend`] (the model, the terminals, the
//! events). Both faces share the backend on the UI thread (`Rc<RefCell<_>>`), so a window keeps one
//! connection to its host, as the laptop's own window keeps one engine.
//!
//! The borrows are short: each method borrows the backend for its own call and returns. The one
//! call that takes the port as an argument (`Backend::on_reply`) does not use it (see
//! `RemoteBackend::on_reply`), so it never needs the port while the backend is borrowed.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use csm_engine::{Event, Reply};
use csm_ui::Backend;
use csm_ui::backend::{Call, Notice, ToolInfo, ToolPort};
use csm_ui::model::UiModel;
use csm_ui::terminal::TerminalSource;
use csm_ui::view_overlay::ViewOverlay;
use serde_json::Value;

use crate::backend::RemoteBackend;

/// How many submitted calls the port remembers for [`ToolPort::submitted`].
const LOG_MAX: usize = 256;

/// The window's calls and tool list: the [`ToolPort`] face.
pub struct RemotePort {
    inner: Rc<RefCell<RemoteBackend>>,
    log: Vec<Call>,
}

/// The window's model and events: the [`Backend`] face.
pub struct RemoteView {
    inner: Rc<RefCell<RemoteBackend>>,
}

/// The window's own handle on its backend, for what the window keeps (its view state).
pub struct RemoteHandle {
    inner: Rc<RefCell<RemoteBackend>>,
}

impl RemoteHandle {
    /// The window's view state for this host, as it is now (a copy).
    pub fn overlay(&self) -> ViewOverlay {
        self.inner.borrow().overlay().clone()
    }
}

/// Splits one backend into the faces a window needs.
pub fn shared(backend: RemoteBackend) -> (RemotePort, RemoteView, RemoteHandle) {
    let inner = Rc::new(RefCell::new(backend));
    (RemotePort { inner: inner.clone(), log: Vec::new() }, RemoteView { inner: inner.clone() }, RemoteHandle { inner })
}

impl ToolPort for RemotePort {
    fn submit(&mut self, id: &str, params: Value) {
        if self.log.len() >= LOG_MAX {
            self.log.remove(0);
        }
        self.log.push(Call { id: id.to_owned(), params: params.clone() });
        self.inner.borrow_mut().submit(id, params);
    }

    fn poll(&mut self) -> (Vec<(Call, Reply)>, Vec<Event>) {
        self.inner.borrow_mut().poll()
    }

    fn busy(&self) -> bool {
        self.inner.borrow().busy()
    }

    fn submitted(&self) -> &[Call] {
        &self.log
    }

    fn tools(&self) -> Arc<[ToolInfo]> {
        self.inner.borrow().tools()
    }
}

impl Backend for RemoteView {
    fn model(&self) -> Arc<UiModel> {
        self.inner.borrow().model()
    }

    fn terminal(&self, id: &str) -> Option<Arc<dyn TerminalSource>> {
        self.inner.borrow().terminal(id)
    }

    fn on_reply(&mut self, call: &Call, reply: &Reply, port: &dyn ToolPort) -> Vec<Notice> {
        self.inner.borrow_mut().on_reply(call, reply, port)
    }

    fn on_event(&mut self, event: &Event) -> Vec<Notice> {
        self.inner.borrow_mut().on_event(event)
    }

    fn run_output(&self, session: &str, run: &str) -> Option<csm_engine::RunOutputHandle> {
        self.inner.borrow().run_output(session, run)
    }

    fn remote(&self) -> Option<csm_ui::remote_window::RemoteStatus> {
        self.inner.borrow().remote()
    }
}

//! The remote window's backend: the UI's [`Backend`] and [`ToolPort`] over one paired host.
//!
//! - Calls: a call the view overlay owns (layout, focus, rail, inspector, grids, theme) is applied
//!   to this client's own [`ViewOverlay`] and answered here. Everything else goes to the host, and
//!   its reply comes back by `poll`. A local focus is also sent to the host (`session_focus`: it
//!   acknowledges the result and wakes a sleeping session); if the host refuses, the overlay is put
//!   back as it was.
//! - State: the mirrored snapshot is rebuilt into a [`UiModel`] whenever it or the overlay changes.
//! - Terminals: [`RemoteBackend::show_terminals`] subscribes the panes the UI shows and drops the
//!   others.
//! - Local-only features (open folder, open with, file drop, image paste, native pickers, backup,
//!   app quit) are not routed here: the UI hides or refuses them for a remote window.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{SystemTime, UNIX_EPOCH};

use csm_engine::{Effects, Event, Reply, ToolOutput};
use csm_peer::client::ClientConfig;
use csm_peer::identity::Identity;
use csm_peer_proto::Snapshot as WireSnapshot;
use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::ids::KEY_LEN;
use csm_ui::backend::{Backend, Call, Notice, ToolInfo, ToolPort};
use csm_ui::docs_model::DocsDetails;
use csm_ui::git_model::GitDetails;
use csm_ui::model::UiModel;
use csm_ui::remote_window::{RemoteLink, RemoteStatus};
use csm_ui::terminal::TerminalSource;
use csm_ui::theme::ThemeKind;
use csm_ui::usage_model::UsageDetails;
use csm_ui::view_overlay::{self, Applied, Route, ThemeFact, ViewOverlay};
use serde_json::Value;

use crate::link::{self, Cmd, Latest, LinkState, Params, Up};
use crate::map;
use crate::terminal::RemoteTerminal;

/// How many forwarded calls are remembered for [`ToolPort::submitted`].
const LOG_MAX: usize = 256;

/// What a remote window needs to connect to a paired host.
pub struct ConnectParams {
    pub addr: PeerAddr,
    pub host_public: [u8; KEY_LEN],
    pub identity: Identity,
    pub cfg: ClientConfig,
    /// This client's own theme mode.
    pub theme: ThemeKind,
    /// The themes this client can draw (its own theme list).
    pub themes: Vec<ThemeFact>,
    /// This client's saved view state for this host. `None` seeds it from the host's first snapshot.
    pub overlay: Option<ViewOverlay>,
}

/// One remote window: a host's snapshot, its terminals and the calls to it.
pub struct RemoteBackend {
    cmds: Sender<Cmd>,
    up: Receiver<Up>,
    /// The newest mirrored snapshot, taken when the worker wakes the window (`Up::Snapshot`).
    latest: Arc<Latest>,
    link: LinkState,
    theme: ThemeKind,
    themes: Vec<ThemeFact>,
    mirror: Option<WireSnapshot>,
    overlay: ViewOverlay,
    seeded: bool,
    tools: Arc<[ToolInfo]>,
    /// Whether the host's tool list has arrived. Until it has, a call cannot be told apart from a
    /// view call (its effects are unknown), so it waits in `awaiting`.
    tools_known: bool,
    awaiting: Vec<(String, Value)>,
    next_key: u64,
    /// Calls sent to the host, by key.
    pending: HashMap<u64, Call>,
    /// The overlay to put back when a forwarded focus is refused.
    restore: HashMap<u64, ViewOverlay>,
    /// Replies the client answered itself, handed out by the next poll.
    ready: Vec<(Call, Reply)>,
    log: Vec<Call>,
    terms: HashMap<String, Arc<RemoteTerminal>>,
    next_stream: u32,
    model: Arc<UiModel>,
    dirty: bool,
    git: GitDetails,
    usage: UsageDetails,
    docs: DocsDetails,
    /// The host's name and address as the window shows them (`describe`).
    host_name: String,
    address: String,
}

/// The name a window shows until the host's own is known.
const UNNAMED_HOST: &str = "the office";

impl RemoteBackend {
    /// Names the host and its address for the window's strip and title. The name is the one the
    /// laptop stored for the host; the address is what the window dials, as text.
    pub fn describe(&mut self, host_name: &str, address: &str) {
        self.host_name = host_name.to_owned();
        self.address = address.to_owned();
    }

    /// Starts connecting. Returns at once: the worker thread does the network I/O, and the state
    /// is in [`RemoteBackend::link_state`].
    pub fn connect(p: ConnectParams) -> Result<Self, String> {
        let (up_tx, up_rx) = mpsc::channel();
        let latest = Arc::new(Latest::default());
        let cmds = link::spawn(Params { addr: p.addr, host_public: p.host_public, identity: p.identity, cfg: p.cfg }, up_tx, latest.clone())
            .map_err(|e| format!("could not start the connection thread: {e}"))?;
        let seeded = p.overlay.is_some();
        let mut b = RemoteBackend {
            cmds,
            up: up_rx,
            latest,
            link: LinkState::Connecting,
            theme: p.theme,
            themes: p.themes,
            mirror: None,
            overlay: p.overlay.unwrap_or_default(),
            seeded,
            tools: Arc::from(Vec::new()),
            tools_known: false,
            awaiting: Vec::new(),
            next_key: 0,
            pending: HashMap::new(),
            restore: HashMap::new(),
            ready: Vec::new(),
            log: Vec::new(),
            terms: HashMap::new(),
            next_stream: 0,
            model: Arc::new(UiModel::default()),
            dirty: true,
            git: GitDetails::default(),
            usage: UsageDetails { live: true, ..Default::default() },
            docs: DocsDetails { live: true, ..Default::default() },
            host_name: UNNAMED_HOST.to_owned(),
            address: String::new(),
        };
        b.rebuild();
        Ok(b)
    }

    /// The connection, as the window's indicator shows it.
    pub fn link_state(&self) -> &LinkState {
        &self.link
    }

    /// The overlay as it is now (the client saves it per host).
    pub fn overlay(&self) -> &ViewOverlay {
        &self.overlay
    }

    /// Makes the panes the UI shows live: a new session subscribes its terminal, a session no pane
    /// shows any more is unsubscribed. Call it once a frame with the visible session ids.
    pub fn show_terminals(&mut self, visible: &[String]) {
        for id in visible {
            if !self.terms.contains_key(id) {
                self.next_stream = self.next_stream.wrapping_add(1).max(1);
                let t = Arc::new(RemoteTerminal::new(id, self.next_stream, self.cmds.clone()));
                let _ = self.cmds.send(t.subscribe_cmd());
                self.terms.insert(id.clone(), t);
            }
        }
        let hidden: Vec<String> = self.terms.keys().filter(|k| !visible.contains(k)).cloned().collect();
        for id in hidden {
            if let Some(t) = self.terms.remove(&id) {
                let _ = self.cmds.send(Cmd::Unsubscribe { stream: t.stream() });
            }
        }
    }

    /// Tells the host which pane is typing (it sizes the PTY to that pane).
    pub fn set_pane_driver(&self, session: &str, driver: bool) {
        if let Some(t) = self.terms.get(session) {
            t.set_driver(driver);
        }
    }

    /// What the UI drew this frame (`Backend::show_panes`): subscribes the shown panes' terminals,
    /// drops the others, and marks `driver` (the focused pane) as the one that types. Every other
    /// pane is marked not driving; a mark that does not change sends nothing.
    pub fn show_panes(&mut self, visible: &[String], driver: Option<&str>) {
        self.show_terminals(visible);
        for (id, t) in &self.terms {
            t.set_driver(driver == Some(id.as_str()));
        }
    }

    /// Ends the connection (a remote window's Quit): the worker closes the link and stops.
    pub fn stop(&self) {
        let _ = self.cmds.send(Cmd::Stop);
    }

    /// Recomputes the model from the mirror and the overlay.
    fn rebuild(&mut self) {
        let mut m = match &self.mirror {
            Some(s) => map::model(s, &self.overlay, self.theme, now_ms()),
            None => UiModel { theme: self.theme, ..UiModel::default() },
        };
        m.remote = true;
        // The last snapshot is kept on screen while the host is out of reach, marked stale.
        m.stale = self.mirror.is_some() && matches!(self.link, LinkState::Reconnecting { .. } | LinkState::Stopped);
        m.git = self.git.map.clone();
        m.folder_worktrees = self.git.folders.clone();
        m.usage = self.usage.clone();
        m.docs = self.docs.clone();
        self.model = Arc::new(m);
        self.dirty = false;
    }

    fn facts(&self) -> view_overlay::ViewFacts {
        map::facts(self.mirror.as_ref(), &self.themes)
    }

    fn next_call_key(&mut self) -> u64 {
        self.next_key = self.next_key.wrapping_add(1).max(1);
        self.next_key
    }

    /// Sends a call to the host. `restore` is the overlay to put back if the host refuses.
    fn forward(&mut self, call: Call, restore: Option<ViewOverlay>) {
        self.log_call(&call);
        let key = self.next_call_key();
        let cmd = Cmd::Call { key, tool: call.id.clone(), params: call.params.clone() };
        match self.cmds.send(cmd) {
            Ok(()) => {
                if let Some(s) = restore {
                    self.restore.insert(key, s);
                }
                self.pending.insert(key, call);
            }
            Err(_) => self.ready.push((call, map::lost("the remote window is not connected"))),
        }
    }

    fn log_call(&mut self, call: &Call) {
        if self.log.len() >= LOG_MAX {
            self.log.remove(0);
        }
        self.log.push(call.clone());
    }

    /// Answers the calls still waiting for a tool list, when there will not be one.
    fn fail_awaiting(&mut self, replies: &mut Vec<(Call, Reply)>) {
        for (id, params) in std::mem::take(&mut self.awaiting) {
            replies.push((Call { id, params }, map::lost("the office is not reachable")));
        }
    }

    fn apply_up(&mut self, u: Up, replies: &mut Vec<(Call, Reply)>, events: &mut Vec<Event>) {
        match u {
            Up::State(s) => {
                if matches!(s, LinkState::Refused { .. } | LinkState::Stopped) {
                    self.fail_awaiting(replies);
                }
                self.link = s;
                // The model's stale mark follows the link.
                self.dirty = true;
            }
            Up::Tools(list) => {
                self.tools = Arc::from(list);
                self.tools_known = true;
            }
            Up::Snapshot => {
                let Some(snap) = self.latest.take() else { return };
                if !self.seeded {
                    map::seed_overlay(&snap, &mut self.overlay);
                    self.seeded = true;
                }
                self.mirror = Some(*snap);
                self.dirty = true;
            }
            Up::Event(e) => events.extend(map::event(&e)),
            Up::Reply { key, outcome } => {
                let Some(call) = self.pending.remove(&key) else { return };
                let reply = map::reply(outcome);
                match self.restore.remove(&key) {
                    Some(saved) if reply.is_err() => {
                        self.overlay = saved;
                        self.dirty = true;
                    }
                    _ => {}
                }
                replies.push((call, reply));
            }
        }
    }
}

impl Drop for RemoteBackend {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Stop);
    }
}

impl ToolPort for RemoteBackend {
    fn submit(&mut self, id: &str, params: Value) {
        log::debug!("remote → {id}");
        if !self.tools_known {
            // Its effects are not known yet, so it is not known whether it is a view call.
            if matches!(self.link, LinkState::Refused { .. } | LinkState::Stopped) {
                self.ready.push((Call { id: id.to_owned(), params }, map::lost("the office is not reachable")));
            } else {
                self.awaiting.push((id.to_owned(), params));
            }
            return;
        }
        let call = Call { id: id.to_owned(), params: params.clone() };
        let tool = self.tools.iter().find(|t| t.id == id).cloned();
        let Some(tool) = tool else {
            // Not in the host's list (yet): the host answers with its own error.
            self.forward(call, None);
            return;
        };
        match view_overlay::route(&tool, &params) {
            Route::Local => {
                let facts = self.facts();
                let result = view_overlay::apply(&mut self.overlay, &facts, &tool, params);
                self.dirty = true;
                self.log_call(&call);
                self.ready.push((call, result.map(|_| ToolOutput::message(""))));
            }
            Route::Focus => {
                let facts = self.facts();
                let before = self.overlay.clone();
                match view_overlay::apply(&mut self.overlay, &facts, &tool, params.clone()) {
                    Ok(Applied::Focused { .. }) => {
                        self.dirty = true;
                        self.forward(call, Some(before));
                    }
                    Ok(_) => self.ready.push((call, Ok(ToolOutput::message("")))),
                    Err(e) => self.ready.push((call, Err(e))),
                }
            }
            Route::Forward | Route::UiNotModelled => self.forward(call, None),
        }
    }

    fn poll(&mut self) -> (Vec<(Call, Reply)>, Vec<Event>) {
        let mut replies = std::mem::take(&mut self.ready);
        let mut events = Vec::new();
        loop {
            match self.up.try_recv() {
                Ok(u) => self.apply_up(u, &mut replies, &mut events),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The worker has ended: nothing else will come for the calls still waiting.
                    for (_, call) in std::mem::take(&mut self.pending) {
                        replies.push((call, map::lost("the remote window stopped")));
                    }
                    self.fail_awaiting(&mut replies);
                    break;
                }
            }
        }
        // Calls that waited for the tool list go out now, in the order they were made.
        if self.tools_known {
            for (id, params) in std::mem::take(&mut self.awaiting) {
                self.submit(&id, params);
            }
        }
        replies.append(&mut self.ready);
        if self.dirty {
            self.rebuild();
        }
        (replies, events)
    }

    fn busy(&self) -> bool {
        !self.pending.is_empty() || !self.ready.is_empty() || !self.awaiting.is_empty()
    }

    fn submitted(&self) -> &[Call] {
        &self.log
    }

    fn tools(&self) -> Arc<[ToolInfo]> {
        self.tools.clone()
    }
}

impl Backend for RemoteBackend {
    fn model(&self) -> Arc<UiModel> {
        self.model.clone()
    }

    fn remote(&self) -> Option<RemoteStatus> {
        let (name, link) = match &self.link {
            LinkState::Connecting => (self.host_name.clone(), RemoteLink::Connecting),
            LinkState::Live { .. } => (self.host_name.clone(), RemoteLink::Connected),
            LinkState::Reconnecting { .. } | LinkState::Stopped => (self.host_name.clone(), RemoteLink::Offline),
            LinkState::Refused { reason } => (self.host_name.clone(), RemoteLink::Refused(reason.clone())),
        };
        Some(RemoteStatus { host_name: name, address: self.address.clone(), link })
    }

    fn terminal(&self, id: &str) -> Option<Arc<dyn TerminalSource>> {
        self.terms.get(id).map(|t| t.clone() as Arc<dyn TerminalSource>)
    }

    fn show_panes(&mut self, visible: &[String], driver: Option<&str>) {
        RemoteBackend::show_panes(self, visible, driver);
    }

    /// The host applied the effect. Errors are shown; so are the results of calls that start or
    /// discard something (the same rule as the office's own window).
    fn on_reply(&mut self, call: &Call, reply: &Reply, _port: &dyn ToolPort) -> Vec<Notice> {
        if let Some(notices) = self.git.on_reply(call, reply) {
            self.rebuild();
            return notices;
        }
        if self.usage.on_reply(call, reply).is_some() || self.docs.on_reply(call, reply).is_some() {
            self.rebuild();
            return Vec::new();
        }
        match reply {
            Err(e) => vec![Notice::Toast(e.to_string())],
            Ok(out) => {
                let loud = Effects::LAUNCHES_PROCESS.union(Effects::MAY_DISCARD_DATA);
                // The host's own declaration (its tool list), read here rather than through the port:
                // a window shares this backend between its two faces, so the port may be borrowed.
                let say = call.id != "app_quit" && self.tools.iter().find(|t| t.id == call.id).is_some_and(|t| t.effects.intersects(loud));
                if say && !out.message.is_empty() { vec![Notice::Toast(out.message.clone())] } else { Vec::new() }
            }
        }
    }

    /// Toasts and focus requests. The theme is this client's own, so a host's theme change is not
    /// followed; the host's quit ends only the host, never this window (see the module docs).
    fn on_event(&mut self, event: &Event) -> Vec<Notice> {
        match event {
            Event::Toast { message } => vec![Notice::Toast(message.clone())],
            Event::FocusRequested { session_id } => vec![Notice::Focus(session_id.clone())],
            _ => Vec::new(),
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

//! The app: one frame = poll the engine, draw the snapshot, turn clicks into actions. Every
//! action that changes anything is a tool call ([`Action::Tool`]); the rest is ephemeral UI state
//! (which menu is open, what the palette's query is).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use csm_engine::Engine;
use egui::{Key, KeyboardShortcut, Modifiers, Rect, Ui, UiBuilder, ViewportId, pos2};
use serde_json::{Value, json};

use crate::backend::{Backend, Call, Dispatcher, Notice};
use crate::model::{GroupId, Scope, SessionId, Status, UiModel};
use crate::screens;
use crate::terminal::{TerminalRenderer, TerminalView, lock};
use crate::theme::{self, ThemeKind, size};

/// Something the person did.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// `engine.submit(Caller::Ui, id, params)`.
    Tool(&'static str, Value),
    OpenPalette,
    OpenQuickOpen { group: Option<GroupId> },
    CloseOverlays,
    OpenMenu(Menu),
    CloseMenu,
    /// Headers view: fold or unfold a group (`group_collapse`).
    ToggleCollapse(GroupId),
    StartRename(SessionId),
    StartGroupRename(GroupId),
    NewGroupPrompt,
    CopyText(String),
    /// Run a registered tool from the palette, filling params from context.
    PaletteTool(&'static str),
    /// Keyboard focus into a pane's terminal.
    FocusTerminal(SessionId),
    /// The busy-close dialog (closing the app).
    AskQuit,
    /// A consent card was answered.
    Consent { request_id: u64, answer: &'static str },
    /// Pane divider drag finished in Free mode, or a float moved: persist the layout.
    Toast(String),
}

/// Which popup menu is open.
#[derive(Debug, Clone, PartialEq)]
pub enum Menu {
    /// ⋯ on a pane header.
    Pane { session: SessionId, at: egui::Pos2 },
    /// Right-click on a sidebar row.
    Row { session: SessionId, at: egui::Pos2 },
    /// Right-click on a rail tab or group header, or ⋯ on the scope header.
    Group { group: GroupId, at: egui::Pos2 },
    /// The grid presets popover under the Grid segment.
    Grid { at: egui::Pos2 },
    /// The run split-button's list.
    Runs { session: SessionId, at: egui::Pos2 },
    /// The quick-open's group picker.
    QuickGroup { at: egui::Pos2 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlay {
    #[default]
    None,
    Palette,
    QuickOpen,
    Busy,
    /// A one-line text prompt (rename, new group).
    Prompt,
}

#[derive(Debug, Clone, Default)]
pub struct PaletteState {
    pub query: String,
    pub sel: usize,
    /// Scroll the selection into view once (after a key press), never every frame.
    pub scroll_to_sel: bool,
    pub focus_input: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemoteKind {
    #[default]
    Ssh,
    Wsl,
}

#[derive(Debug, Clone, Default)]
pub struct QuickOpenState {
    pub query: String,
    pub sel: usize,
    pub cmd: usize,
    pub remote: bool,
    pub rkind: RemoteKind,
    pub host: String,
    pub remote_folder: String,
    pub distro: String,
    pub linux_folder: String,
    pub group: Option<GroupId>,
    pub focus_input: bool,
    pub scroll_to_sel: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PromptKind {
    RenameSession(SessionId),
    RenameGroup(GroupId),
    NewGroup,
}

#[derive(Debug, Clone)]
pub struct PromptState {
    pub kind: PromptKind,
    pub text: String,
    pub focus: bool,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub until: f64,
}

/// In-progress drags. Their end is a tool call; while they run, the preview is local.
#[derive(Debug, Clone, Default)]
pub struct Drags {
    /// The inbox divider: the height being shown.
    pub inbox_h: Option<f32>,
    /// A Free-mode divider: the tree being shown.
    pub tree: Option<crate::model::SplitNode>,
    /// A floating pane being moved: (session, rect).
    pub float: Option<(SessionId, [f32; 4])>,
}

/// Ephemeral UI state.
#[derive(Default)]
pub struct UiState {
    pub overlay: Overlay,
    pub palette: PaletteState,
    pub quick: QuickOpenState,
    pub prompt: Option<PromptState>,
    pub menu: Option<Menu>,
    /// The submenu open inside the current menu (by label).
    pub submenu: Option<String>,
    pub toasts: Vec<Toast>,
    pub consents: Vec<Value>,
    pub drags: Drags,
    /// Notes being edited, by folder (saved with `note_set` after a pause).
    pub notes: HashMap<String, (String, f64)>,
    pub focus_terminal: Option<SessionId>,
    /// The last active session seen, to move keyboard focus when it changes.
    pub last_active: Option<SessionId>,
}

/// Terminal views and the renderer, shared with torn-off windows (deferred viewports draw from
/// the same atlas). Only ever locked on the UI thread, one window at a time.
pub struct PaneHost {
    pub renderer: TerminalRenderer,
    pub views: HashMap<String, TerminalView>,
}

impl PaneHost {
    pub fn view(&mut self, key: &str) -> &mut TerminalView {
        self.views.entry(key.to_string()).or_insert_with(|| TerminalView::new(egui::Id::new(("csm-pane", key.to_string()))))
    }
}

pub struct CsmApp {
    pub backend: Box<dyn Backend>,
    pub dispatch: Dispatcher,
    pub host: Arc<Mutex<PaneHost>>,
    pub ui: UiState,
    pub model: Arc<UiModel>,
    /// Actions raised inside torn-off windows, drained by the main frame.
    pub remote_actions: Arc<Mutex<Vec<Action>>>,
    theme_applied: Option<ThemeKind>,
    fonts_installed: bool,
    /// `app_quit` finished: the window may close now.
    pub quit_ready: bool,
}

impl CsmApp {
    pub fn new(engine: Engine, backend: Box<dyn Backend>, renderer: TerminalRenderer) -> Self {
        let model = backend.model();
        Self {
            backend,
            dispatch: Dispatcher::new(engine),
            host: Arc::new(Mutex::new(PaneHost { renderer, views: HashMap::new() })),
            ui: UiState::default(),
            model,
            remote_actions: Arc::new(Mutex::new(Vec::new())),
            theme_applied: None,
            fonts_installed: false,
            quit_ready: false,
        }
    }

    /// Calls the UI submitted (most recent last).
    pub fn submitted(&self) -> &[Call] {
        self.dispatch.submitted()
    }

    pub fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        let now = ctx.input(|i| i.time);
        self.ui.toasts.retain(|t| t.until > now);
        self.ui.toasts.push(Toast { text: text.into(), until: now + f64::from(theme::timing::TOAST_SECS) });
        if self.ui.toasts.len() > 3 {
            self.ui.toasts.remove(0);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f32(theme::timing::TOAST_SECS));
    }

    fn notices(&mut self, ctx: &egui::Context, notices: Vec<Notice>) {
        for n in notices {
            match n {
                Notice::Toast(t) if !t.is_empty() => self.toast(ctx, t),
                Notice::Toast(_) => {}
                Notice::Focus(id) => self.ui.focus_terminal = Some(id),
                Notice::QuitReady => {
                    self.quit_ready = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    /// Poll replies and events; refresh the snapshot.
    fn poll(&mut self, ctx: &egui::Context) {
        let (done, events) = self.dispatch.poll();
        for (call, reply) in done {
            let n = self.backend.on_reply(&call, &reply);
            self.notices(ctx, n);
        }
        for e in events {
            match &e {
                csm_engine::Event::ConsentRequested { request } => self.ui.consents.push(request.clone()),
                csm_engine::Event::ConsentResolved { request_id } => {
                    self.ui.consents.retain(|r| r["request_id"].as_u64() != Some(*request_id));
                }
                csm_engine::Event::Error { message } => self.toast(ctx, message.clone()),
                _ => {}
            }
            let n = self.backend.on_event(&e);
            self.notices(ctx, n);
        }
        let n = self.backend.tick(ctx);
        self.notices(ctx, n);
        self.model = self.backend.model();
        if self.dispatch.busy() {
            ctx.request_repaint();
        }
    }

    /// Installs fonts and the theme. Fonts set on a context take effect only from a later pass,
    /// so until our families are really bound nothing is drawn (returns false) and another pass
    /// is requested.
    fn apply_theme(&mut self, ctx: &egui::Context) -> bool {
        let bound = ctx.fonts_mut(|f| f.definitions().families.contains_key(&theme::semibold_family()));
        if !bound {
            if !self.fonts_installed {
                setup(ctx, self.model.theme);
                self.theme_applied = Some(self.model.theme);
                self.fonts_installed = true;
            }
            ctx.request_repaint();
            return false;
        }
        if self.theme_applied != Some(self.model.theme) {
            theme::apply(ctx, self.model.theme);
            self.theme_applied = Some(self.model.theme);
        }
        // Terminal glyph weight follows the theme (measured against egui's text: see
        // `tests/text_weight.rs`); glyphs already in the atlas were made with the old curve.
        let want = crate::terminal::fonts::Coverage::for_dark_theme(self.model.theme == ThemeKind::Mocha);
        let mut host = lock(&self.host);
        if host.renderer.fonts.coverage != want {
            host.renderer.fonts.coverage = want;
            if let Some(atlas) = host.renderer.atlas.as_mut() {
                atlas.clear();
            }
        }
        true
    }

    /// The scope the panes show and the layout changes go to.
    pub fn scope(&self) -> Scope {
        self.model.pane_scope()
    }

    pub fn run(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Tool(id, params) => {
                // Opening a menu item closes the menu.
                self.ui.menu = None;
                self.ui.submenu = None;
                self.dispatch.submit(id, params);
                ctx.request_repaint();
            }
            Action::OpenPalette => {
                self.ui.menu = None;
                self.ui.overlay = Overlay::Palette;
                self.ui.palette = PaletteState { focus_input: true, ..PaletteState::default() };
            }
            Action::OpenQuickOpen { group } => {
                self.ui.menu = None;
                self.ui.overlay = Overlay::QuickOpen;
                let group = group.or_else(|| match self.scope() {
                    Scope::Group(g) => Some(g),
                    Scope::All => None,
                });
                self.ui.quick = QuickOpenState {
                    group,
                    focus_input: true,
                    ..QuickOpenState::default()
                };
            }
            Action::CloseOverlays => {
                self.ui.overlay = Overlay::None;
                self.ui.menu = None;
                self.ui.submenu = None;
                self.ui.prompt = None;
                if let Some(a) = self.model.active.clone() {
                    self.ui.focus_terminal = Some(a);
                }
            }
            Action::OpenMenu(m) => {
                self.ui.submenu = None;
                self.ui.menu = if self.ui.menu.as_ref() == Some(&m) { None } else { Some(m) };
            }
            Action::CloseMenu => {
                self.ui.menu = None;
                self.ui.submenu = None;
            }
            Action::ToggleCollapse(g) => {
                let collapsed = self.model.group(&g).is_some_and(|x| x.collapsed);
                self.run(ctx, Action::Tool("group_collapse", json!({"group_id": g, "collapsed": !collapsed})));
            }
            Action::StartRename(id) => {
                let text = self.model.session(&id).map(|s| s.name.clone()).unwrap_or_default();
                self.ui.menu = None;
                self.ui.prompt = Some(PromptState { kind: PromptKind::RenameSession(id), text, focus: true });
                self.ui.overlay = Overlay::Prompt;
            }
            Action::StartGroupRename(id) => {
                let text = self.model.group(&id).map(|g| g.name.clone()).unwrap_or_default();
                self.ui.menu = None;
                self.ui.prompt = Some(PromptState { kind: PromptKind::RenameGroup(id), text, focus: true });
                self.ui.overlay = Overlay::Prompt;
            }
            Action::NewGroupPrompt => {
                self.ui.menu = None;
                self.ui.prompt = Some(PromptState { kind: PromptKind::NewGroup, text: String::new(), focus: true });
                self.ui.overlay = Overlay::Prompt;
            }
            Action::CopyText(t) => {
                self.ui.menu = None;
                ctx.copy_text(t);
                self.toast(ctx, "Copied the folder path.");
            }
            Action::PaletteTool(id) => self.palette_tool(ctx, id),
            Action::FocusTerminal(id) => self.ui.focus_terminal = Some(id),
            Action::AskQuit => {
                self.ui.menu = None;
                if self.model.shutdown.is_some() {
                    // Already closing: the board is up.
                    return;
                }
                let busy = self.model.sessions.iter().any(|s| matches!(s.status, Status::Working | Status::Approval | Status::Question));
                if busy {
                    self.ui.overlay = Overlay::Busy;
                } else {
                    self.run(ctx, Action::Tool("app_quit", json!({})));
                }
            }
            Action::Consent { request_id, answer } => {
                self.ui.consents.retain(|r| r["request_id"].as_u64() != Some(request_id));
                self.run(ctx, Action::Tool("consent_answer", json!({"request_id": request_id, "answer": answer})));
            }
            Action::Toast(t) => self.toast(ctx, t),
        }
    }

    /// A tool picked in the palette: params are filled from context (the active session, the
    /// group the panes show); tools that need typed input open their own panel.
    fn palette_tool(&mut self, ctx: &egui::Context, id: &'static str) {
        self.ui.overlay = Overlay::None;
        let active = self.model.active.clone();
        let group = match self.scope() {
            Scope::Group(g) => Some(g),
            Scope::All => active.as_deref().and_then(|a| self.model.session(a)).and_then(|s| s.group.clone()),
        };
        match id {
            "session_new" => return self.run(ctx, Action::OpenQuickOpen { group: None }),
            "group_new" => return self.run(ctx, Action::NewGroupPrompt),
            "session_rename" => {
                if let Some(a) = active {
                    self.run(ctx, Action::StartRename(a));
                }
                return;
            }
            "group_rename" => {
                if let Some(g) = group {
                    self.run(ctx, Action::StartGroupRename(g));
                }
                return;
            }
            "app_quit" => return self.run(ctx, Action::AskQuit),
            "inspector_show" => {
                let tab = if self.model.inspector.is_some() { "hidden" } else { "notes" };
                return self.run(ctx, Action::Tool("inspector_show", json!({"tab": tab})));
            }
            "inbox_panel_set" => {
                return self.run(ctx, Action::Tool("inbox_panel_set", json!({"open": !self.model.inbox_panel.open})));
            }
            _ => {}
        }
        let Some(spec) = self.dispatch.registry().get(id).copied() else { return };
        let schema = (spec.input_schema)();
        let required: Vec<String> =
            schema["required"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let mut params = serde_json::Map::new();
        let mut missing = Vec::new();
        for r in &required {
            let v = match r.as_str() {
                "session_id" => active.clone().map(Value::from),
                "group_id" => group.clone().map(Value::from),
                "group" => Some(Value::from(self.scope().key())),
                "tab" => Some(Value::from("notes")),
                "folder" => active.as_deref().and_then(|a| self.model.session(a)).map(|s| Value::from(s.folder.clone())),
                _ => None,
            };
            match v {
                Some(v) => {
                    params.insert(r.clone(), v);
                }
                None => missing.push(r.clone()),
            }
        }
        if !missing.is_empty() {
            self.toast(ctx, format!("“{}” needs {}: use it from its menu.", spec.title, missing.join(", ")));
            return;
        }
        // Programmatic confirmations: the palette asks nothing more for bulk tools; they keep
        // their own guards (busy sessions are refused without `force`).
        self.run(ctx, Action::Tool(spec.id, Value::Object(params)));
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let pressed = |ctx: &egui::Context, m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        if pressed(ctx, cmd, Key::K) {
            if self.ui.overlay == Overlay::Palette {
                self.run(ctx, Action::CloseOverlays);
            } else {
                self.run(ctx, Action::OpenPalette);
            }
        }
        if pressed(ctx, cmd, Key::T) {
            self.run(ctx, Action::OpenQuickOpen { group: None });
        }
        if pressed(ctx, cmd | Modifiers::SHIFT, Key::T) {
            self.run(ctx, Action::Tool("session_reopen_closed", json!({})));
        }
        if pressed(ctx, cmd, Key::J) {
            self.run(ctx, Action::Tool("inbox_next", json!({})));
        }
        if pressed(ctx, cmd, Key::I) {
            let tab = if self.model.inspector.is_some() { "hidden" } else { "notes" };
            self.run(ctx, Action::Tool("inspector_show", json!({"tab": tab})));
        }
        if let Some(a) = self.model.active.clone() {
            if pressed(ctx, Modifiers::SHIFT, Key::F5) {
                self.run(ctx, Action::Tool("run_stop", json!({"session_id": a})));
            } else if pressed(ctx, Modifiers::NONE, Key::F5) {
                self.run(ctx, Action::Tool("run_start", json!({"session_id": a})));
            }
            if pressed(ctx, cmd, Key::W) {
                self.run(ctx, Action::Tool("session_close", json!({"session_id": a, "force": false})));
            }
        }
        let open = self.ui.overlay != Overlay::None || self.ui.menu.is_some();
        if open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.run(ctx, Action::CloseOverlays);
        }
    }

    /// One frame, in `ui` (the root of the main window).
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        // Closing the window is quitting: sessions get their time to exit (the shutdown board),
        // and the window closes on `QuitReady`.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_ready {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.run(&ctx, Action::AskQuit);
        }
        if !self.apply_theme(&ctx) {
            return;
        }
        let remote: Vec<Action> = std::mem::take(&mut *lock(&self.remote_actions));
        for a in remote {
            self.run(&ctx, a);
        }
        self.shortcuts(&ctx);
        // The active session changed (inbox, palette, click): keyboard follows.
        if self.model.active != self.ui.last_active {
            self.ui.last_active = self.model.active.clone();
            if self.ui.overlay == Overlay::None
                && let Some(a) = self.model.active.clone()
            {
                self.ui.focus_terminal = Some(a);
            }
        }

        let t = theme::Tokens::get(&ctx);
        // The whole window, whatever margin the host gave the root ui.
        let full = ctx.content_rect();
        let mut root = ui.new_child(UiBuilder::new().max_rect(full));
        root.set_clip_rect(full);
        let ui = &mut root;
        ui.painter().rect_filled(full, 0.0, t.bg);
        let mut actions = Vec::new();

        let title = Rect::from_min_max(full.min, pos2(full.max.x, full.min.y + size::TITLE_BAR));
        ui.scope_builder(UiBuilder::new().max_rect(title), |ui| screens::titlebar::show(ui, self, &mut actions));
        let mut body_top = title.max.y;
        if let Some(r) = &self.model.restore {
            let rail = Rect::from_min_max(pos2(full.min.x, body_top), pos2(full.max.x, body_top + size::RESTORE_RAIL));
            screens::overlays::restore_rail(ui, rail, r);
            body_top = rail.max.y;
        }
        let body = Rect::from_min_max(pos2(full.min.x, body_top), full.max);
        let mut x = body.min.x;
        if self.model.group_mode == crate::model::GroupMode::Rail {
            let rail = Rect::from_min_max(pos2(x, body.min.y), pos2(x + size::RAIL, body.max.y));
            ui.scope_builder(UiBuilder::new().max_rect(rail), |ui| screens::sidebar::rail(ui, self, &mut actions));
            x = rail.max.x;
        }
        let side = Rect::from_min_max(pos2(x, body.min.y), pos2(x + size::SIDEBAR, body.max.y));
        ui.scope_builder(UiBuilder::new().max_rect(side), |ui| screens::sidebar::show(ui, self, &mut actions));
        let mut work = Rect::from_min_max(pos2(side.max.x, body.min.y), body.max);
        if self.model.inspector.is_some() {
            let insp = Rect::from_min_max(pos2(body.max.x - size::INSPECTOR, body.min.y), body.max);
            work.max.x = insp.min.x;
            ui.scope_builder(UiBuilder::new().max_rect(insp), |ui| screens::inspector::show(ui, self, &mut actions));
        }
        ui.scope_builder(UiBuilder::new().max_rect(work), |ui| screens::panes::workspace(ui, self, &mut actions));
        screens::panes::torn_windows(&ctx, self);

        // Overlays, top-most last.
        screens::menus::show(&ctx, self, &mut actions);
        screens::palette::show(&ctx, self, &mut actions);
        screens::overlays::show(&ctx, self, &mut actions);

        for a in actions {
            self.run(&ctx, a);
        }
        if self.dispatch.busy() {
            ctx.request_repaint();
        }
    }

    /// Wakes the main window from another thread or viewport.
    pub fn repaint_main(ctx: &egui::Context) {
        ctx.request_repaint_of(ViewportId::ROOT);
    }
}

/// Fonts, image loaders and the theme on a context. Call it when the context is created (the
/// eframe creator) so the first frame already has them; the app also does it itself if needed.
pub fn setup(ctx: &egui::Context, kind: ThemeKind) {
    theme::install_fonts(ctx);
    egui_extras::install_image_loaders(ctx);
    theme::apply(ctx, kind);
}

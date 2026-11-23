//! The app: one frame = poll the engine, draw the snapshot, turn clicks into actions. Every
//! action that changes anything is a tool call ([`Action::Tool`]); the rest is ephemeral UI state
//! (which menu is open, what the palette's query is).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Key, Modifiers, Rect, Ui, UiBuilder, ViewportId, pos2};
use serde_json::{Value, json};

use crate::backend::{Backend, Call, Notice, ToolPort};
use crate::model::{GroupId, Scope, SessionId, Status, UiModel};
use crate::screens;
use crate::terminal::{TerminalRenderer, TerminalView, lock};
use crate::theme::{self, ThemeKind, size};

/// Something the person did.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A tool call on the port (`engine.submit(Caller::Ui, id, params)` for the engine).
    Tool(&'static str, Value),
    /// [`Action::Tool`] for an id that is only known at run time (an add-on's, from the tool list).
    ToolNamed(String, Value),
    /// Ask the person to enable an add-on: a confirm dialog lists what it contributes, then
    /// `addon_enable` with the version shown.
    AddonConsent(String),
    OpenPalette,
    /// Open (or close) the Recent menu under the title bar's clock button (Ctrl+Shift+E).
    OpenRecent,
    OpenQuickOpen {
        group: Option<GroupId>,
    },
    CloseOverlays,
    OpenMenu(Menu),
    CloseMenu,
    /// Headers view: fold or unfold a group (`group_collapse`).
    ToggleCollapse(GroupId),
    StartRename(SessionId),
    StartGroupRename(GroupId),
    NewGroupPrompt,
    /// "Send to all sessions…" on a group: the text prompt, then a confirm with the count
    /// (`session_send_text_many`).
    BroadcastPrompt(GroupId),
    CopyText(String),
    /// Copy text and say so (`(text, toast)`).
    CopyAs(String, String),
    /// Open the Runs tab with its editor on this session's run commands.
    EditRuns(SessionId),
    /// "New worktree from this branch…": the Git tab on this session with its New worktree
    /// input open (`worktree_new`).
    NewWorktree(SessionId),
    /// The Open with editor (`open_with_set`, `open_with_detect`).
    OpenWithEditor,
    /// Run a registered tool from the palette, filling params from context.
    PaletteTool(String),
    /// Keyboard focus into a pane's terminal.
    FocusTerminal(SessionId),
    /// The busy-close dialog (closing the app).
    AskQuit,
    /// The answer to "Keep CodeShellManager running?" (closing the window with `CloseAction`
    /// Ask): hide to the tray or quit, and with `remember` make it the `CloseAction`.
    CloseAnswer {
        tray: bool,
        remember: bool,
    },
    /// Close one session from the UI (a pane's ✕, its ⋯ menu, the sidebar menu, Ctrl+W):
    /// `session_close`, asking first when it is busy unless that warning is off.
    CloseSession(SessionId),
    /// A consent card was answered.
    Consent {
        request_id: u64,
        answer: &'static str,
    },
    /// The quick-open in edit mode, filled in from the session (`session_get`), saving with
    /// `session_edit`.
    EditSession(SessionId),
    /// Settings, as a page over the work area (title bar ⚙). Loads with `settings_get`,
    /// `app_info` and `grant_list`.
    OpenSettings,
    CloseSettings,
    /// The Usage dashboard (a page over the work area, like Settings), on a session when given.
    OpenUsage(Option<crate::screens::usage_page::Drill>),
    CloseUsage,
    /// The keyboard shortcuts (`?`, F1, Ctrl+/).
    ShowHelp,
    /// Ask before running `yes` (a small dialog; Enter is the default answer).
    Confirm(ConfirmState),
    /// Commit or cancel an in-place rename / new group field.
    CommitInline,
    CancelInline,
    /// Open the search panel (Ctrl+F, the palette row) with this query; a non-empty one searches at once.
    OpenSearch(String),
    /// Run the search panel's current query (`search_output` + `search_notes`).
    RunSearch,
    /// Pane divider drag finished in Free mode, or a float moved: persist the layout.
    Toast(String),
    /// Sidebar multi-select (widget state, not a tool): Ctrl-click toggles a row,
    /// Shift-click selects the visible range from the anchor, Esc clears.
    SelectToggle(SessionId),
    SelectRange(SessionId),
    ClearSelection,
    /// The Board's "New task" prompt for a project; `assign_to`: then assign it to that session
    /// and send it (an idle card's "+ task").
    NewTaskPrompt { project: String, assign_to: Option<SessionId> },
    /// The prompt was answered: `task_new`, then (`assign_to`) `task_assign` once it is made.
    NewTask { project: String, title: String, assign_to: Option<SessionId> },
    /// A widget in an add-on screen raised an event for its plugin (widget-level input, like a
    /// keystroke into a pane: not a tool).
    ScreenEvent { instance: String, event: csm_core::addons::screen::ScreenEvent },
    /// A button in an add-on screen runs a tool, through the normal dispatch. `confirm`: CSM
    /// asks first, showing the add-on, the tool and the exact text and target.
    ScreenTool { instance: String, tool: String, params: Value, confirm: bool },
    /// The person confirmed a [`Action::ScreenTool`].
    ScreenConfirmed { tool: String, params: Value },
    /// Show this add-on screen in the view strip; `None` shows the sessions.
    ScreenFocus(Option<String>),
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
    /// The Git tab's GitHub links (▾ beside Open on GitHub).
    GitHub { session: SessionId, at: egui::Pos2 },
    /// The quick-open's group picker.
    QuickGroup { at: egui::Pos2 },
    /// The sort control next to the Sessions header.
    Sort { at: egui::Pos2 },
    /// The Recent menu (title bar, Ctrl+Shift+E), its top-right corner at `at`.
    Recent { at: egui::Pos2 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlay {
    #[default]
    None,
    Palette,
    QuickOpen,
    Busy,
    /// Search all output and notes (Ctrl+F).
    Search,
    /// A one-line text prompt (rename, new group), when the row to rename in place isn't shown.
    Prompt,
    /// The keyboard shortcut list.
    Help,
    /// A yes/no question ([`ConfirmState`]).
    Confirm,
    /// Closing the window: keep running in the tray, or quit? (`CloseAction` Ask.)
    CloseAsk,
}

/// A yes/no question: "Restart now?" after an edit that needs a new process.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmState {
    pub title: String,
    pub body: String,
    pub yes_label: String,
    pub no_label: String,
    pub yes: Box<Action>,
}

/// What an in-place text field edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineTarget {
    Session(SessionId),
    Group(GroupId),
    NewGroup,
}

/// Renaming in place (F2): the row draws a text field instead of its name.
#[derive(Debug, Clone)]
pub struct InlineEdit {
    pub target: InlineTarget,
    pub text: String,
    pub focus: bool,
    /// Frames since it opened without its row being drawn (it falls back to the prompt).
    pub unseen_frames: u8,
}

/// The settings page's sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsSection {
    #[default]
    General,
    Appearance,
    Groups,
    Mcp,
    /// Add-ons: the folder, enable and disable with consent, reload (`screens::addons`).
    Addons,
    /// Sessions and running time per command (`usage_stats`).
    Usage,
    About,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 7] = [
        SettingsSection::General,
        SettingsSection::Appearance,
        SettingsSection::Groups,
        SettingsSection::Mcp,
        SettingsSection::Addons,
        SettingsSection::Usage,
        SettingsSection::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Groups => "Group view",
            SettingsSection::Mcp => "MCP",
            SettingsSection::Addons => "Add-ons",
            SettingsSection::Usage => "Usage",
            SettingsSection::About => "About",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            SettingsSection::General => "sliders",
            SettingsSection::Appearance => "sun",
            SettingsSection::Groups => "layers",
            SettingsSection::Mcp => "server",
            SettingsSection::Addons => "extensions",
            SettingsSection::Usage => "history",
            SettingsSection::About => "terminal",
        }
    }
}

/// The settings page: the values come from tools (`settings_get`, `app_info`, `grant_list`),
/// every change is a `settings_set`.
#[derive(Debug, Clone, Default)]
pub struct SettingsState {
    pub open: bool,
    pub section: SettingsSection,
    /// `settings_get`'s map (PascalCase keys).
    pub values: serde_json::Map<String, Value>,
    pub info: Value,
    pub grants: Vec<Value>,
    /// Text fields being edited, by key (saved on Enter or when the field loses focus).
    pub drafts: HashMap<String, String>,
    /// A launch command being added.
    pub new_command: String,
    /// Settings → Usage: what `usage_stats` answered (asked again each time the section opens).
    pub usage: crate::screens::backup::UsageState,
    /// Settings → General → Import…: the backup being reviewed before it replaces anything.
    pub import: Option<crate::screens::backup::ImportReview>,
    /// Settings → General → Start-up: what `app_autostart_get` last said (`Null` until it answers).
    pub autostart: Value,
}

impl SettingsState {
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.values.get(key).and_then(Value::as_bool)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(Value::as_str)
    }

    pub fn num(&self, key: &str) -> Option<f64> {
        self.values.get(key).and_then(Value::as_f64)
    }
}

/// Settings the screens follow live, read from `settings_get` / `settings_set` replies.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    /// `dark`, `light` or `system`.
    pub theme: String,
    /// `ThemeDark` / `ThemeLight`: the theme ids drawn in dark and in light mode.
    pub theme_dark: String,
    pub theme_light: String,
    pub warn_on_close_when_busy: bool,
    pub terminal_font_size: Option<f32>,
    pub terminal_padding: f32,
    /// `ShowToastNotifications` / `ShowNotificationSound` (both off by default, as in WPF).
    pub notifications: bool,
    pub notification_sound: bool,
    /// `UiZoomPercent` (see [`crate::zoom`]).
    pub ui_zoom_percent: i32,
    /// `CloseAction` (see [`crate::background`]).
    pub close_action: csm_core::models::CloseAction,
    /// `TrayNoticeShown`: "still running in the tray" was said once, in some run.
    pub tray_notice_shown: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            theme: "dark".into(),
            theme_dark: "mocha".into(),
            theme_light: "latte".into(),
            warn_on_close_when_busy: true,
            terminal_font_size: None,
            terminal_padding: theme::space::SM,
            notifications: false,
            notification_sound: false,
            ui_zoom_percent: csm_core::models::UI_ZOOM_DEFAULT,
            close_action: csm_core::models::CloseAction::Ask,
            tray_notice_shown: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PaletteState {
    pub query: String,
    pub sel: usize,
    /// Scroll the selection into view once (after a key press), never every frame.
    pub scroll_to_sel: bool,
    pub focus_input: bool,
}

/// The search panel's state: what was asked, and what the two search tools answered.
#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub query: String,
    pub focus_input: bool,
    /// The query the results below belong to (late replies for an older one are dropped).
    pub searched: Option<String>,
    /// Searches submitted and not yet answered.
    pub pending: u8,
    /// `search_output` results: `{session_id, session_name, folder, ts, excerpt, matches, reopenable}`.
    pub output: Vec<Value>,
    /// `search_notes` results: `{folder, excerpt, matches, session_ids}`.
    pub notes: Vec<Value>,
    pub error: Option<String>,
    /// A result whose session is gone and can be reopened: (session id, name).
    pub offer: Option<(SessionId, String)>,
}

/// What `note_get` / `note_set` last told the Notes tab, by session.
#[derive(Debug, Clone, Default)]
pub struct NotesCache {
    /// The saved text, as the engine's notes store has it.
    pub saved: HashMap<SessionId, String>,
    /// How many sessions share the note (the store counts them).
    pub shared: HashMap<SessionId, usize>,
    /// Why a session has no note (SSH, no folder, no index).
    pub unavailable: HashMap<SessionId, String>,
    /// The session whose note was last asked for: it is asked again when the tab shows another one.
    pub asked: Option<SessionId>,
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
    /// The user moved through the folder list (arrows or a click) since last typing. Until then a
    /// typed absolute path wins over the suggestions it happens to match.
    pub navigated: bool,
    /// Edit mode: the session being edited (`session_edit` instead of `session_new`).
    pub edit: Option<SessionId>,
    /// Edit mode: the name field.
    pub name: String,
    /// A command that isn't in the launch-command list (edit mode keeps the session's own).
    pub custom_cmd: Option<String>,
    pub ssh_port: String,
    pub wsl_user: String,
    /// Edit mode: `session_get` filled the fields in.
    pub loaded: bool,
    /// WSL: the installed distributions (`wsl_distros`, asked once the WSL fields show).
    pub distros: Option<Vec<String>>,
    /// New, local: the sibling worktrees to start in too (their paths), for [`Self::siblings_for`].
    pub siblings: Vec<String>,
    /// The folder the sibling checkboxes are for, and when it became the chosen one (`ctx`
    /// time), and whether `worktree_list {folder}` was asked (once it stayed chosen a moment).
    pub siblings_for: Option<(String, f64, bool)>,
    /// Windows Terminal profiles (`profile_list`, asked once when profile import is on): each
    /// `{name, commandline, starting_directory, look, …}` as the tool returns it.
    pub profiles: Option<Vec<Value>>,
    /// The Profile picker's choice.
    pub profile: ProfilePick,
    /// An enabled add-on's launch profile (`addon-id.name`) the new session starts with; the
    /// engine takes the command from it (`session_new` `profile`).
    pub addon_profile: Option<String>,
}

/// What the quick-open's Profile picker says about the session's look.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ProfilePick {
    /// Edit mode: leave the session's look as it is.
    #[default]
    Keep,
    /// No look of its own (new: none; edit: clear it).
    None,
    /// This profile's look (by name, from [`QuickOpenState::profiles`]).
    Profile(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PromptKind {
    RenameSession(SessionId),
    RenameGroup(GroupId),
    NewGroup,
    /// Text to paste into every live session of the group.
    Broadcast(GroupId),
    /// A new Board task in this project (and, `assign_to`, sent to that session).
    NewTask { project: String, assign_to: Option<SessionId> },
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
    /// A sidebar row being dragged (onto a rail tab, a group header, or between rows).
    pub row: Option<SessionId>,
    /// A Free-mode pane header being dragged onto another pane's edge.
    pub pane: Option<SessionId>,
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
    pub notes_cache: NotesCache,
    pub search: SearchState,
    pub focus_terminal: Option<SessionId>,
    /// The last active session seen, to move keyboard focus when it changes.
    pub last_active: Option<SessionId>,
    pub settings: SettingsState,
    pub inline: Option<InlineEdit>,
    /// When the usage tools were last asked (the Usage tab, the sidebar chips, Settings → Usage).
    pub usage: crate::screens::usage::PollState,
    /// The Usage dashboard.
    pub usage_page: crate::screens::usage_page::PageState,
    /// The Docs tab: the doc chosen per folder, and when the docs tools were last asked.
    pub docs: crate::screens::docs::DocsUi,
    pub confirm: Option<ConfirmState>,
    /// The busy-close dialog's "Don't warn me again".
    pub dont_warn: bool,
    /// The close dialog's "Remember my choice".
    pub close_remember: bool,
    /// The sidebar is folded to the rail (a narrow window).
    pub compact: bool,
    /// Compact: the sidebar shown over the panes (the rail's last button).
    pub peek_sidebar: bool,
    /// OS notifications decided here and sent by the host ([`CsmApp::take_notifications`]).
    pub notify: crate::notify::NotifyState,
    /// The sidebar's multi-selection (Ctrl/Shift-click).
    pub selection: csm_core::sidebar_selection::Selection,
    /// The session rows the sidebar drew last frame, top to bottom (Shift-click's range).
    pub sidebar_visible: Vec<SessionId>,
    /// The last sort applied (`session_sort`): its field shows its direction and flips when
    /// picked again.
    pub sort: Option<(csm_core::session_sort::SortField, bool)>,
    /// Where the sort control drew last (the palette's "Sort sessions…" opens its menu there).
    pub sort_at: Option<egui::Pos2>,
    /// A native file dialog to show (the quick-open's Browse…, Settings' Export… / Import…). The
    /// host owns the window handle, so it shows the dialog ([`CsmApp::take_dialog_request`])
    /// and answers with [`CsmApp::dialog_answered`].
    pub dialog_request: Option<DialogRequest>,
    /// The Recent menu's selected row (Enter picks it).
    pub recent_sel: usize,
    /// `task_new` calls whose task is to be assigned (and sent) to a session once it exists:
    /// (the call's params, the session).
    pub pending_assign: Vec<(Value, SessionId)>,
    /// The add-on screens: the view strip's selection, drafts, split ratios (see `screens::addon_screen`).
    pub screens: Arc<Mutex<screens::addon_screen::ScreenUi>>,
    /// The add-on screens open this frame (fetched once per frame).
    pub screen_infos: Vec<csm_engine::ScreenInfo>,
}

/// A native dialog the UI asks the host for. The answer comes back on a later frame; the frame
/// loop never waits for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogRequest {
    /// The quick-open's Browse…: a folder, starting in this one (empty: the system's choice).
    Folder { start: String },
    /// Settings → Export…: where to save the `.csm` backup (`state_export`).
    SaveBackup { file_name: String },
    /// Settings → Import…: the `.csm` backup to read (`state_import`).
    OpenBackup,
}

/// Terminal views and the renderer, shared with torn-off windows (deferred viewports draw from
/// the same atlas). Only ever locked on the UI thread, one window at a time.
pub struct PaneHost {
    pub renderer: TerminalRenderer,
    pub views: HashMap<String, TerminalView>,
    /// The terminal font size the app started with (`--font-size`); the setting overrides it.
    pub base_font_size: f32,
    /// Space between a pane's edge and its cells (`TerminalPadding`).
    pub padding: f32,
}

impl PaneHost {
    pub fn view(&mut self, key: &str) -> &mut TerminalView {
        self.views.entry(key.to_string()).or_insert_with(|| TerminalView::new(egui::Id::new(("csm-pane", key.to_string()))))
    }
}

pub struct CsmApp {
    pub backend: Box<dyn Backend>,
    /// Where tool calls go and the tool list comes from (the engine, or a remote port).
    pub dispatch: Box<dyn ToolPort>,
    pub host: Arc<Mutex<PaneHost>>,
    pub ui: UiState,
    pub model: Arc<UiModel>,
    /// Actions raised inside torn-off windows, drained by the main frame.
    pub remote_actions: Arc<Mutex<Vec<Action>>>,
    theme_applied: Option<theme::Palette>,
    fonts_installed: bool,
    /// `app_quit` finished: the window may close now.
    pub quit_ready: bool,
    /// Settings the screens follow live.
    pub prefs: Prefs,
    /// The monitors, as the host reads them (see [`crate::placement`]); empty when unknown.
    pub monitors: crate::placement::Monitors,
    /// The main window's bounds: restored on the first frame, saved as it moves.
    pub main_window: crate::placement::WindowTracker,
    /// Hidden to the tray, and what keeps running then (see [`crate::background`]).
    pub background: crate::background::Background,
    /// A minimized or hidden start (`StartMinimized`), set by the host before the first frame.
    pub start: crate::start_window::StartUp,
    /// The host opened the main window without the OS title bar (Windows): the title bar then
    /// carries minimize, maximize and close, and the window's edges resize it
    /// ([`screens::titlebar`]).
    pub window_controls: bool,
}

impl CsmApp {
    pub fn new(port: Box<dyn ToolPort>, backend: Box<dyn Backend>, renderer: TerminalRenderer) -> Self {
        let model = backend.model();
        let mut app = Self {
            backend,
            dispatch: port,
            host: Arc::new(Mutex::new(PaneHost {
                base_font_size: renderer.fonts.size_pt,
                renderer,
                views: HashMap::new(),
                padding: theme::space::SM,
            })),
            ui: UiState::default(),
            model,
            remote_actions: Arc::new(Mutex::new(Vec::new())),
            theme_applied: None,
            fonts_installed: false,
            quit_ready: false,
            prefs: Prefs::default(),
            monitors: crate::placement::Monitors::default(),
            main_window: crate::placement::WindowTracker::new("main"),
            background: crate::background::Background::default(),
            start: crate::start_window::StartUp::default(),
            window_controls: false,
        };
        // The settings the screens follow (theme, terminal font size and padding, the busy warning).
        app.dispatch.submit("settings_get", json!({}));
        app
    }

    /// Calls the UI submitted (most recent last).
    pub fn submitted(&self) -> &[Call] {
        self.dispatch.submitted()
    }

    /// A folder dialog the quick-open asked for (Browse…): the folder to start in, empty for the
    /// system's choice. The host shows it without blocking the frame loop and answers with
    /// [`Self::folder_picked`].
    pub fn take_folder_request(&mut self) -> Option<String> {
        match self.ui.dialog_request.take() {
            Some(DialogRequest::Folder { start }) => Some(start),
            other => {
                self.ui.dialog_request = other;
                None
            }
        }
    }

    /// A native dialog the UI asked for (Browse…, Export…, Import…). The host shows it without
    /// blocking the frame loop and answers with [`Self::dialog_answered`].
    pub fn take_dialog_request(&mut self) -> Option<DialogRequest> {
        self.ui.dialog_request.take()
    }

    /// A dialog's answer (`None`: cancelled): a folder fills the quick-open, a save path exports
    /// the setup there, an opened backup is read for review (`state_import` with `dry_run`).
    pub fn dialog_answered(&mut self, request: DialogRequest, path: Option<String>) {
        match request {
            DialogRequest::Folder { .. } => self.folder_picked(path),
            DialogRequest::SaveBackup { .. } | DialogRequest::OpenBackup => screens::backup::dialog_answered(self, &request, path),
        }
    }

    /// The folder dialog's answer (`None`: cancelled). The path fills the quick-open's folder
    /// field, as if typed; Enter then starts (or, editing, saves) as usual. Dropped when the
    /// quick-open closed while the dialog was up.
    pub fn folder_picked(&mut self, path: Option<String>) {
        let q = &mut self.ui.quick;
        q.focus_input = true;
        if self.ui.overlay != Overlay::QuickOpen || q.remote {
            return;
        }
        if let Some(p) = path.filter(|p| !p.trim().is_empty()) {
            q.query = p;
            q.sel = 0;
            q.navigated = false;
        }
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
                Notice::FocusScreen(instance) => {
                    lock(&self.ui.screens).active = Some(instance);
                    self.ui.settings.open = false;
                }
                // OSC 52 (#156): egui's output sets the clipboard after the frame, no blocking.
                Notice::Clipboard(text) => ctx.copy_text(text),
                Notice::QuitReady => {
                    self.quit_ready = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    // Hidden to the tray, the close is seen on the next logic pass.
                    ctx.request_repaint();
                }
            }
        }
    }

    /// Replies the app itself follows (before the backend sees them): the settings page's values,
    /// the live prefs, the edit panel's fields, and "Restart now?" after an edit.
    fn on_app_reply(&mut self, ctx: &egui::Context, call: &Call, reply: &csm_engine::Reply) {
        // "+ task" on an idle card: once the task exists, assign it and send it.
        if call.id == "task_new"
            && let Some(i) = self.ui.pending_assign.iter().position(|(p, _)| *p == call.params)
        {
            let (_, sid) = self.ui.pending_assign.remove(i);
            if let Ok(out) = reply
                && let Some(id) = out.data["id"].as_str()
            {
                self.dispatch.submit("task_assign", json!({"task_id": id, "session_id": sid, "send": true}));
            }
        }
        let out = match reply {
            Ok(out) => out,
            Err(_) => {
                // The edit panel keeps what the snapshot told it (the demo's sessions aren't the
                // engine's, and a session can be gone).
                if call.id == "session_get" {
                    self.ui.quick.loaded = true;
                }
                if call.id == "profile_list" {
                    // Import off, or unreadable: the picker shows no profiles.
                    self.ui.quick.profiles = Some(Vec::new());
                }
                return;
            }
        };
        match call.id.as_str() {
            "settings_get" if call.params.get("key").is_none() => {
                if let Value::Object(m) = &out.data {
                    self.ui.settings.values = m.clone();
                    self.prefs_from_settings();
                }
            }
            "settings_set" => {
                if let (Some(k), Some(v)) = (out.data.get("key").and_then(Value::as_str), out.data.get("value")) {
                    self.ui.settings.values.insert(k.to_string(), v.clone());
                    self.ui.settings.drafts.remove(k);
                    self.prefs_from_settings();
                }
            }
            "app_info" => self.ui.settings.info = out.data.clone(),
            "wsl_distros" => {
                let list = out.data.as_array().cloned().unwrap_or_default();
                let names: Vec<String> = list.iter().filter_map(|d| d["name"].as_str().map(String::from)).collect();
                if self.ui.quick.distro.trim().is_empty()
                    && let Some(d) = list.iter().find(|d| d["default"] == true).or(list.first()).and_then(|d| d["name"].as_str())
                {
                    self.ui.quick.distro = d.to_string();
                }
                self.ui.quick.distros = Some(names);
            }
            "profile_list" => self.ui.quick.profiles = Some(out.data.as_array().cloned().unwrap_or_default()),
            "grant_list" => self.ui.settings.grants = out.data.as_array().cloned().unwrap_or_default(),
            "grant_revoke" => self.dispatch.submit("grant_list", json!({})),
            "session_get" => {
                let id = call.params.get("session_id").and_then(Value::as_str).unwrap_or_default();
                if self.ui.quick.edit.as_deref() == Some(id) && !self.ui.quick.loaded {
                    fill_edit_from_config(&mut self.ui.quick, &out.data["config"], &self.model.launch_commands);
                }
            }
            "session_edit" => {
                let asked_restart = call.params.get("restart").and_then(Value::as_bool).unwrap_or(false);
                if out.data.get("requires_restart").and_then(Value::as_bool) == Some(true) && !asked_restart {
                    let id = call.params.get("session_id").and_then(Value::as_str).unwrap_or_default().to_string();
                    let name = self.model.session(&id).map_or_else(|| "the session".to_string(), |s| s.name.clone());
                    self.ui.confirm = Some(ConfirmState {
                        title: "Restart now?".into(),
                        body: format!(
                            "The change to {name} needs a new process. Restart it now, or keep it running and the change applies at its next start."
                        ),
                        yes_label: "Restart now".into(),
                        no_label: "Later".into(),
                        yes: Box::new(Action::Tool("session_restart", json!({"session_id": id}))),
                    });
                    self.ui.overlay = Overlay::Confirm;
                    ctx.request_repaint();
                }
            }
            _ => {}
        }
    }

    /// Asks both search tools for the panel's query. The replies land in [`SearchState`] (see
    /// [`CsmApp::on_tool_reply`]); nothing waits.
    fn submit_search(&mut self, ctx: &egui::Context) {
        let q = self.ui.search.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let s = &mut self.ui.search;
        s.searched = Some(q.clone());
        s.pending = 2;
        s.output.clear();
        s.notes.clear();
        s.error = None;
        s.offer = None;
        self.dispatch.submit("search_output", json!({"query": q, "limit": 100}));
        self.dispatch.submit("search_notes", json!({"query": q, "limit": 30}));
        ctx.request_repaint();
    }

    /// Replies the screens act on: search results and the notes store's answers.
    fn on_tool_reply(&mut self, call: &Call, reply: &csm_engine::Reply) {
        match call.id.as_str() {
            "search_output" | "search_notes" => {
                let s = &mut self.ui.search;
                if call.params["query"].as_str() != s.searched.as_deref() {
                    return; // an older search answering late
                }
                s.pending = s.pending.saturating_sub(1);
                let rows = |o: &csm_engine::ToolOutput| o.data["results"].as_array().cloned().unwrap_or_default();
                match (call.id.as_str(), reply) {
                    ("search_output", Ok(o)) => s.output = rows(o),
                    ("search_notes", Ok(o)) => s.notes = rows(o),
                    (_, Err(e)) => s.error = Some(e.to_string()),
                    _ => {}
                }
            }
            "note_get" | "note_set" => {
                let Some(sid) = call.params["session_id"].as_str() else { return };
                let n = &mut self.ui.notes_cache;
                match reply {
                    Ok(o) => {
                        n.unavailable.remove(sid);
                        let text = if call.id == "note_get" {
                            o.data["text"].as_str().map(str::to_owned)
                        } else {
                            call.params["text"].as_str().map(str::to_owned)
                        };
                        let shared = o.data["shared_by"].as_u64().and_then(|v| usize::try_from(v).ok());
                        // Sessions in one folder share the note: they all see the new text.
                        let folder = self.model.session(sid).map(|s| s.folder.clone());
                        let same: Vec<SessionId> = match (&folder, call.id.as_str()) {
                            (Some(f), "note_set") => self.model.sessions.iter().filter(|s| &s.folder == f).map(|s| s.id.clone()).collect(),
                            _ => vec![sid.to_owned()],
                        };
                        for id in same {
                            if let Some(t) = &text {
                                n.saved.insert(id.clone(), t.clone());
                            }
                            if let Some(c) = shared {
                                n.shared.insert(id, c);
                            }
                        }
                    }
                    Err(csm_engine::ToolError::Refused(m)) => {
                        n.unavailable.insert(sid.to_owned(), m.clone());
                    }
                    Err(_) => {}
                }
            }
            _ => {}
        }
    }

    /// Follows the settings the screens apply live.
    fn prefs_from_settings(&mut self) {
        let s = &self.ui.settings;
        let mut p = self.prefs.clone();
        if let Some(t) = s.str("Theme") {
            p.theme = t.to_ascii_lowercase();
        }
        if let Some(t) = s.str("ThemeDark") {
            p.theme_dark = t.trim().to_owned();
        }
        if let Some(t) = s.str("ThemeLight") {
            p.theme_light = t.trim().to_owned();
        }
        if let Some(w) = s.bool("WarnOnCloseWhenBusy") {
            p.warn_on_close_when_busy = w;
        }
        if let Some(b) = s.bool("ShowToastNotifications") {
            p.notifications = b;
        }
        if let Some(b) = s.bool("ShowNotificationSound") {
            p.notification_sound = b;
        }
        if let Some(n) = s.num("TerminalFontPoints") {
            // 0 (the default) is the size the app started with.
            p.terminal_font_size = (n.is_finite() && (6.0..=40.0).contains(&n)).then_some(n as f32);
        }
        if let Some(n) = s.num("TerminalPadding") {
            p.terminal_padding = if n.is_finite() { (n as f32).clamp(0.0, theme::space::XXL) } else { theme::space::SM };
        }
        if let Some(n) = s.num("UiZoomPercent") {
            p.ui_zoom_percent = crate::zoom::sanitize(n);
        }
        // Its number from `settings_get`, its name as a `settings_set` echoes it.
        if let Some(a) = s.values.get("CloseAction").and_then(|v| serde_json::from_value(v.clone()).ok()) {
            p.close_action = a;
        }
        if let Some(b) = s.bool("TrayNoticeShown") {
            p.tray_notice_shown = b;
        }
        self.prefs = p;
    }

    /// Poll replies and events; refresh the snapshot.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        let (done, events) = self.dispatch.poll();
        for (call, reply) in done {
            self.on_app_reply(ctx, &call, &reply);
            self.on_tool_reply(&call, &reply);
            screens::backup::on_reply(self, ctx, &call, &reply);
            screens::startup::on_reply(self, &call, &reply);
            let n = self.backend.on_reply(&call, &reply, self.dispatch.as_ref());
            self.notices(ctx, n);
        }
        for e in events {
            match &e {
                csm_engine::Event::ConsentRequested { request } => self.ui.consents.push(request.clone()),
                csm_engine::Event::ConsentResolved { request_id } => {
                    self.ui.consents.retain(|r| r["request_id"].as_u64() != Some(*request_id));
                }
                csm_engine::Event::Error { message } => self.toast(ctx, message.clone()),
                csm_engine::Event::WindowShowRequested => {
                    crate::notify::show_window(ctx);
                    self.shown();
                }
                csm_engine::Event::NotificationTestRequested => self.test_notification(),
                // A setting changed by the CLI or an MCP client: follow it as if set here.
                csm_engine::Event::SettingChanged { key, value } => {
                    self.ui.settings.values.insert(key.clone(), value.clone());
                    self.prefs_from_settings();
                }
                _ => {}
            }
            screens::startup::on_event(self, &e);
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
                self.theme_applied = Some(theme::Palette::builtin(self.model.theme));
                self.fonts_installed = true;
            }
            ctx.request_repaint();
            return false;
        }
        let kind = self.effective_theme(ctx);
        // The chosen theme for this mode; an add-on theme arrives with the snapshot (and again
        // when its file changes), so this follows it without reading anything here.
        let (palette, _) = self.theme_palette(kind);
        if self.theme_applied != Some(palette) {
            theme::apply_palette(ctx, &palette);
            self.theme_applied = Some(palette);
        }
        // Terminal glyph weight follows the theme (measured against egui's text: see
        // `tests/text_weight.rs`); glyphs already in the atlas were made with the old curve.
        let want = crate::terminal::fonts::Coverage::for_dark_theme(kind == ThemeKind::Mocha);
        let mut host = lock(&self.host);
        let size = self.prefs.terminal_font_size.unwrap_or(host.base_font_size);
        if (host.renderer.fonts.size_pt - size).abs() > f32::EPSILON {
            // Cell metrics and glyphs are keyed by pixel size: nothing else to invalidate.
            host.renderer.fonts.size_pt = size;
        }
        host.padding = self.prefs.terminal_padding;
        if host.renderer.fonts.coverage != want {
            host.renderer.fonts.coverage = want;
            if let Some(atlas) = host.renderer.atlas.as_mut() {
                atlas.clear();
            }
        }
        true
    }

    /// The palette drawn in `kind`'s mode (`ThemeDark` or `ThemeLight`), and why it is the
    /// built-in when the chosen theme is not available.
    pub fn theme_palette(&self, kind: ThemeKind) -> (theme::Palette, Option<String>) {
        let id = if kind == ThemeKind::Mocha { &self.prefs.theme_dark } else { &self.prefs.theme_light };
        theme::resolve(kind, id, &self.model.addons)
    }

    /// The theme to draw: the snapshot's, or the OS's when the setting is `system`.
    pub fn effective_theme(&self, ctx: &egui::Context) -> ThemeKind {
        if self.prefs.theme == "system" {
            match ctx.input(|i| i.raw.system_theme) {
                Some(egui::Theme::Light) => ThemeKind::Latte,
                Some(egui::Theme::Dark) => ThemeKind::Mocha,
                None => self.model.theme,
            }
        } else {
            self.model.theme
        }
    }

    /// The scope the panes show and the layout changes go to.
    pub fn scope(&self) -> Scope {
        self.model.pane_scope()
    }

    pub fn run(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::AddonConsent(id) => {
                if let Some(a) = self.model.addons.iter().find(|a| a.id == id) {
                    self.ui.confirm = Some(ConfirmState {
                        title: format!("Enable {}?", a.name),
                        body: crate::screens::addons::consent_body(a),
                        yes_label: "Enable".into(),
                        no_label: "Cancel".into(),
                        // The digest pins the exact file the dialog listed (`addon_enable` refuses another).
                        yes: Box::new(Action::Tool("addon_enable", json!({"id": a.id, "version": a.version, "digest": a.digest.clone().unwrap_or_default()}))),
                    });
                    self.ui.overlay = Overlay::Confirm;
                    ctx.request_repaint();
                }
            }
            Action::Tool(id, params) => self.tool_action(ctx, id, params),
            Action::ToolNamed(id, params) => self.tool_action(ctx, &id, params),
            Action::OpenPalette => {
                self.ui.menu = None;
                self.ui.overlay = Overlay::Palette;
                self.ui.palette = PaletteState { focus_input: true, ..PaletteState::default() };
            }
            Action::OpenRecent => {
                self.ui.submenu = None;
                if matches!(self.ui.menu, Some(Menu::Recent { .. })) {
                    self.ui.menu = None;
                    return;
                }
                if matches!(self.ui.overlay, Overlay::Palette | Overlay::Help) {
                    self.ui.overlay = Overlay::None;
                }
                let at = ctx.data(|d| d.get_temp::<egui::Pos2>(egui::Id::new(RECENT_ANCHOR))).unwrap_or_else(|| {
                    let r = ctx.content_rect();
                    pos2(r.max.x - theme::space::SM, r.min.y + size::TITLE_BAR)
                });
                self.ui.recent_sel = 0;
                self.ui.menu = Some(Menu::Recent { at });
            }
            Action::OpenQuickOpen { group } => {
                self.ui.menu = None;
                self.ui.overlay = Overlay::QuickOpen;
                let group = group.or_else(|| match self.scope() {
                    Scope::Group(g) => Some(g),
                    Scope::All => None,
                });
                self.ui.quick = QuickOpenState { group, focus_input: true, profile: ProfilePick::None, ..QuickOpenState::default() };
            }
            Action::CloseOverlays => {
                self.ui.overlay = Overlay::None;
                self.ui.menu = None;
                self.ui.submenu = None;
                self.ui.prompt = None;
                self.ui.confirm = None;
                self.ui.peek_sidebar = false;
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
                self.start_inline(InlineTarget::Session(id), text);
            }
            Action::StartGroupRename(id) => {
                let text = self.model.group(&id).map(|g| g.name.clone()).unwrap_or_default();
                self.start_inline(InlineTarget::Group(id), text);
            }
            Action::NewGroupPrompt => self.start_inline(InlineTarget::NewGroup, String::new()),
            Action::BroadcastPrompt(g) => {
                self.ui.menu = None;
                self.ui.submenu = None;
                self.ui.prompt = Some(PromptState { kind: PromptKind::Broadcast(g), text: String::new(), focus: true });
                self.ui.overlay = Overlay::Prompt;
            }
            Action::NewTaskPrompt { project, assign_to } => {
                self.ui.menu = None;
                self.ui.submenu = None;
                self.ui.prompt = Some(PromptState { kind: PromptKind::NewTask { project, assign_to }, text: String::new(), focus: true });
                self.ui.overlay = Overlay::Prompt;
            }
            Action::NewTask { project, title, assign_to } => {
                let params = json!({"project": project, "title": title});
                if let Some(sid) = assign_to {
                    self.ui.pending_assign.push((params.clone(), sid));
                }
                self.dispatch.submit("task_new", params);
            }
            Action::CopyText(t) => {
                self.ui.menu = None;
                ctx.copy_text(t);
                self.toast(ctx, "Copied to the clipboard.");
            }
            Action::CopyAs(text, toast) => {
                self.ui.menu = None;
                ctx.copy_text(text);
                self.toast(ctx, toast);
            }
            Action::EditRuns(id) => {
                if let Some(s) = self.model.session(&id).cloned() {
                    screens::runs::open_editor(ctx, &s);
                }
                if self.model.active.as_deref() != Some(id.as_str()) {
                    self.run(ctx, Action::Tool("session_focus", json!({"session_id": id})));
                }
                self.run(ctx, Action::Tool("inspector_show", json!({"tab": "runs"})));
            }
            Action::NewWorktree(id) => {
                screens::git::open_new_worktree(ctx);
                if self.model.active.as_deref() != Some(id.as_str()) {
                    self.run(ctx, Action::Tool("session_focus", json!({"session_id": id})));
                }
                self.run(ctx, Action::Tool("inspector_show", json!({"tab": "git"})));
            }
            Action::OpenWithEditor => {
                self.ui.menu = None;
                self.ui.submenu = None;
                screens::open_with::open(ctx, &self.model.open_with);
            }
            Action::OpenSearch(q) => {
                self.ui.menu = None;
                self.ui.submenu = None;
                self.ui.overlay = Overlay::Search;
                self.ui.search.focus_input = true;
                if q.trim().is_empty() {
                    return;
                }
                self.ui.search.query = q;
                self.run(ctx, Action::RunSearch);
            }
            Action::RunSearch => self.submit_search(ctx),
            Action::PaletteTool(id) => self.palette_tool(ctx, &id),
            Action::FocusTerminal(id) => self.ui.focus_terminal = Some(id),
            Action::CloseSession(id) => {
                self.ui.menu = None;
                let Some(s) = self.model.session(&id) else { return };
                let next = screens::panes::close_action(s, !self.prefs.warn_on_close_when_busy);
                self.run(ctx, next);
            }
            Action::AskQuit => {
                self.ui.menu = None;
                // Window bounds still settling are saved now, ahead of the quit's own save, by
                // every way of quitting (the window's close, the title bar, the menu, `app_quit`).
                self.flush_placements(ctx);
                if self.model.shutdown.is_some() {
                    // Already closing: the board is up.
                    return;
                }
                let busy = self.model.sessions.iter().any(|s| matches!(s.status, Status::Working | Status::Approval | Status::Question));
                if busy && self.prefs.warn_on_close_when_busy {
                    self.ui.dont_warn = false;
                    self.ui.overlay = Overlay::Busy;
                } else {
                    self.run(ctx, Action::Tool("app_quit", json!({})));
                }
            }
            Action::CloseAnswer { tray, remember } => {
                self.run(ctx, Action::CloseOverlays);
                if remember {
                    let value = if tray { "Tray" } else { "Quit" };
                    self.run(ctx, Action::Tool("settings_set", json!({"key": "CloseAction", "value": value})));
                }
                if tray {
                    self.hide_to_tray(ctx);
                } else {
                    self.run(ctx, Action::AskQuit);
                }
            }
            Action::Consent { request_id, answer } => {
                self.ui.consents.retain(|r| r["request_id"].as_u64() != Some(request_id));
                self.run(ctx, Action::Tool("consent_answer", json!({"request_id": request_id, "answer": answer})));
            }
            Action::Toast(t) => self.toast(ctx, t),
            Action::SelectToggle(id) => {
                self.ui.menu = None;
                self.ui.selection.toggle(&id);
            }
            Action::SelectRange(id) => {
                self.ui.menu = None;
                let visible = self.ui.sidebar_visible.clone();
                self.ui.selection.range(&visible, &id, self.model.active.as_deref());
            }
            Action::ClearSelection => {
                self.ui.menu = None;
                self.ui.selection.clear();
            }
            Action::EditSession(id) => self.open_edit(&id),
            Action::OpenSettings => {
                self.ui.menu = None;
                self.ui.overlay = Overlay::None;
                self.ui.settings.open = true;
                self.ui.usage_page.open = false;
                for (id, p) in [("settings_get", json!({})), ("app_info", json!({})), ("grant_list", json!({})), ("app_autostart_get", json!({}))] {
                    self.dispatch.submit(id, p);
                }
            }
            Action::OpenUsage(drill) => {
                self.ui.menu = None;
                self.ui.overlay = Overlay::None;
                if self.model.claude_usage_on() {
                    screens::usage_page::open(self, drill);
                } else {
                    self.toast(ctx, csm_engine::addons::CLAUDE_USAGE_OFF);
                }
            }
            Action::CloseUsage => {
                self.ui.usage_page.open = false;
                if let Some(a) = self.model.active.clone() {
                    self.ui.focus_terminal = Some(a);
                }
            }
            Action::CloseSettings => {
                self.ui.settings.open = false;
                self.ui.settings.drafts.clear();
                if let Some(a) = self.model.active.clone() {
                    self.ui.focus_terminal = Some(a);
                }
            }
            Action::ShowHelp => {
                self.ui.menu = None;
                self.ui.overlay = if self.ui.overlay == Overlay::Help { Overlay::None } else { Overlay::Help };
            }
            Action::ScreenFocus(i) => {
                lock(&self.ui.screens).active = i;
                self.ui.settings.open = false;
                self.ui.usage_page.open = false;
            }
            Action::ScreenEvent { instance, event } => {
                if let Err(e) = self.backend.screen_event(&instance, event) {
                    self.toast(ctx, e);
                }
            }
            Action::ScreenTool { instance, tool, params, confirm } => self.screen_tool(ctx, &instance, tool, params, confirm),
            Action::ScreenConfirmed { tool, params } => self.dispatch.submit(&tool, params),
            Action::Confirm(c) => {
                self.ui.menu = None;
                self.ui.confirm = Some(c);
                self.ui.overlay = Overlay::Confirm;
            }
            Action::CommitInline => {
                if let Some(e) = self.ui.inline.take() {
                    let text = e.text.trim().to_string();
                    let call = match (&e.target, text.is_empty()) {
                        (_, true) => None,
                        (InlineTarget::Session(id), _) if self.model.session(id).is_some_and(|s| s.name != text) => {
                            Some(("session_rename", json!({"session_id": id, "name": text})))
                        }
                        (InlineTarget::Group(id), _) if self.model.group(id).is_some_and(|g| g.name != text) => {
                            Some(("group_rename", json!({"group_id": id, "name": text})))
                        }
                        (InlineTarget::NewGroup, _) => Some(("group_new", json!({"name": text}))),
                        _ => None,
                    };
                    if let Some((id, p)) = call {
                        self.run(ctx, Action::Tool(id, p));
                    }
                }
            }
            Action::CancelInline => self.ui.inline = None,
        }
    }

    /// Saves window bounds that are still settling (before a quit or a hide to the tray).
    pub(crate) fn flush_placements(&mut self, ctx: &egui::Context) {
        let pending = self.main_window.flush().into_iter().chain(screens::panes::flush_torn_placements(ctx));
        for p in pending.collect::<Vec<_>>() {
            self.dispatch.submit("window_placement_set", p);
        }
    }

    /// Renames in place: the row (or group header) draws a text field. When that row isn't on
    /// screen (a collapsed group, another rail tab), the one-line prompt takes over.
    fn start_inline(&mut self, target: InlineTarget, text: String) {
        self.ui.menu = None;
        self.ui.submenu = None;
        if self.ui.overlay != Overlay::None {
            self.ui.overlay = Overlay::None;
        }
        self.ui.inline = Some(InlineEdit { target, text, focus: true, unseen_frames: 0 });
    }

    /// An in-place field whose row wasn't drawn for a few frames becomes the prompt.
    fn inline_fallback(&mut self) {
        let Some(e) = &self.ui.inline else { return };
        if e.unseen_frames <= 2 {
            return;
        }
        let Some(e) = self.ui.inline.take() else { return };
        let kind = match e.target {
            InlineTarget::Session(id) => PromptKind::RenameSession(id),
            InlineTarget::Group(id) => PromptKind::RenameGroup(id),
            InlineTarget::NewGroup => PromptKind::NewGroup,
        };
        self.ui.prompt = Some(PromptState { kind, text: e.text, focus: true });
        self.ui.overlay = Overlay::Prompt;
    }

    /// The quick-open in edit mode: filled in from the snapshot now, and from `session_get`
    /// (arguments, SSH and WSL fields) when it answers.
    fn open_edit(&mut self, id: &str) {
        let Some(s) = self.model.session(id).cloned() else { return };
        self.ui.menu = None;
        self.ui.overlay = Overlay::QuickOpen;
        let mut q = QuickOpenState {
            edit: Some(id.to_string()),
            name: s.name.clone(),
            group: s.group.clone(),
            focus_input: true,
            ..QuickOpenState::default()
        };
        match s.kind {
            crate::model::Kind::Local => q.query = s.folder.clone(),
            crate::model::Kind::Ssh => {
                // `user@host:/path` until `session_get` gives the exact fields.
                q.remote = true;
                q.rkind = RemoteKind::Ssh;
                let (host, folder) = s.folder.split_once(':').unwrap_or((s.folder.as_str(), ""));
                q.host = host.trim().to_string();
                q.remote_folder = folder.trim().to_string();
            }
            crate::model::Kind::Wsl => {
                // `Distro · ~/folder` likewise.
                q.remote = true;
                q.rkind = RemoteKind::Wsl;
                let (distro, folder) = s.folder.split_once('·').unwrap_or((s.folder.as_str(), ""));
                q.distro = distro.trim().to_string();
                q.linux_folder = folder.trim().to_string();
            }
        }
        set_edit_command(&mut q, &s.command, &self.model.launch_commands);
        self.ui.quick = q;
        self.dispatch.submit("session_get", json!({"session_id": id}));
    }
    /// A button in an add-on screen runs `tool`. Direct tools go through the dispatch at once, as a
    /// palette click does; the ones that need a question (a paste into a session, a link, an
    /// editor) first show a confirm CSM draws, with the add-on's name, the tool and the exact
    /// parameters, because the button's label is the plugin's.
    fn screen_tool(&mut self, ctx: &egui::Context, instance: &str, tool: String, params: Value, confirm: bool) {
        if !confirm {
            self.dispatch.submit(&tool, params);
            return;
        }
        let info = self.ui.screen_infos.iter().find(|i| i.instance == instance).cloned();
        let (name, id) = info.map_or_else(|| ("An add-on".to_owned(), String::new()), |i| (i.addon_name, i.addon));
        let session = params.get("session_id").and_then(Value::as_str).and_then(|s| self.model.session(s)).map(|s| s.name.clone());
        let (title, body) = screens::addon_screen::confirm_text(&name, &id, &tool, &params, session.as_deref());
        let yes = Box::new(Action::ScreenConfirmed { tool, params });
        self.run(ctx, Action::Confirm(ConfirmState { title, body, yes_label: "Run it".into(), no_label: "Cancel".into(), yes }));
    }

    /// A tool picked in the palette: params are filled from context (the active session, the
    /// group the panes show); tools that need typed input open their own panel.
    fn palette_tool(&mut self, ctx: &egui::Context, id: &str) {
        self.ui.overlay = Overlay::None;
        let active = self.model.active.clone();
        let group = match self.scope() {
            Scope::Group(g) => Some(g),
            Scope::All => active.as_deref().and_then(|a| self.model.session(a)).and_then(|s| s.group.clone()),
        };
        match id {
            "session_new" => return self.run(ctx, Action::OpenQuickOpen { group: None }),
            "session_recent" => return self.run(ctx, Action::OpenRecent),
            // The look's picker is the Profile row of Edit session.
            "session_edit" | "session_look_set" => {
                if let Some(a) = active {
                    self.run(ctx, Action::EditSession(a));
                }
                return;
            }
            "settings_get" | "settings_set" | "app_info" | "grant_list" | "grant_revoke" => return self.run(ctx, Action::OpenSettings),
            "app_autostart_get" | "app_autostart_set" => return self.run(ctx, Action::OpenSettings),
            "usage_report" => return self.run(ctx, Action::OpenUsage(None)),
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
            "search_output" | "search_notes" => return self.run(ctx, Action::OpenSearch(String::new())),
            "session_sort" => {
                let at = self.ui.sort_at.unwrap_or_else(|| ctx.content_rect().center());
                return self.run(ctx, Action::OpenMenu(Menu::Sort { at }));
            }
            // Its group is optional (none = Ungrouped), so filling only the required ids would
            // silently ungroup the selection: open the selection's row menu (Move to group).
            "session_move_many" => {
                let Some(first) = self.ui.selection.ids().first().cloned() else {
                    return self.toast(ctx, "Select sessions first: Ctrl-click or Shift-click rows in the sidebar.");
                };
                let at = self.ui.sort_at.unwrap_or_else(|| ctx.content_rect().center());
                return self.run(ctx, Action::OpenMenu(Menu::Row { session: first, at }));
            }
            "app_quit" => return self.run(ctx, Action::AskQuit),
            // The 0.x editors are answered on their card, where the commands are spelled out.
            "open_with_pending" | "open_with_pending_answer" => {
                let msg = if self.model.pending_open_with.is_empty() {
                    "No editors from CodeShellManager 0.x are waiting."
                } else {
                    "The card at the bottom right lists the editors from CodeShellManager 0.x: Allow or Don't allow there."
                };
                return self.toast(ctx, msg);
            }
            // Backup and Usage: the same path as Settings' buttons (file dialog, then the review).
            "state_export" | "state_import" | "usage_stats" => return screens::backup::from_palette(self, ctx, id),
            "inspector_show" => {
                let tab = if self.model.inspector.is_some() { "hidden" } else { "notes" };
                return self.run(ctx, Action::Tool("inspector_show", json!({"tab": tab})));
            }
            "inbox_panel_set" => {
                return self.run(ctx, Action::Tool("inbox_panel_set", json!({"open": !self.model.inbox_panel.open})));
            }
            _ => {}
        }
        let Some(spec) = self.dispatch.tool(id) else { return };
        let schema = spec.input_schema.clone();
        let required: Vec<String> =
            schema["required"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
        let mut params = serde_json::Map::new();
        let mut missing = Vec::new();
        for r in &required {
            let v = match r.as_str() {
                "session_id" => active.clone().map(Value::from),
                // The bulk tools act on the sidebar's selection.
                "session_ids" => (!self.ui.selection.is_empty()).then(|| {
                    let order: Vec<&str> = self.model.sessions.iter().map(|s| s.id.as_str()).collect();
                    let first = self.ui.selection.ids().first().cloned().unwrap_or_default();
                    Value::from(self.ui.selection.targets(&first, &order))
                }),
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
        self.tool_action(ctx, id, Value::Object(params));
    }

    /// A tool call from a menu, a button or the palette: it closes the menu, then goes to the port.
    fn tool_action(&mut self, ctx: &egui::Context, id: &str, params: Value) {
        // Opening a menu item closes the menu.
        self.ui.menu = None;
        self.ui.submenu = None;
        if id == "session_sort" {
            // The sort menu shows the field sorted last, which flips when picked again.
            let field = params["by"].as_str().and_then(csm_core::session_sort::SortField::from_id);
            let desc = params["descending"].as_bool().or_else(|| field.map(|f| f.default_descending()));
            self.ui.sort = field.zip(desc);
        }
        self.dispatch.submit(id, params);
        ctx.request_repaint();
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let pressed = |ctx: &egui::Context, m: Modifiers, k: Key| ctx.input_mut(|i| consume_exact(i, m, k));
        // The Recent menu is modal from the keyboard: it takes the keys before anything else.
        if matches!(self.ui.menu, Some(Menu::Recent { .. })) && self.recent_keys(ctx) {
            return;
        }
        if pressed(ctx, cmd | Modifiers::SHIFT, Key::E) {
            self.run(ctx, Action::OpenRecent);
        }
        if pressed(ctx, cmd | Modifiers::SHIFT, Key::N) {
            let tab = if self.model.inspector == Some(crate::model::InspectorTab::Notes) { "hidden" } else { "notes" };
            self.run(ctx, Action::Tool("inspector_show", json!({"tab": tab})));
        }
        if let Some(a) = self.model.active.clone() {
            if pressed(ctx, cmd | Modifiers::SHIFT, Key::R) {
                self.run(ctx, Action::Tool("session_restart", json!({"session_id": a})));
            }
            // Ctrl+.: the active pane's ⋯ menu, where its button was last drawn.
            if pressed(ctx, cmd, Key::Period) {
                let at =
                    ctx.data(|d| d.get_temp::<egui::Pos2>(egui::Id::new(("pane-more-at", a.clone())))).unwrap_or_else(|| ctx.content_rect().center());
                self.run(ctx, Action::OpenMenu(Menu::Pane { session: a, at }));
            }
        }
        if pressed(ctx, cmd, Key::K) {
            if self.ui.overlay == Overlay::Palette {
                self.run(ctx, Action::CloseOverlays);
            } else {
                self.run(ctx, Action::OpenPalette);
            }
        }
        if pressed(ctx, cmd, Key::F) {
            if self.ui.overlay == Overlay::Search {
                self.run(ctx, Action::CloseOverlays);
            } else {
                self.run(ctx, Action::OpenSearch(String::new()));
            }
        }
        // One chord each: `consume_exact` matches the modifiers exactly, so Ctrl+T never takes
        // Ctrl+Shift+T or Ctrl+Alt+T.
        if pressed(ctx, cmd | Modifiers::ALT, Key::T) {
            if let Some(a) = self.model.active.clone() {
                self.run(ctx, Action::Tool("session_duplicate", json!({"session_id": a})));
            }
        } else if pressed(ctx, cmd | Modifiers::SHIFT, Key::T) {
            self.run(ctx, Action::Tool("session_reopen_closed", json!({})));
        } else if pressed(ctx, cmd, Key::T) {
            self.run(ctx, Action::OpenQuickOpen { group: None });
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
                self.run(ctx, Action::CloseSession(a));
            }
        }
        // F2 renames the active session in place.
        if self.ui.overlay == Overlay::None
            && self.ui.inline.is_none()
            && let Some(a) = self.model.active.clone()
            && pressed(ctx, Modifiers::NONE, Key::F2)
        {
            self.run(ctx, Action::StartRename(a));
        }
        // Ctrl+Tab / Ctrl+Shift+Tab: the next / previous live session the panes can show.
        if pressed(ctx, Modifiers::CTRL | Modifiers::SHIFT, Key::Tab) {
            self.cycle(-1);
        } else if pressed(ctx, Modifiers::CTRL, Key::Tab) {
            self.cycle(1);
        }
        // Ctrl+1…9: the rail's tabs (1 is All).
        for (n, key) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9].into_iter().enumerate() {
            if pressed(ctx, cmd, key) {
                let tab = if n == 0 { Some(Scope::All) } else { self.model.groups.get(n - 1).map(|g| Scope::Group(g.id.clone())) };
                if let Some(tab) = tab {
                    self.run(ctx, Action::Tool("view_set", json!({"rail_tab": tab.key()})));
                }
            }
        }
        if pressed(ctx, cmd, Key::Comma) {
            let a = if self.ui.settings.open { Action::CloseSettings } else { Action::OpenSettings };
            self.run(ctx, a);
        }
        crate::zoom::shortcuts(self, ctx);
        // The shortcut list: F1 and Ctrl+/ anywhere; `?` only when nothing takes typing.
        let typing = ctx.memory(|m| m.focused().is_some());
        let question = !typing
            && ctx.input_mut(|i| {
                let is_q = |e: &egui::Event| matches!(e, egui::Event::Text(t) if t == "?");
                let had = i.events.iter().any(is_q);
                i.events.retain(|e| !is_q(e));
                had
            });
        if pressed(ctx, Modifiers::NONE, Key::F1) || pressed(ctx, cmd, Key::Slash) || question {
            self.run(ctx, Action::ShowHelp);
        }
        let open = self.ui.overlay != Overlay::None || self.ui.menu.is_some();
        if open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.run(ctx, Action::CloseOverlays);
        } else if !self.ui.selection.is_empty()
            && self.ui.inline.is_none()
            && !self.terminal_focused(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.run(ctx, Action::ClearSelection);
        } else if self.ui.settings.open && self.ui.inline.is_none() && !typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.run(ctx, Action::CloseSettings);
        } else if self.ui.usage_page.open && self.ui.inline.is_none() && !typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.run(ctx, Action::CloseUsage);
        }
    }

    /// A terminal pane has keyboard focus: its keys (Esc above all) belong to the program in it.
    fn terminal_focused(&self, ctx: &egui::Context) -> bool {
        let Some(focused) = ctx.memory(|m| m.focused()) else { return false };
        lock(&self.host).views.values().any(|v| v.id() == focused)
    }

    /// Keys while the Recent menu is open: arrows move, Enter picks the selected row, a digit
    /// picks a session row, Escape or Ctrl+Shift+E closes it. Every other key is swallowed (the
    /// menu has the keyboard, as a context menu does), so nothing reaches the terminal. Returns
    /// true: the frame's other shortcuts are skipped.
    fn recent_keys(&mut self, ctx: &egui::Context) -> bool {
        let n = screens::recent::items(&self.model).len();
        let sessions = screens::recent::items(&self.model).iter().filter(|i| matches!(i, screens::recent::Item::Session(_))).count();
        let digits = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        let (close, down, up, enter, digit) = ctx.input_mut(|i| {
            let close = consume_exact(i, Modifiers::NONE, Key::Escape) || consume_exact(i, Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
            let down = consume_exact(i, Modifiers::NONE, Key::ArrowDown) || consume_exact(i, Modifiers::NONE, Key::Tab);
            let up = consume_exact(i, Modifiers::NONE, Key::ArrowUp) || consume_exact(i, Modifiers::SHIFT, Key::Tab);
            let enter = consume_exact(i, Modifiers::NONE, Key::Enter);
            let digit = digits.iter().position(|k| consume_exact(i, Modifiers::NONE, *k));
            // The menu has the keyboard: typed text and other keys go nowhere.
            i.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
            (close, down, up, enter, digit)
        });
        if close {
            self.run(ctx, Action::CloseMenu);
            return true;
        }
        if n > 0 {
            if down {
                self.ui.recent_sel = (self.ui.recent_sel.min(n - 1) + 1) % n;
            }
            if up {
                self.ui.recent_sel = (self.ui.recent_sel.min(n - 1) + n - 1) % n;
            }
        }
        let pick = match (enter, digit) {
            (_, Some(d)) => Some(screens::recent::index_for_digit(d)).filter(|i| *i < sessions),
            (true, None) => (n > 0).then(|| self.ui.recent_sel.min(n - 1)),
            _ => None,
        };
        if let Some(a) = pick.and_then(|i| screens::recent::items(&self.model).get(i).map(screens::recent::Item::action)) {
            self.run(ctx, a);
        }
        if down || up || pick.is_some() {
            ctx.request_repaint();
        }
        true
    }

    /// Focuses the live session `step` places after (or before) the active one, in the order the
    /// sidebar shows the panes' scope.
    fn cycle(&mut self, step: i32) {
        let scope = self.scope();
        let live: Vec<SessionId> = self.model.scope_sessions(&scope).into_iter().filter(|s| s.status.is_live()).map(|s| s.id.clone()).collect();
        if live.is_empty() {
            return;
        }
        let n = live.len() as i32;
        let at = self.model.active.as_ref().and_then(|a| live.iter().position(|x| x == a)).map_or(-1, |i| i as i32);
        let next = if at < 0 { if step > 0 { 0 } else { n - 1 } } else { (at + step).rem_euclid(n) };
        if let Some(id) = live.get(next as usize) {
            self.dispatch.submit("session_focus", json!({"session_id": id}));
        }
    }

    /// One frame, in `ui` (the root of the main window).
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // Polling the engine, notifications and the window's close request (quit, or hide to
        // the tray): `logic`, which runs while hidden too.
        self.logic_in_frame(&ctx);
        screens::usage::poll(self, &ctx);
        screens::addon_screen::tick(self);
        let min = egui::vec2(size::WINDOW_MIN[0], size::WINDOW_MIN[1]);
        self.main_window_step(&ctx, min);
        if !self.apply_theme(&ctx) {
            return;
        }
        crate::zoom::apply(&ctx, self.prefs.ui_zoom_percent);
        let remote: Vec<Action> = std::mem::take(&mut *lock(&self.remote_actions));
        for a in remote {
            self.run(&ctx, a);
        }
        let model = &self.model;
        self.ui.selection.retain(|id| model.session(id).is_some());
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
        // A narrow window folds the sidebar to the rail (which every group view then shows); the
        // rail's last button opens the sidebar over the panes.
        self.ui.compact = full.width() < size::COMPACT_W;
        let mut x = body.min.x;
        if self.model.group_mode == crate::model::GroupMode::Rail || self.ui.compact {
            let rail = Rect::from_min_max(pos2(x, body.min.y), pos2(x + size::RAIL, body.max.y));
            ui.scope_builder(UiBuilder::new().max_rect(rail), |ui| screens::sidebar::rail(ui, self, &mut actions));
            x = rail.max.x;
        }
        if let Some(e) = &mut self.ui.inline {
            e.unseen_frames = e.unseen_frames.saturating_add(1);
        }
        let sidebar_w = if self.ui.compact { 0.0 } else { size::SIDEBAR };
        let side = Rect::from_min_max(pos2(x, body.min.y), pos2(x + sidebar_w, body.max.y));
        if !self.ui.compact {
            ui.scope_builder(UiBuilder::new().max_rect(side), |ui| screens::sidebar::show(ui, self, &mut actions));
        } else {
            self.ui.peek_sidebar &= self.ui.overlay == Overlay::None;
        }
        let mut work = Rect::from_min_max(pos2(side.max.x, body.min.y), body.max);
        if self.ui.usage_page.open && !self.model.claude_usage_on() {
            // The Claude usage add-on was turned off while the dashboard was open.
            self.ui.usage_page.open = false;
        }
        if self.ui.settings.open {
            ui.scope_builder(UiBuilder::new().max_rect(work), |ui| screens::settings::show(ui, self, &mut actions));
        } else if self.ui.usage_page.open {
            ui.scope_builder(UiBuilder::new().max_rect(work), |ui| screens::usage_page::show(ui, self, &mut actions));
        } else {
            if self.model.inspector.is_some() {
                // Never narrower than a readable column, never more than 40% of the body.
                let w = size::INSPECTOR.min((body.width() * 0.4).max(size::INSPECTOR_MIN));
                let insp = Rect::from_min_max(pos2(body.max.x - w, body.min.y), body.max);
                work.max.x = insp.min.x;
                ui.scope_builder(UiBuilder::new().max_rect(insp), |ui| screens::inspector::show(ui, self, &mut actions));
            }
            ui.scope_builder(UiBuilder::new().max_rect(work), |ui| screens::panes::workspace(ui, self, &mut actions));
        }
        if self.ui.compact && self.ui.peek_sidebar {
            let peek = Rect::from_min_max(pos2(side.min.x, body.min.y), pos2(side.min.x + size::SIDEBAR, body.max.y));
            screens::sidebar::peek(&ctx, self, peek, &mut actions);
        }
        self.inline_fallback();
        screens::panes::torn_windows(&ctx, self);
        screens::addon_screen::pop_windows(&ctx, self);

        // Overlays, top-most last.
        screens::open_with::show(&ctx, self, &mut actions);
        screens::menus::show(&ctx, self, &mut actions);
        screens::palette::show(&ctx, self, &mut actions);
        if self.ui.overlay == Overlay::Search {
            screens::search::show(&ctx, self, &mut actions);
        }
        screens::overlays::show(&ctx, self, &mut actions);
        if self.window_controls {
            screens::titlebar::resize_edges(&ctx);
        }
        // A drag that left the window or was dropped nowhere ends here.
        if !ctx.input(|i| i.pointer.any_down()) {
            self.ui.drags.row = None;
            self.ui.drags.pane = None;
        }

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

/// Where the title bar's Recent button left the menu's anchor (temp data under this id).
pub const RECENT_ANCHOR: &str = "recent-anchor";

/// Consumes a key press with exactly these modifiers (Ctrl and Cmd as egui's `COMMAND`).
/// egui's `consume_shortcut` ignores extra Shift and Alt, so Ctrl+Shift+T would also be Ctrl+T;
/// here each binding gets only its own chord.
///
/// A punctuation key (`/`, `.`, `,` …) is the character, not the key cap: on many layouts it
/// needs Shift (`/` is Shift+7 on Danish and German keyboards), so for those an extra Shift is
/// ignored and Ctrl+/ still opens the shortcut list there.
pub fn consume_exact(i: &mut egui::InputState, m: Modifiers, key: Key) -> bool {
    let symbol = matches!(
        key,
        Key::Slash
            | Key::Period
            | Key::Comma
            | Key::Colon
            | Key::Semicolon
            | Key::Minus
            | Key::Plus
            | Key::Equals
            | Key::Questionmark
            | Key::Exclamationmark
            | Key::Backslash
            | Key::Pipe
            | Key::OpenBracket
            | Key::CloseBracket
            | Key::OpenCurlyBracket
            | Key::CloseCurlyBracket
            | Key::Backtick
            | Key::Quote
    );
    let matches = |pressed: &Modifiers| {
        if symbol && !m.shift && pressed.shift {
            pressed.matches_exact(Modifiers { shift: true, ..m })
        } else {
            pressed.matches_exact(m)
        }
    };
    let mut hit = false;
    i.events.retain(|e| {
        let is = matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && matches(modifiers));
        hit |= is;
        !is
    });
    hit
}

/// Selects `cmd` in the quick-open's command cycle, or keeps it as the session's own command.
fn set_edit_command(q: &mut QuickOpenState, cmd: &str, launch: &[String]) {
    let cmd = cmd.trim();
    match launch.iter().position(|c| c.trim() == cmd) {
        Some(i) => {
            q.cmd = i;
            q.custom_cmd = None;
        }
        None => {
            q.custom_cmd = (!cmd.is_empty()).then(|| cmd.to_string());
            // The session's own command is the extra entry after the launch commands.
            q.cmd = if q.custom_cmd.is_some() { launch.len() } else { 0 };
        }
    }
}

/// Fills the edit panel from `session_get`'s `config` (the file's values; untrusted shapes are
/// skipped, not fatal).
fn fill_edit_from_config(q: &mut QuickOpenState, cfg: &Value, launch: &[String]) {
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let command = s(&cfg["command"]);
    let args = s(&cfg["args"]);
    let full = if args.trim().is_empty() { command.clone() } else { format!("{command} {args}") };
    if !command.is_empty() {
        set_edit_command(q, &full, launch);
    }
    match cfg["kind"].as_str() {
        Some("ssh") => {
            q.remote = true;
            q.rkind = RemoteKind::Ssh;
            let user = s(&cfg["ssh"]["user"]);
            let host = s(&cfg["ssh"]["host"]);
            q.host = if user.is_empty() { host } else { format!("{user}@{host}") };
            q.ssh_port = cfg["ssh"]["port"].as_i64().filter(|p| *p != 22).map(|p| p.to_string()).unwrap_or_default();
            q.remote_folder = s(&cfg["ssh"]["remote_folder"]);
        }
        Some("wsl") => {
            q.remote = true;
            q.rkind = RemoteKind::Wsl;
            q.distro = s(&cfg["wsl"]["distro"]);
            q.wsl_user = s(&cfg["wsl"]["user"]);
            q.linux_folder = s(&cfg["wsl"]["folder"]);
        }
        _ => {
            q.remote = false;
            let f = s(&cfg["folder"]);
            if !f.is_empty() {
                q.query = f;
            }
        }
    }
    q.loaded = true;
}

/// Fonts, image loaders and the theme on a context. Call it when the context is created (the
/// eframe creator) so the first frame already has them; the app also does it itself if needed.
pub fn setup(ctx: &egui::Context, kind: ThemeKind) {
    theme::install_fonts(ctx);
    egui_extras::install_image_loaders(ctx);
    theme::apply(ctx, kind);
    // The zoom shortcuts are ours (`crate::zoom`): persisted, and Ctrl+_ stays the terminal's.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}

/// The window icon: the title bar's logo (a blue rounded square with a prompt), drawn at
/// `side` pixels so no image file is needed.
pub fn app_icon(side: u32) -> egui::IconData {
    let t = theme::Tokens::for_kind(ThemeKind::Mocha);
    let (bg, ink) = (t.blue, t.on_accent);
    let n = side.max(16) as f32;
    let radius = n * 0.22;
    let mut rgba = Vec::with_capacity((side * side * 4) as usize);
    // Coverage of the rounded square at a pixel centre, with a one-pixel soft edge.
    let square = |x: f32, y: f32| -> f32 {
        let dx = (x - n / 2.0).abs() - (n / 2.0 - radius);
        let dy = (y - n / 2.0).abs() - (n / 2.0 - radius);
        let d = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius;
        (0.5 - d).clamp(0.0, 1.0)
    };
    // Distance from a point to a segment, for the ">" and "_" strokes.
    let seg = |px: f32, py: f32, a: (f32, f32), b: (f32, f32)| -> f32 {
        let (vx, vy) = (b.0 - a.0, b.1 - a.1);
        let h = (((px - a.0) * vx + (py - a.1) * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
        (px - a.0 - vx * h).hypot(py - a.1 - vy * h)
    };
    let w = n * 0.085;
    let strokes = [((0.28, 0.32), (0.46, 0.50)), ((0.46, 0.50), (0.28, 0.68)), ((0.54, 0.70), (0.74, 0.70))];
    for y in 0..side {
        for x in 0..side {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let a = square(px, py);
            let d = strokes.iter().map(|(p, q)| seg(px, py, (p.0 * n, p.1 * n), (q.0 * n, q.1 * n))).fold(f32::MAX, f32::min);
            let k = (w - d + 0.5).clamp(0.0, 1.0);
            let mix = |c0: u8, c1: u8| (f32::from(c0) * (1.0 - k) + f32::from(c1) * k).round() as u8;
            rgba.extend_from_slice(&[mix(bg.r(), ink.r()), mix(bg.g(), ink.g()), mix(bg.b(), ink.b()), (a * 255.0).round() as u8]);
        }
    }
    egui::IconData { rgba, width: side, height: side }
}

/// The sizes [`app_icon_ico`] holds: Explorer, the taskbar, Start and Alt+Tab pick from these.
pub const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// [`app_icon`] as a Windows `.ico` file (one 32-bit BMP image per size in [`ICO_SIZES`]), for the
/// exe's resource and the installer's shortcuts. `apps/codeshell/assets/codeshell.ico` is this,
/// generated; a test there keeps the committed file in step with the drawing.
pub fn app_icon_ico() -> Vec<u8> {
    let images: Vec<Vec<u8>> = ICO_SIZES.iter().map(|&s| ico_bmp(&app_icon(s))).collect();
    let mut out = Vec::new();
    let count = u16::try_from(images.len()).unwrap_or(u16::MAX);
    // ICONDIR: reserved, type 1 (icon), count.
    for v in [0u16, 1, count] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let mut offset = 6 + 16 * images.len();
    for (&side, img) in ICO_SIZES.iter().zip(&images) {
        // ICONDIRENTRY: width and height (0 means 256), colours, reserved, planes, bit count,
        // bytes in the image, offset of the image.
        let dim = if side >= 256 { 0 } else { side as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

/// One `.ico` image: a BITMAPINFOHEADER (height doubled for the mask), BGRA rows bottom-up, then
/// an all-clear 1-bit AND mask (the alpha channel does the masking), rows padded to 4 bytes.
fn ico_bmp(icon: &egui::IconData) -> Vec<u8> {
    let (w, h) = (icon.width as usize, icon.height as usize);
    let mask_row = w.div_ceil(32) * 4;
    let mut out = Vec::with_capacity(40 + w * h * 4 + mask_row * h);
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(2 * h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]); // compression (BI_RGB), sizes, resolution, palette: all zero
    for y in (0..h).rev() {
        for px in icon.rgba.as_chunks::<4>().0.iter().skip(y * w).take(w) {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    out.resize(out.len() + mask_row * h, 0);
    out
}

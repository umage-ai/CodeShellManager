//! The app: one frame = poll the engine, draw the snapshot, turn clicks into actions. Every
//! action that changes anything is a tool call ([`Action::Tool`]); the rest is ephemeral UI state
//! (which menu is open, what the palette's query is).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Key, Modifiers, Rect, Ui, UiBuilder, ViewportId, pos2};
use serde_json::{Value, json};

use crate::backend::{Backend, Call, Notice, ToolPort};
use crate::focus::{self, FocusRegion};
use crate::model::{GroupId, Scope, SessionId, Status, UiModel};
use crate::remote_window::{self, ConnectDialog, LinkTarget, RemoteStatus};
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
    /// Copy what a Claude reply tool answers (`claude_reply_get`, `claude_code_block_get`): the
    /// call goes out, and its text lands on the clipboard with a toast when it answers.
    CopyClaude(&'static str, Value),
    /// "Copy reply…": the picker of Claude's recent replies and their code blocks
    /// (`claude_reply_list`), where the pane menu was.
    OpenClaudeReplies(SessionId),
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
    /// Settings on the Remote access section (the title bar's indicator, the palette).
    OpenRemoteAccess,
    /// Settings on the Add-ons section (the title bar's add-on status dot).
    OpenAddonSettings,
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
    /// "Connect to another CodeShellManager…": the dialog (`peer_connect_open`).
    OpenConnectRemote,
    /// Pair with the link and name in the dialog (`peer_client_pair`).
    ConnectRemotePair,
    /// Stop a pairing that waits for the office (`peer_client_pair_cancel`).
    ConnectRemoteCancel,
    /// Start a remote window on a paired host: a separate process with its own view state.
    ConnectRemoteOpen { host_id: String, name: String },
    /// "Forget" in the dialog's list: asks first, then `peer_host_forget`.
    ConnectRemoteForget { host_id: String, name: String },
    /// "Quit CodeShellManager on OFFICE…" in a remote window: asks first, then quits the office.
    RemoteQuitOffice,
    /// The confirmed office quit (`app_quit` on the host itself, not a disconnect).
    OfficeQuitConfirmed,
    /// A loopback link the person confirmed: opened in this laptop's browser.
    OpenLinkLocally(String),
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
    /// "Copy reply…" on a Claude pane: its recent replies and their code blocks.
    ClaudeReplies { session: SessionId, at: egui::Pos2 },
    /// The main menu: every action, grouped by area (F10, the title bar's button).
    /// Its top-left corner at `at`.
    Main { at: egui::Pos2 },
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
    /// "Connect to another CodeShellManager…": pair with a link, the paired offices.
    ConnectRemote,
}

/// A yes/no question: "Restart now?" after an edit that needs a new process.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmState {
    pub title: String,
    pub body: String,
    pub yes_label: String,
    pub no_label: String,
    pub yes: Box<Action>,
    /// A third answer, between yes and no: its label and the action it runs (the port card's
    /// "Copy link"). Closes the dialog as the others do.
    pub also: Option<(String, Box<Action>)>,
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
    /// Every keyboard shortcut, with its chords: record, clear, reset (`screens::settings`).
    Keyboard,
    Groups,
    Mcp,
    /// Add-ons: the folder, enable and disable with consent, reload (`screens::addons`).
    Addons,
    /// Let another CodeShellManager connect to this one (`peer_*`, `screens::remote`).
    Remote,
    /// Sessions and running time per command (`usage_stats`).
    Usage,
    About,
}

impl SettingsSection {
    pub const ALL: [SettingsSection; 9] = [
        SettingsSection::General,
        SettingsSection::Appearance,
        SettingsSection::Keyboard,
        SettingsSection::Groups,
        SettingsSection::Mcp,
        SettingsSection::Addons,
        SettingsSection::Remote,
        SettingsSection::Usage,
        SettingsSection::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Keyboard => "Keyboard",
            SettingsSection::Groups => "Group view",
            SettingsSection::Mcp => "MCP",
            SettingsSection::Addons => "Add-ons",
            SettingsSection::Remote => "Remote access",
            SettingsSection::Usage => "Usage",
            SettingsSection::About => "About",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            SettingsSection::General => "sliders",
            SettingsSection::Appearance => "sun",
            SettingsSection::Keyboard => "keyboard",
            SettingsSection::Groups => "layers",
            SettingsSection::Mcp => "server",
            SettingsSection::Addons => "extensions",
            SettingsSection::Remote => "wifi",
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
    /// Settings → Keyboard: the action whose next chord is being recorded (`App::capture_chord`).
    pub keymap_capture: Option<String>,
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
    /// `CleanClaudeCopies`: a selection copied from a Claude pane is cleaned up (on by default).
    pub clean_claude_copies: bool,
    /// The keyboard map: the defaults with the person's overrides (`keymap_*` tools, `csm_core::keymap`).
    pub keymap: csm_core::keymap::Keymap,
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
            clean_claude_copies: true,
            keymap: csm_core::keymap::Keymap::default(),
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
    /// The New session form open in the dialog, by (add-on id, form id), and its picked choice.
    /// A form whose choices were asked waits for the answer in the model, never here.
    pub new_form: Option<(String, String)>,
    pub new_choice: Option<String>,
    /// The typed name of the open form (its name field, when the form has one). Sent with the
    /// submit only when it is not blank.
    pub new_name: String,
    /// The form was submitted from this dialog: close it when the submit is done.
    pub new_pending: bool,
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
    /// The inspector's left edge: the width being shown.
    pub inspector_w: Option<f32>,
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
    /// The connect dialog's state while it is open ([`Overlay::ConnectRemote`]).
    pub connect: ConnectDialog,
    /// When the dialog last asked the engine for the pairing status (seconds, the frame clock).
    pub connect_polled: f64,
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
    /// The keyboard focus region (F6). `None` is the default: the active pane's terminal has the
    /// keys. A mouse press clears it.
    pub region: Option<FocusRegion>,
    /// The regions drawn last frame, with their rects, in the order drawn. F6 cycles over them and
    /// the focus ring goes round the focused one.
    pub region_rects: Vec<(FocusRegion, Rect)>,
    /// The keyboard-focusable items drawn last frame, by region: (region, widget id, selected). F6 enters a
    /// region at its selected item (else its first); a focus that moves into a region sets `region`.
    pub region_items: Vec<(FocusRegion, egui::Id, bool)>,
    /// The panes drawn last frame (live, not minimised), with their rects: Ctrl+Alt+Arrow moves
    /// between them.
    pub pane_rects: Vec<(SessionId, Rect)>,
    /// The sessions whose panes this frame draws (`Backend::show_panes` gets them at the frame's end).
    pub shown_panes: Vec<SessionId>,
    /// The last active session seen, to move keyboard focus when it changes.
    pub last_active: Option<SessionId>,
    pub settings: SettingsState,
    /// Remote access (another CodeShellManager connecting to this one): what the `peer_*` tools
    /// last said, and the enable walk-through.
    pub remote: screens::remote::RemoteState,
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
    /// The sidebar's focus order, top to bottom, as drawn last frame: each session row (with its
    /// session id) and each group header (`None`). The arrow keys move through it.
    pub sidebar_nav: Vec<(egui::Id, Option<SessionId>)>,
    /// The last sort applied (`session_sort`): its field shows its direction and flips when
    /// picked again.
    pub sort: Option<(csm_core::session_sort::SortField, bool)>,
    /// Where the sort control drew last (the palette's "Sort sessions…" opens its menu there).
    pub sort_at: Option<egui::Pos2>,
    /// A native file dialog to show (the quick-open's Browse…, Settings' Export… / Import…). The
    /// host owns the window handle, so it shows the dialog ([`CsmApp::take_dialog_request`])
    /// and answers with [`CsmApp::dialog_answered`].
    pub dialog_request: Option<DialogRequest>,
    /// The highlighted row at each open menu level (the keyboard or the pointer; see `menu_nav`).
    /// Empty: nothing highlighted. The first entry is the top level, a later one a submenu.
    pub menu_sel: Vec<usize>,
    /// `task_new` calls whose task is to be assigned (and sent) to a session once it exists:
    /// (the call's params, the session).
    pub pending_assign: Vec<(Value, SessionId)>,
    /// Claude reply calls whose text goes to the clipboard when they answer ([`Action::CopyClaude`]):
    /// (tool id, params).
    pub pending_copies: Vec<(String, Value)>,
    /// The "Copy reply…" picker's list (`claude_reply_list`) for a session: `None` while asked.
    pub claude_replies: Option<ClaudeReplies>,
    /// The add-on screens: the view strip's selection, drafts, split ratios (see `screens::addon_screen`).
    pub screens: Arc<Mutex<screens::addon_screen::ScreenUi>>,
    /// The add-on screens open this frame (fetched once per frame).
    pub screen_infos: Vec<csm_engine::ScreenInfo>,
}

impl UiState {
    /// Whether the active pane's terminal may take the keyboard: no region has it, or the panes
    /// region does.
    pub fn terminal_takes_focus(&self) -> bool {
        self.region.is_none_or(|r| r == FocusRegion::Panes)
    }
}

/// The "Copy reply…" picker's list of a session's replies (`claude_reply_list`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeReplies {
    pub session: SessionId,
    /// The replies (newest first), or why there are none; `None` while asked.
    pub rows: Option<Result<Vec<Value>, String>>,
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
    /// `CleanClaudeCopies`: copies from a Claude pane are cleaned up.
    pub clean_claude_copies: bool,
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
    /// The host and link of a remote window (`Backend::remote`, read once a frame); `None` for
    /// the laptop's own window.
    pub remote: Option<RemoteStatus>,
    /// Set when the window should close now: a remote window's Quit (it disconnects; the office
    /// keeps running). The host reads it once a frame and closes the window.
    pub disconnect: bool,
    /// What a remote window does outside the UI: open a link in this laptop's browser, start
    /// another remote window. The app's own window sets these; tests replace them.
    pub hooks: RemoteHooks,
}

/// The effects a remote window has on this laptop, behind closures so a test can record them. A
/// remote window to a paired host is started through the engine (`remote_window_open`, whose
/// launcher the app wires in), not here.
#[derive(Clone)]
pub struct RemoteHooks {
    /// Opens an http or https link in this laptop's browser (checked by the caller).
    pub open_link: Arc<dyn Fn(String) + Send + Sync>,
}

impl Default for RemoteHooks {
    fn default() -> Self {
        Self { open_link: csm_engine::real_url_opener() }
    }
}

impl std::fmt::Debug for RemoteHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHooks").finish_non_exhaustive()
    }
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
                clean_claude_copies: true,
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
            remote: None,
            disconnect: false,
            hooks: RemoteHooks::default(),
        };
        // The settings the screens follow (theme, terminal font size and padding, the busy warning).
        app.dispatch.submit("settings_get", json!({}));
        // Whether remote access is on (the title bar's indicator). A read; off and unavailable
        // both answer quietly.
        app.dispatch.submit("peer_status", json!({}));
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
        if self.remote.is_some() {
            // A remote window never opens a dialog on this laptop (`remote_step` says so).
            return None;
        }
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
        if self.remote.is_some() {
            return None;
        }
        self.ui.dialog_request.take()
    }

    /// Once a frame, for a remote window: its host and link (the strip and the title), and the
    /// file dialogs refused (a native dialog would browse this laptop, not the office).
    fn remote_step(&mut self, ctx: &egui::Context) {
        let status = self.backend.remote();
        if status != self.remote {
            if let Some(r) = &status {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(r.window_title()));
            }
            self.remote = status;
        }
        if self.remote.is_some() && self.ui.dialog_request.take().is_some() {
            self.toast(ctx, remote_window::refusal_text("A file dialog"));
        }
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
        if self.claude_reply(ctx, call, reply) {
            return;
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
        also: None,
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

    /// The Claude reply tools' answers: the "Copy reply…" picker's list, and the text of a copy
    /// asked for ([`Action::CopyClaude`]) onto the clipboard, with a toast. True when handled.
    fn claude_reply(&mut self, ctx: &egui::Context, call: &Call, reply: &csm_engine::Reply) -> bool {
        match call.id.as_str() {
            "claude_reply_list" => {
                let sid = call.params["session_id"].as_str().unwrap_or_default();
                if let Some(r) = self.ui.claude_replies.as_mut()
                    && r.session == sid
                {
                    r.rows = Some(match reply {
                        Ok(o) => Ok(o.data["replies"].as_array().cloned().unwrap_or_default()),
                        Err(e) => Err(e.to_string()),
                    });
                    ctx.request_repaint();
                }
                true
            }
            "claude_reply_get" | "claude_code_block_get" => {
                let Some(i) = self.ui.pending_copies.iter().position(|(id, p)| *id == call.id && *p == call.params) else {
                    return false;
                };
                self.ui.pending_copies.remove(i);
                // An error: the backend toasts it ("not available for this session…").
                if let Ok(o) = reply {
                    let block = call.id == "claude_code_block_get";
                    let text = o.data[if block { "code" } else { "text" }].as_str().unwrap_or_default();
                    let lines = o.data["lines"].as_u64().unwrap_or(0);
                    ctx.copy_text(text.to_owned());
                    let n = if lines == 1 { "1 line".to_owned() } else { format!("{lines} lines") };
                    let what = if block {
                        format!("code block {}", o.data["block_index"].as_u64().unwrap_or(0) + 1)
                    } else if o.data["index"].as_u64() == Some(0) {
                        "Claude's last reply".to_owned()
                    } else {
                        "Claude's reply".to_owned()
                    };
                    self.toast(ctx, format!("Copied {what} ({n})"));
                }
                true
            }
            _ => false,
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

    /// A link to a loopback port of an SSH session's host was not opened (`open_url` answered
    /// `needs_forward`): the card offers a forward that opens it, or the link to copy.
    fn port_card(&mut self, data: &Value) {
        let port = data["remote_port"].as_u64().and_then(|p| u16::try_from(p).ok()).unwrap_or(0);
        let destination = data["destination"].as_str().unwrap_or("the SSH host").to_owned();
        let session = data["session_id"].as_str().unwrap_or_default().to_owned();
        let link = data["url"].as_str().unwrap_or_default().to_owned();
        self.ui.confirm = Some(ConfirmState {
            title: format!("Port {port} is on {destination}, not on this computer"),
            body: "The link points at that machine's own port, so it would open nothing here. Forward the port over SSH to open it on this computer, or copy the link.".into(),
            yes_label: "Forward and open".into(),
            no_label: "Cancel".into(),
            yes: Box::new(Action::Tool("session_port_forward", json!({"session_id": session, "remote_port": port, "open": true}))),
            also: Some(("Copy link".into(), Box::new(Action::CopyText(link)))),
        });
        self.ui.overlay = Overlay::Confirm;
    }

    /// Replies the screens act on: search results and the notes store's answers.
    fn on_tool_reply(&mut self, call: &Call, reply: &csm_engine::Reply) {
        match call.id.as_str() {
            "open_url" => {
                if let Ok(out) = reply
                    && out.data["needs_forward"].as_bool() == Some(true)
                {
                    self.port_card(&out.data);
                }
            }
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
        if let Some(b) = s.bool("CleanClaudeCopies") {
            p.clean_claude_copies = b;
        }
        // The keyboard map follows the overrides the engine saved (a damaged entry is left out).
        if let Some(v) = s.values.get("Keymap") {
            p.keymap = csm_core::keymap::Keymap::new(&csm_core::keymap::overrides_from_value(v));
        }
        self.prefs = p;
    }

    /// Settings → Keyboard: while a chord is being recorded, the next key press is that chord, and
    /// the shortcuts do not see it. Escape with no modifier, or a click, cancels. Returns whether the
    /// keys were taken (the shortcuts then stop for this frame).
    fn capture_chord(&mut self, ctx: &egui::Context) -> bool {
        // The capture ends with the settings page: a close by any route must not keep the keys.
        if !self.ui.settings.open {
            self.ui.settings.keymap_capture = None;
        }
        let Some(id) = self.ui.settings.keymap_capture.clone() else { return false };
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.ui.settings.keymap_capture = None;
            return false;
        }
        let press = ctx.input_mut(|i| {
            let mut found = None;
            i.events.retain(|e| match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    found = found.or(Some((*key, *modifiers)));
                    false
                }
                egui::Event::Text(_) => false,
                _ => true,
            });
            found
        });
        let Some((key, modifiers)) = press else { return true };
        if key == Key::Escape && !modifiers.any() {
            self.ui.settings.keymap_capture = None;
            return true;
        }
        // A key the map cannot name keeps the capture open for the next press.
        if let Some(chord) = crate::keys::chord_of(key, modifiers) {
            self.ui.settings.keymap_capture = None;
            self.run(ctx, Action::Tool("keymap_set", json!({"action": id, "chords": [chord.to_string()]})));
        }
        true
    }

    /// Poll replies and events; refresh the snapshot.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) {
        let (done, events) = self.dispatch.poll();
        for (call, reply) in done {
            self.on_app_reply(ctx, &call, &reply);
            self.on_tool_reply(&call, &reply);
            screens::backup::on_reply(self, ctx, &call, &reply);
            screens::connect_remote::on_reply(self, ctx, &call, &reply);
            // The office quit: this window has nothing left to show, so it disconnects.
            if call.id == "app_quit" && self.remote.is_some() && reply.is_ok() {
                self.disconnect = true;
            }
            screens::startup::on_reply(self, &call, &reply);
            screens::remote::on_reply(self, ctx, &call, &reply);
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
        host.clean_claude_copies = self.prefs.clean_claude_copies;
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
        if self.refused_in_remote(ctx, &action) {
            return;
        }
        match action {
            Action::OpenConnectRemote => {
                self.ui.connect = ConnectDialog::default();
                self.ui.overlay = Overlay::ConnectRemote;
                self.dispatch.submit("peer_hosts_list", json!({}));
                self.dispatch.submit("peer_client_pair_status", json!({}));
            }
            Action::ConnectRemotePair => {
                if !self.ui.connect.can_pair() {
                    return;
                }
                let mut params = json!({"link": self.ui.connect.link.trim()});
                let name = self.ui.connect.name.trim();
                if !name.is_empty() {
                    params["name"] = json!(name);
                }
                self.ui.connect.error = None;
                self.ui.connect.phase = remote_window::PairPhase::Connecting;
                self.ui.connect_polled = 0.0;
                self.dispatch.submit("peer_client_pair", params);
            }
            Action::ConnectRemoteCancel => self.dispatch.submit("peer_client_pair_cancel", json!({})),
            Action::ConnectRemoteOpen { host_id, name: _ } => {
                self.ui.overlay = Overlay::None;
                // The engine's `remote_window_open` starts the window (and says so, or why not).
                self.dispatch.submit("remote_window_open", json!({"host_id": host_id}));
            }
            Action::ConnectRemoteForget { host_id, name } => {
                self.ui.confirm = Some(ConfirmState {
                    also: None,
                    title: format!("Forget {name}?"),
                    body: format!(
                        "This laptop removes {name} and the host key it pinned, and stops connecting there until it is paired again. {name} keeps this laptop's device key, so nothing changes on {name}."
                    ),
                    yes_label: "Forget".into(),
                    no_label: "Keep".into(),
                    yes: Box::new(Action::Tool("peer_host_forget", json!({"host_id": host_id}))),
                });
                self.ui.overlay = Overlay::Confirm;
            }
            Action::RemoteQuitOffice => {
                let name = self.remote.as_ref().map_or_else(|| "the office".to_owned(), |r| r.host_name.clone());
                self.ui.confirm = Some(ConfirmState {
                    also: None,
                    title: format!("Quit CodeShellManager on {name}?"),
                    body: format!("Every session on {name} is closed and {name}'s app quits, so nothing runs there until it is started again. This window disconnects afterwards."),
                    yes_label: "Quit on the office".into(),
                    no_label: "Cancel".into(),
                    yes: Box::new(Action::OfficeQuitConfirmed),
                });
                self.ui.overlay = Overlay::Confirm;
            }
            Action::OfficeQuitConfirmed => {
                self.ui.overlay = Overlay::None;
                // The host's own app_quit: a forwarded call, not this window's disconnect.
                self.dispatch.submit("app_quit", json!({}));
                let name = self.remote.as_ref().map_or_else(|| "the office".to_owned(), |r| r.host_name.clone());
                self.toast(ctx, format!("Quitting CodeShellManager on {name}."));
            }
            Action::OpenLinkLocally(url) => {
                self.ui.overlay = Overlay::None;
                (self.hooks.open_link)(url.clone());
                self.toast(ctx, format!("Opening {url} in this laptop's browser."));
            }
            Action::AddonConsent(id) => {
                if let Some(a) = self.model.addons.iter().find(|a| a.id == id) {
                    self.ui.confirm = Some(ConfirmState {
        also: None,
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
                self.ui.menu_sel = vec![0];
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
                self.ui.menu_sel.clear();
                self.ui.menu = if self.ui.menu.as_ref() == Some(&m) { None } else { Some(m) };
            }
            Action::CloseMenu => {
                self.ui.menu = None;
                self.ui.submenu = None;
                self.ui.menu_sel.clear();
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
            Action::CopyClaude(id, params) => {
                self.ui.menu = None;
                self.ui.submenu = None;
                self.ui.pending_copies.push((id.to_owned(), params.clone()));
                self.dispatch.submit(id, params);
            }
            Action::OpenClaudeReplies(session) => {
                // Where the pane menu was, else where the pane's ⋯ button was drawn.
                let at = match &self.ui.menu {
                    Some(Menu::Pane { at, .. } | Menu::Row { at, .. }) => *at,
                    _ => ctx
                        .data(|d| d.get_temp::<egui::Pos2>(egui::Id::new(("pane-more-at", session.clone()))))
                        .unwrap_or_else(|| ctx.content_rect().center()),
                };
                self.ui.submenu = None;
                self.ui.claude_replies = Some(ClaudeReplies { session: session.clone(), rows: None });
                self.dispatch.submit("claude_reply_list", json!({"session_id": session, "limit": 10}));
                self.ui.menu_sel.clear();
                self.ui.menu = Some(Menu::ClaudeReplies { session, at });
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
            Action::AskQuit if self.remote.is_some() => {
                // A remote window's Quit disconnects. It never quits the office.
                self.ui.menu = None;
                self.disconnect = true;
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
            Action::OpenRemoteAccess => {
                self.run(ctx, Action::OpenSettings);
                self.ui.settings.section = SettingsSection::Remote;
                screens::remote::opened(self, ctx);
            }
            Action::OpenAddonSettings => {
                self.run(ctx, Action::OpenSettings);
                self.ui.settings.section = SettingsSection::Addons;
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
                self.ui.settings.keymap_capture = None;
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
        self.run(ctx, Action::Confirm(ConfirmState { also: None, title, body, yes_label: "Run it".into(), no_label: "Cancel".into(), yes }));
    }

    /// A tool picked in the palette: params are filled from context (the active session, the
    /// group the panes show); tools that need typed input open their own panel.
    fn palette_tool(&mut self, ctx: &egui::Context, id: &str) {
        self.ui.overlay = Overlay::None;
        // The two rows only a remote window lists (the palette adds them; see `palette::rows`).
        match id {
            "remote_disconnect" => return self.run(ctx, Action::AskQuit),
            "remote_quit_office" => return self.run(ctx, Action::RemoteQuitOffice),
            "peer_connect_open" => return self.run(ctx, Action::OpenConnectRemote),
            _ => {}
        }
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
            // Turning remote access on, pairing, revoking and the relay (its fields are on the page) are on their page.
            // Turning it off is one click and needs nothing else, so it runs as it is.
            "peer_server_enable" | "peer_pair_start" | "peer_pair_answer" | "peer_device_revoke" | "peer_interfaces" | "peer_status" | "peer_device_list" | "peer_relay_set" => {
                return self.run(ctx, Action::OpenRemoteAccess);
            }
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
    /// A remote window refuses the tools whose effect is on this laptop (see
    /// [`remote_window::is_local_only`]) with a toast, before they run. `true`: refused.
    fn refused_in_remote(&mut self, ctx: &egui::Context, action: &Action) -> bool {
        if self.remote.is_none() {
            return false;
        }
        let id = match action {
            Action::Tool(id, _) => id,
            Action::ToolNamed(id, _) | Action::PaletteTool(id) => id.as_str(),
            Action::OpenWithEditor => "open_with",
            Action::OpenConnectRemote => "peer_connect_open",
            _ => return false,
        };
        if !remote_window::is_local_only(id) {
            return false;
        }
        let what = self.dispatch.tool(id).map_or_else(|| id.to_owned(), |t| t.title);
        self.toast(ctx, remote_window::refusal_text(&what));
        true
    }

    /// A link Ctrl+clicked in a remote window opens in this laptop's browser, after the same
    /// http/https check the engine makes. A loopback link asks first: it names the office's machine.
    fn open_link_in_remote(&mut self, ctx: &egui::Context, params: &Value) {
        let url = params.get("url").and_then(Value::as_str).unwrap_or("");
        let host = self.remote.as_ref().map_or_else(|| "the office".to_owned(), |r| r.host_name.clone());
        match remote_window::check_link(url) {
            None => self.toast(ctx, "Only absolute http and https links open."),
            Some(LinkTarget::Public(u)) => self.run(ctx, Action::OpenLinkLocally(u)),
            Some(LinkTarget::Localhost(u)) => {
                let (title, body) = remote_window::localhost_warning(&host, &u);
                self.ui.confirm = Some(crate::app::ConfirmState {
                    also: None,
                    title,
                    body,
                    yes_label: "Open on this laptop".into(),
                    no_label: "Cancel".into(),
                    yes: Box::new(Action::OpenLinkLocally(u)),
                });
                self.ui.overlay = Overlay::Confirm;
            }
        }
    }

    fn tool_action(&mut self, ctx: &egui::Context, id: &str, params: Value) {
        // Opening a menu item closes the menu.
        self.ui.menu = None;
        self.ui.submenu = None;
        if self.remote.is_some() {
            // Quit in a remote window disconnects; links open on this laptop.
            if id == "app_quit" {
                self.disconnect = true;
                return;
            }
            if id == "open_url" {
                return self.open_link_in_remote(ctx, &params);
            }
        }
        if id == "session_sort" {
            // The sort menu shows the field sorted last, which flips when picked again.
            let field = params["by"].as_str().and_then(csm_core::session_sort::SortField::from_id);
            let desc = params["descending"].as_bool().or_else(|| field.map(|f| f.default_descending()));
            self.ui.sort = field.zip(desc);
        }
        self.dispatch.submit(id, params);
        ctx.request_repaint();
    }

    /// Whether the keyboard map's chords of `id` were pressed in this frame, taking the key press
    /// (`keys::consume`). An action with no chords never fires.
    pub(crate) fn bound(&self, ctx: &egui::Context, id: &str) -> bool {
        let chords = self.prefs.keymap.chords(id);
        !chords.is_empty() && ctx.input_mut(|i| chords.iter().any(|c| crate::keys::consume(i, c)))
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // The settings' key capture takes the keys first: the next chord is the one recorded.
        if self.capture_chord(ctx) {
            return;
        }
        self.sync_region_to_focus(ctx);
        // The main menu: F10 (the keymap's `ui.main_menu`).
        if self.bound(ctx, "ui.main_menu") {
            self.toggle_main_menu(ctx);
        }
        // An open menu takes its keys before anything else: it is modal from the keyboard
        // (`screens::menus::menu_keys`). Ctrl chords it does not read still reach the shortcuts.
        // A dialog press stored by an earlier frame is dropped too: the menu, not the dialog, has the keys.
        if self.ui.menu.is_some() {
            screens::overlays::clear_presses(ctx);
        }
        if self.ui.menu.is_some()
            && let Some(actions) = screens::menus::menu_keys(ctx, self)
        {
            for a in actions {
                self.run(ctx, a);
            }
            return;
        }
        // A modal dialog reads its own keys first: Tab between its controls, Enter, Space on its
        // checkbox, and Escape to cancel (`screens::overlays::take_keys`). Other chords still reach
        // the shortcuts below.
        if screens::overlays::take_keys(ctx, self) {
            self.run(ctx, Action::CloseOverlays);
        }
        // Shift+F10 opens the menu of the focused item, outside the terminal (which keeps the key
        // when it has the focus). A region with no menu leaves the key alone.
        if !self.ui.terminal_takes_focus()
            && self.ui.overlay == Overlay::None
            && self.ui.menu.is_none()
            && self.ui.inline.is_none()
            && let Some(menu) = self.focused_item_menu(ctx)
            && self.bound(ctx, "ui.menu_focused")
        {
            self.run(ctx, Action::OpenMenu(menu.clone()));
            self.ui.menu_sel = screens::menus::first_highlight(self, &menu);
        }
        if self.bound(ctx, "session_recent") {
            self.run(ctx, Action::OpenRecent);
        }
        if self.bound(ctx, "ui.inspector_notes") {
            let tab = if self.model.inspector == Some(crate::model::InspectorTab::Notes) { "hidden" } else { "notes" };
            self.run(ctx, Action::Tool("inspector_show", json!({"tab": tab})));
        }
        if let Some(a) = self.model.active.clone() {
            if self.bound(ctx, "session_restart") {
                self.run(ctx, Action::Tool("session_restart", json!({"session_id": a})));
            }
            // Ctrl+Shift+C: Claude's last reply. A focused terminal gets it as a copy (egui sends
            // no key with it) and handles it there: a selection is copied instead. Elsewhere it is
            // this key, or a copy nothing focused takes.
            if self.model.session(&a).is_some_and(crate::model::SessionView::is_claude) {
                let nothing_focused = ctx.memory(|m| m.focused().is_none());
                let copy = self.bound(ctx, "claude_reply_get")
                    || (nothing_focused
                        && ctx.input_mut(|i| {
                            let had = i.modifiers.shift && i.events.iter().any(|e| matches!(e, egui::Event::Copy));
                            if had {
                                i.events.retain(|e| !matches!(e, egui::Event::Copy));
                            }
                            had
                        }));
                if copy {
                    self.run(ctx, Action::CopyClaude("claude_reply_get", json!({"session_id": a})));
                }
            }
            // Ctrl+.: the active pane's ⋯ menu, where its button was last drawn.
            if self.bound(ctx, "ui.pane_menu") {
                let at =
                    ctx.data(|d| d.get_temp::<egui::Pos2>(egui::Id::new(("pane-more-at", a.clone())))).unwrap_or_else(|| ctx.content_rect().center());
                self.run(ctx, Action::OpenMenu(Menu::Pane { session: a, at }));
            }
        }
        if self.bound(ctx, "ui.palette") {
            if self.ui.overlay == Overlay::Palette {
                self.run(ctx, Action::CloseOverlays);
            } else {
                self.run(ctx, Action::OpenPalette);
            }
        }
        if self.bound(ctx, "search_output") {
            if self.ui.overlay == Overlay::Search {
                self.run(ctx, Action::CloseOverlays);
            } else {
                self.run(ctx, Action::OpenSearch(String::new()));
            }
        }
        // One chord each: the map matches the modifiers exactly, so Ctrl+T never takes
        // Ctrl+Shift+T or Ctrl+Alt+T.
        if self.bound(ctx, "session_duplicate") {
            if let Some(a) = self.model.active.clone() {
                self.run(ctx, Action::Tool("session_duplicate", json!({"session_id": a})));
            }
        } else if self.bound(ctx, "session_reopen_closed") {
            self.run(ctx, Action::Tool("session_reopen_closed", json!({})));
        } else if self.bound(ctx, "session_new") {
            self.run(ctx, Action::OpenQuickOpen { group: None });
        }
        if self.bound(ctx, "inbox_next") {
            self.run(ctx, Action::Tool("inbox_next", json!({})));
        }
        if self.bound(ctx, "inspector_show") {
            let tab = if self.model.inspector.is_some() { "hidden" } else { "notes" };
            self.run(ctx, Action::Tool("inspector_show", json!({"tab": tab})));
        }
        if let Some(a) = self.model.active.clone() {
            if self.bound(ctx, "run_stop") {
                self.run(ctx, Action::Tool("run_stop", json!({"session_id": a})));
            } else if self.bound(ctx, "run_start") {
                self.run(ctx, Action::Tool("run_start", json!({"session_id": a})));
            }
            if self.bound(ctx, "session_close") {
                self.run(ctx, Action::CloseSession(a));
            }
        }
        // Keyboard focus (docs/design/keyboard.md, sections 2 and 6). A mouse press leaves the
        // region mode: the mouse takes the focus where it lands. F6 and Ctrl+Alt+Arrow wait while
        // an overlay, a menu or an inline edit has the keyboard.
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.ui.region = None;
        }
        if self.ui.overlay == Overlay::None && self.ui.menu.is_none() && self.ui.inline.is_none() {
            if self.bound(ctx, "ui.region_next") {
                self.cycle_region(ctx, 1);
            } else if self.bound(ctx, "ui.region_prev") {
                self.cycle_region(ctx, -1);
            }
            let arrows = [
                ("ui.pane_left", focus::Dir::Left),
                ("ui.pane_right", focus::Dir::Right),
                ("ui.pane_up", focus::Dir::Up),
                ("ui.pane_down", focus::Dir::Down),
            ];
            for (id, dir) in arrows {
                if self.bound(ctx, id) {
                    self.move_pane_focus(dir);
                }
            }
        }
        // F2 renames the active session in place. A focused sidebar row or group header renames
        // its own item instead (the sidebar takes the key).
        if self.ui.overlay == Overlay::None
            && self.ui.inline.is_none()
            && let Some(a) = self.model.active.clone()
            && !self.sidebar_item_focused(ctx)
            && self.bound(ctx, "session_rename")
        {
            self.run(ctx, Action::StartRename(a));
        }
        // Ctrl+Tab / Ctrl+Shift+Tab: the next / previous live session the panes can show.
        if self.bound(ctx, "ui.session_prev") {
            self.cycle(-1);
        } else if self.bound(ctx, "ui.session_next") {
            self.cycle(1);
        }
        // Ctrl+1…9: the rail's tabs (1 is All).
        for (n, id) in RAIL_TAB_IDS.into_iter().enumerate() {
            if self.bound(ctx, id) {
                let tab = if n == 0 { Some(Scope::All) } else { self.model.groups.get(n - 1).map(|g| Scope::Group(g.id.clone())) };
                if let Some(tab) = tab {
                    self.run(ctx, Action::Tool("view_set", json!({"rail_tab": tab.key()})));
                }
            }
        }
        // Ctrl+PgUp / Ctrl+PgDn: the add-on screen tab before / after the shown one. Ctrl+Alt+PgUp /
        // PgDn: the rail tab before / after (`rail_tab_prev`, `rail_tab_next`). docs/design/keyboard.md, 3.
        // Only with an add-on screen open (two tabs or more): with none, the chords are the terminal's
        // (vim's tabprev and tabnext are Ctrl+PgUp and Ctrl+PgDn).
        let screen_tabs = self.ui.screen_infos.iter().any(|i| !i.popped_out);
        if screen_tabs && self.bound(ctx, "ui.screen_tab_next") {
            self.screen_tab_step(ctx, true);
        }
        if screen_tabs && self.bound(ctx, "ui.screen_tab_prev") {
            self.screen_tab_step(ctx, false);
        }
        if self.bound(ctx, "rail_tab_next") {
            self.run(ctx, Action::Tool("rail_tab_next", json!({})));
        }
        if self.bound(ctx, "rail_tab_prev") {
            self.run(ctx, Action::Tool("rail_tab_prev", json!({})));
        }
        if self.bound(ctx, "ui.settings") {
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
        if self.bound(ctx, "ui.help") || question {
            self.run(ctx, Action::ShowHelp);
        }
        let open = self.ui.overlay != Overlay::None || self.ui.menu.is_some();
        if open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.run(ctx, Action::CloseOverlays);
        } else if self.ui.region.is_some_and(|r| r != FocusRegion::Panes)
            && self.ui.inline.is_none()
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            // Esc leaves a non-terminal region: the keys go back to the active pane.
            self.set_region(ctx, None);
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

    /// Opens the main menu, or closes it when it is open. It opens only over the panes: a dialog
    /// or an inline edit keeps the keyboard.
    fn toggle_main_menu(&mut self, ctx: &egui::Context) {
        if matches!(self.ui.menu, Some(Menu::Main { .. })) {
            self.run(ctx, Action::CloseMenu);
        } else if self.ui.overlay == Overlay::None && self.ui.inline.is_none() {
            let menu = Menu::Main { at: screens::menus::main_menu_at(ctx) };
            self.run(ctx, Action::OpenMenu(menu.clone()));
            self.ui.menu_sel = screens::menus::first_highlight(self, &menu);
        }
    }

    /// The menu of the focused item, for Shift+F10: the session row that has the keyboard focus
    /// (or the active session when the sidebar has the region but no row), or the group of the
    /// active rail tab. `None` where the region has no menu.
    fn focused_item_menu(&self, ctx: &egui::Context) -> Option<Menu> {
        match self.ui.region? {
            FocusRegion::SidebarList => {
                let focused = ctx.memory(|m| m.focused());
                let session = self
                    .model
                    .sessions
                    .iter()
                    .find(|s| focused == Some(egui::Id::new(("session-row", s.id.clone()))))
                    .map(|s| s.id.clone())
                    .or_else(|| self.model.active.clone())?;
                let key = egui::Id::new(("row-menu-at", session.clone()));
                let at = ctx.data(|d| d.get_temp::<egui::Pos2>(key)).unwrap_or_else(|| ctx.content_rect().center());
                Some(Menu::Row { session, at })
            }
            FocusRegion::RailTabs => {
                let at = self.ui.region_rects.iter().find(|(r, _)| *r == FocusRegion::RailTabs).map(|(_, rect)| rect.left_bottom())?;
                match self.scope() {
                    Scope::Group(group) => Some(Menu::Group { group, at }),
                    Scope::All => None,
                }
            }
            _ => None,
        }
    }

    /// Moves the keyboard focus region. `None` gives the keys back to the active pane's terminal;
    /// a region other than the panes takes them from a focused terminal.
    fn set_region(&mut self, ctx: &egui::Context, region: Option<FocusRegion>) {
        self.ui.region = region;
        if self.ui.terminal_takes_focus() {
            self.ui.focus_terminal = self.model.active.clone();
        } else if self.terminal_focused(ctx)
            && let Some(focused) = ctx.memory(|m| m.focused())
        {
            ctx.memory_mut(|m| m.surrender_focus(focused));
        }
        // The keyboard focus goes into the region too, on its entry item (the shown one, else the first),
        // so its arrow keys work at once.
        if let Some(r) = region.filter(|r| *r != FocusRegion::Panes) {
            let of_region: Vec<_> = self.ui.region_items.iter().filter(|(x, _, _)| *x == r).collect();
            if let Some((_, id, _)) = of_region.iter().find(|(_, _, on)| *on).or(of_region.first()) {
                ctx.memory_mut(|m| m.request_focus(*id));
            }
        }
        ctx.request_repaint();
    }

    /// Keeps `ui.region` on the region of the focused item when the keyboard moved the focus there (Tab,
    /// a focus request), not a mouse press, so F6 cycles from it and the ring follows the focus.
    fn sync_region_to_focus(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_pass_nr();
        let key = egui::Id::new("region-sync");
        let (prev, last_press): (Option<egui::Id>, u64) = ctx.data(|d| d.get_temp(key)).unwrap_or((None, 0));
        let press_frame = if ctx.input(|i| i.pointer.any_pressed()) { frame } else { last_press };
        let focus = ctx.memory(|m| m.focused());
        if focus != prev
            && frame.saturating_sub(press_frame) > 1
            && let Some(f) = focus
            && let Some((region, _, _)) = self.ui.region_items.iter().find(|(_, id, _)| *id == f).copied()
        {
            self.ui.region = Some(region);
        }
        ctx.data_mut(|d| d.insert_temp(key, (focus, press_frame)));
    }

    /// F6 (`step` 1) or Shift+F6 (-1): the next region that was drawn last frame, from the current
    /// one (the panes when none has the ring).
    fn cycle_region(&mut self, ctx: &egui::Context, step: i32) {
        let visible: Vec<FocusRegion> = self.ui.region_rects.iter().map(|(r, _)| *r).collect();
        let current = self.ui.region.unwrap_or(FocusRegion::Panes);
        if let Some(next) = focus::next_region(&visible, current, step) {
            self.set_region(ctx, Some(next));
        }
    }

    /// Ctrl+Alt+Arrow: focuses the pane nearest the active one in `dir`, by where the panes were
    /// drawn. The focus goes to that pane's terminal, and any region mode ends.
    fn move_pane_focus(&mut self, dir: focus::Dir) {
        let Some(active) = self.model.active.clone() else { return };
        let Some(from) = self.ui.pane_rects.iter().find(|(id, _)| *id == active).map(|(_, r)| *r) else { return };
        let others: Vec<(SessionId, Rect)> = self.ui.pane_rects.iter().filter(|(id, _)| *id != active).cloned().collect();
        let rects: Vec<Rect> = others.iter().map(|(_, r)| *r).collect();
        let Some((id, _)) = focus::neighbour(from, &rects, dir).and_then(|i| others.get(i)) else { return };
        self.ui.region = None;
        self.ui.focus_terminal = Some(id.clone());
        self.dispatch.submit("session_focus", json!({"session_id": id}));
    }

    /// Whether a sidebar session row or group header has the keyboard focus (as drawn last frame).
    fn sidebar_item_focused(&self, ctx: &egui::Context) -> bool {
        ctx.memory(|m| m.focused()).is_some_and(|f| self.ui.sidebar_nav.iter().any(|(id, _)| *id == f))
    }

    /// Ctrl+PgUp / Ctrl+PgDn: shows the add-on screen tab before (or after) the one shown. The
    /// Sessions tab is first; a screen in its own window is skipped (its tab docks it on a click).
    fn screen_tab_step(&mut self, ctx: &egui::Context, forward: bool) {
        let mut tabs: Vec<Option<String>> = vec![None];
        tabs.extend(self.ui.screen_infos.iter().filter(|i| !i.popped_out).map(|i| Some(i.instance.clone())));
        let now = lock(&self.ui.screens).active.clone();
        let n = tabs.len();
        let at = tabs.iter().position(|t| *t == now).unwrap_or(0);
        let to = if forward { (at + 1) % n } else { (at + n - 1) % n };
        if let Some(next) = tabs.get(to).cloned() {
            self.run(ctx, Action::ScreenFocus(next));
        }
    }

    /// A terminal pane has keyboard focus: its keys (Esc above all) belong to the program in it.
    fn terminal_focused(&self, ctx: &egui::Context) -> bool {
        let Some(focused) = ctx.memory(|m| m.focused()) else { return false };
        lock(&self.host).views.values().any(|v| v.id() == focused)
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
        screens::remote::poll(self, &ctx);
        self.remote_step(&ctx);
        screens::connect_remote::poll(self, &ctx);
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
                && self.ui.terminal_takes_focus()
                && let Some(a) = self.model.active.clone()
            {
                self.ui.focus_terminal = Some(a);
            }
        }

        let t = theme::Tokens::get(&ctx);
        // The regions drawn this frame (F6 reads them next frame).
        self.ui.region_rects.clear();
        self.ui.region_items.clear();
        // The whole window, whatever margin the host gave the root ui.
        let full = ctx.content_rect();
        let mut root = ui.new_child(UiBuilder::new().max_rect(full));
        root.set_clip_rect(full);
        let ui = &mut root;
        ui.painter().rect_filled(full, 0.0, t.bg);
        let mut actions = Vec::new();

        let title = Rect::from_min_max(full.min, pos2(full.max.x, full.min.y + size::TITLE_BAR));
        ui.scope_builder(UiBuilder::new().max_rect(title), |ui| screens::titlebar::show(ui, self, &mut actions));
        self.ui.region_rects.push((FocusRegion::TitleBar, title));
        let mut body_top = title.max.y;
        if let Some(status) = self.remote.clone() {
            // Always visible in a remote window: the host and address, and the link's state.
            let bar = Rect::from_min_max(pos2(full.min.x, body_top), pos2(full.max.x, body_top + size::REMOTE_BAR));
            ui.scope_builder(UiBuilder::new().max_rect(bar), |ui| screens::remote_bar::show(ui, &status));
            body_top = bar.max.y;
        }
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
            self.ui.region_rects.push((FocusRegion::RailTabs, rail));
            x = rail.max.x;
        }
        if let Some(e) = &mut self.ui.inline {
            e.unseen_frames = e.unseen_frames.saturating_add(1);
        }
        let sidebar_w = if self.ui.compact { 0.0 } else { size::SIDEBAR };
        let side = Rect::from_min_max(pos2(x, body.min.y), pos2(x + sidebar_w, body.max.y));
        if !self.ui.compact {
            ui.scope_builder(UiBuilder::new().max_rect(side), |ui| screens::sidebar::show(ui, self, &mut actions));
            self.ui.region_rects.push((FocusRegion::SidebarList, side));
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
            let mut inspector_rect = None;
            if self.model.inspector.is_some() {
                // Never narrower than a readable column. The default is at most 40% of the body;
                // dragging the left edge may widen it to 70% (`inspector_panel_set`).
                let max_w = (body.width() * 0.7).max(size::INSPECTOR_MIN);
                let w = match self.ui.drags.inspector_w.or(self.model.inspector_width) {
                    Some(w) => w.clamp(size::INSPECTOR_MIN, max_w),
                    None => size::INSPECTOR.min((body.width() * 0.4).max(size::INSPECTOR_MIN)),
                };
                let insp = Rect::from_min_max(pos2(body.max.x - w, body.min.y), body.max);
                work.max.x = insp.min.x;
                ui.scope_builder(UiBuilder::new().max_rect(insp), |ui| screens::inspector::show(ui, self, &mut actions));
                inspector_rect = Some((insp, w, max_w));
                self.ui.region_rects.push((FocusRegion::Inspector, insp));
            }
            self.ui.region_rects.push((FocusRegion::Panes, work));
            ui.scope_builder(UiBuilder::new().max_rect(work), |ui| screens::panes::workspace(ui, self, &mut actions));
            // The inspector's left edge drags its width (registered last, so it sits on top of
            // both sides); a double click resets it.
            if let Some((insp, w, max_w)) = inspector_rect {
                let grip = Rect::from_center_size(pos2(insp.min.x, insp.center().y), egui::vec2(size::SPLIT_GRIP, insp.height()));
                let resp = ui.interact(grip, egui::Id::new("inspector-split"), egui::Sense::click_and_drag());
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Inspector edge"));
                if resp.dragged() || resp.hovered() {
                    let t = theme::Tokens::get(ui.ctx());
                    ui.painter().vline(insp.min.x, insp.y_range(), egui::Stroke::new(size::RING, t.blue));
                }
                let resp = resp.on_hover_cursor(egui::CursorIcon::ResizeColumn).on_hover_text("Drag to resize the inspector. Double-click resets.");
                // A triple click (a double click right after another click) resets it too, as the inbox
                // divider does.
                if resp.double_clicked() || resp.triple_clicked() {
                    self.ui.drags.inspector_w = None;
                    actions.push(Action::Tool("inspector_panel_set", json!({"reset": true})));
                } else if resp.dragged() {
                    let base = self.ui.drags.inspector_w.unwrap_or(w);
                    self.ui.drags.inspector_w = Some((base - resp.drag_delta().x).clamp(size::INSPECTOR_MIN, max_w));
                } else if resp.drag_stopped()
                    && let Some(w) = self.ui.drags.inspector_w.take()
                {
                    actions.push(Action::Tool("inspector_panel_set", json!({"width": w.clamp(200.0, 2000.0)})));
                }
            }
        }
        if self.ui.compact && self.ui.peek_sidebar {
            let peek = Rect::from_min_max(pos2(side.min.x, body.min.y), pos2(side.min.x + size::SIDEBAR, body.max.y));
            screens::sidebar::peek(&ctx, self, peek, &mut actions);
        }
        // A remote window whose host is out of reach shows its last snapshot, veiled (the strip says why).
        if self.model.stale {
            ui.painter().rect_filled(body, 0.0, theme::Tokens::tint(t.bg, theme::alpha::STALE));
        }
        self.inline_fallback();
        screens::panes::torn_windows(&ctx, self);
        screens::addon_screen::pop_windows(&ctx, self);
        // The panes drawn this frame, and the one with focus, for a backend that subscribes terminals.
        let shown = std::mem::take(&mut self.ui.shown_panes);
        let active = self.model.active.clone();
        self.backend.show_panes(&shown, active.as_deref());

        // The keyboard focus ring, round the region that has it (none for a mouse user).
        if let Some(r) = self.ui.region
            && let Some((_, rect)) = self.ui.region_rects.iter().find(|(x, _)| *x == r)
        {
            crate::widgets::focus_ring(ui, *rect, theme::radius::SM);
        }
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

/// Where the title bar's main menu button is drawn last frame. A press on the button is not
/// "outside" the menu, so a second click closes the menu rather than reopening it.
pub const MAIN_ANCHOR: &str = "main-menu-anchor";

/// The rail's tab shortcuts, Ctrl+1 (All) to Ctrl+9, in the order of the rail's tabs.
const RAIL_TAB_IDS: [&str; 9] = [
    "ui.rail_tab_1",
    "ui.rail_tab_2",
    "ui.rail_tab_3",
    "ui.rail_tab_4",
    "ui.rail_tab_5",
    "ui.rail_tab_6",
    "ui.rail_tab_7",
    "ui.rail_tab_8",
    "ui.rail_tab_9",
];

/// Consumes a key press with exactly these modifiers (Ctrl and Cmd as egui's `COMMAND`).
/// egui's `consume_shortcut` ignores extra Shift and Alt, so Ctrl+Shift+T would also be Ctrl+T;
/// here each binding gets only its own chord.
///
/// A punctuation key (`/`, `.`, `,` …) is the character, not the key cap: on many layouts it
/// needs Shift (`/` is Shift+7 on Danish and German keyboards), so for those an extra Shift is
/// ignored and Ctrl+/ still opens the shortcut list there. Minus is not one of them: Ctrl+Shift+-
/// is Ctrl+_ (the terminal's undo), which must not match the zoom out on Ctrl+-.
pub fn consume_exact(i: &mut egui::InputState, m: Modifiers, key: Key) -> bool {
    let symbol = matches!(
        key,
        Key::Slash
            | Key::Period
            | Key::Comma
            | Key::Colon
            | Key::Semicolon
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

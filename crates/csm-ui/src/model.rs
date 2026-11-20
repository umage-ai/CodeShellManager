//! What the screens draw: a plain-data [`UiModel`], one immutable snapshot per frame.
//!
//! The UI never mutates it. Every change is a tool call (`engine.submit(Caller::Ui, id, params)`);
//! the backend answers with a new snapshot. Today the demo backend builds it; with the real
//! engine it is a mapping of `engine.view()` (see `backend.rs` for exactly what to map).
//!
//! Types here also serialise as tool params (`layout_set` sends a [`Layout`]), so their serde
//! shape is the wire shape: snake_case, ids as strings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::theme::ThemeKind;

pub type SessionId = String;
pub type GroupId = String;

/// The one status vocabulary: the same glyph and word in the sidebar, the pane header, the
/// palette and the inbox. OSC 7501 drives it when a program speaks it; the heuristic otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Launching, or queued in the restore loop.
    Starting,
    Working,
    /// Needs you: a tool approval.
    Approval,
    /// Needs you: a question.
    Question,
    /// Done, and not seen yet.
    Done,
    #[default]
    Idle,
    Asleep,
    Error,
}

impl Status {
    pub const ALL: [Status; 8] =
        [Status::Working, Status::Approval, Status::Question, Status::Done, Status::Idle, Status::Asleep, Status::Error, Status::Starting];

    pub fn id(self) -> &'static str {
        match self {
            Status::Starting => "starting",
            Status::Working => "working",
            Status::Approval => "approval",
            Status::Question => "question",
            Status::Done => "done",
            Status::Idle => "idle",
            Status::Asleep => "asleep",
            Status::Error => "error",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Starting => "Starting",
            Status::Working => "Working",
            Status::Approval => "Needs you: approval",
            Status::Question => "Needs you: question",
            Status::Done => "Done, unseen",
            Status::Idle => "Idle",
            Status::Asleep => "Asleep",
            Status::Error => "Error",
        }
    }

    pub fn from_id(s: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|x| x.id() == s)
    }

    /// Approval or question: listed under "Needs you".
    pub fn needs_you(self) -> bool {
        matches!(self, Status::Approval | Status::Question)
    }

    /// Done or error, not yet looked at: listed under "Unseen".
    pub fn unseen(self) -> bool {
        matches!(self, Status::Done | Status::Error)
    }

    /// Has a running process (everything but asleep).
    pub fn is_live(self) -> bool {
        self != Status::Asleep
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Local,
    Ssh,
    Wsl,
}

/// A session's accent: an index into the 12-colour palette (`csm_core::color`), drawn in the
/// theme's own shade, or an exact colour pushed by OSC 9001.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    Palette(u8),
    Rgb([u8; 3]),
}

impl Default for Accent {
    fn default() -> Self {
        Accent::Palette(7)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    NotRun,
    Running,
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RunView {
    pub id: String,
    pub label: String,
    pub command_line: String,
    pub is_default: bool,
    pub state: RunState,
    /// "running · 4 min", "failed · exit 1".
    pub detail: String,
    /// The last lines of output (ANSI-stripped). The real backend reads output through
    /// [`crate::Backend::run_output`] instead; this is the demo's.
    pub output: String,
    /// `process` (cmd /c, the login shell elsewhere) or `powershell`.
    pub mode: String,
    /// Opened in the browser after the run exits 0 (absolute http/https only).
    pub post_run_url: Option<String>,
    pub exit_code: Option<i32>,
}

/// One "Open with" entry as a session's menus show it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OpenWithItem {
    pub entry_id: String,
    pub label: String,
    /// The program's file name, shown beside the label.
    pub program: String,
    /// The exact command it would run for this session (the tooltip), or why it can't.
    pub command: String,
    pub is_default: bool,
}

/// One entry of the Open with list, as the editor shows it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OpenWithEntryView {
    pub id: String,
    pub label: String,
    pub executable: String,
    pub program: String,
    pub local_args: Option<String>,
    pub wsl_args: Option<String>,
    pub ssh_args: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SessionView {
    pub id: SessionId,
    pub name: String,
    pub group: Option<GroupId>,
    pub kind: Kind,
    /// The command it runs (`claude`, `pwsh`, …).
    pub command: String,
    /// Shown folder: a path, `user@host:/path` or `Ubuntu · ~/src/x`.
    pub folder: String,
    pub branch: Option<String>,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
    /// What the branch tracks (`origin/main`).
    pub upstream: Option<String>,
    /// Changed paths.
    pub changes: u32,
    /// The git header line (`main → origin/main · ↑2 · 3 changed · worktree of X`): the chip's
    /// tooltip and the Git tab's first line.
    pub git_summary: Option<String>,
    /// In a linked worktree: the main repo's name.
    pub worktree_of: Option<String>,
    /// A git operation is running on this session's repository.
    pub git_busy: bool,
    pub status: Status,
    /// Working progress, 0–100, when the program reports one.
    pub progress: Option<u8>,
    /// Why it needs you / what happened ("Bash: dotnet test …").
    pub detail: String,
    /// When the status was entered (ms since the epoch).
    pub status_since_ms: Option<u64>,
    pub accent: Accent,
    /// The accent was pushed by the program (OSC 9001): "Reset accent colour" is offered.
    pub accent_overridden: bool,
    /// The repo session this worktree session belongs under.
    pub parent: Option<SessionId>,
    pub runs: Vec<RunView>,
    /// The Open with entries that apply to this session.
    pub open_with: Vec<OpenWithItem>,
    /// "2 h 14 min ago · resumes on restart".
    pub started: String,
    /// The process ended by itself. Shown as Idle with "Exited" as its word (and the code), so
    /// the status vocabulary keeps its seven states; the pane stays until closed or restarted.
    pub exited: bool,
    pub exit_code: Option<i32>,
    /// When it was last the focused session (ms since the epoch): the Recent menu's order.
    pub last_used_ms: Option<i64>,
}

impl SessionView {
    pub fn default_run(&self) -> Option<&RunView> {
        self.runs.iter().find(|r| r.is_default).or(self.runs.first())
    }

    /// "Working · 40%" or the status label.
    pub fn status_text(&self) -> String {
        match (self.status, self.progress) {
            (Status::Working, Some(p)) => format!("Working · {p}%"),
            (Status::Idle, _) if self.exited => match self.exit_code {
                Some(c) => format!("Exited · code {c}"),
                None => "Exited".to_string(),
            },
            (s, _) => s.label().to_string(),
        }
    }

    pub fn is_claude(&self) -> bool {
        self.command.split_whitespace().next().is_some_and(|c| c.eq_ignore_ascii_case("claude"))
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GroupView {
    pub id: GroupId,
    pub name: String,
    /// Lives in its own OS window (`group_tear_off`).
    pub torn_off: bool,
    /// Folded in the headers view (`group_collapse`).
    pub collapsed: bool,
}

/// How groups appear in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    /// A strip of group tabs; one group at a time, the panes follow it. The default.
    #[default]
    Rail,
    /// Every group in one list, collapsible.
    Headers,
    /// No group UI.
    Flat,
}

/// What a layout belongs to: one group, or "all sessions" (the rail's All tab, flat view).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Scope {
    #[default]
    All,
    Group(GroupId),
}

impl Scope {
    /// The key layouts are stored under (and the `group` param of `layout_set`).
    pub fn key(&self) -> String {
        match self {
            Scope::All => "all".into(),
            Scope::Group(g) => g.clone(),
        }
    }

    pub fn from_key(key: &str) -> Scope {
        if key == "all" { Scope::All } else { Scope::Group(key.to_string()) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    /// One pane; the sidebar is the switcher.
    Focus,
    /// The pinned sessions, auto-tiled. The default.
    #[default]
    Side,
    /// Every live session as a grid of mini-terminals.
    Overview,
    /// Fixed presets.
    Grid,
    /// A split tree with draggable dividers, floating and torn-off panes.
    Free,
}

impl LayoutMode {
    pub const ALL: [LayoutMode; 5] = [LayoutMode::Focus, LayoutMode::Side, LayoutMode::Overview, LayoutMode::Grid, LayoutMode::Free];

    pub fn id(self) -> &'static str {
        match self {
            LayoutMode::Focus => "focus",
            LayoutMode::Side => "side",
            LayoutMode::Overview => "overview",
            LayoutMode::Grid => "grid",
            LayoutMode::Free => "free",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LayoutMode::Focus => "Focus",
            LayoutMode::Side => "Side by side",
            LayoutMode::Overview => "Overview",
            LayoutMode::Grid => "Grid",
            LayoutMode::Free => "Free",
        }
    }

    /// The icon name in the mock-up's set.
    pub fn icon(self) -> &'static str {
        self.id()
    }
}

/// Grid presets (1, 2, 2×2, 3×2, 3×3) and a custom N×M.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridPreset {
    pub cols: u8,
    pub rows: u8,
}

impl GridPreset {
    pub const PRESETS: [GridPreset; 5] = [
        GridPreset { cols: 1, rows: 1 },
        GridPreset { cols: 2, rows: 1 },
        GridPreset { cols: 2, rows: 2 },
        GridPreset { cols: 3, rows: 2 },
        GridPreset { cols: 3, rows: 3 },
    ];

    pub fn new(cols: u8, rows: u8) -> GridPreset {
        GridPreset { cols: cols.clamp(1, 6), rows: rows.clamp(1, 6) }
    }

    pub fn cells(self) -> usize {
        usize::from(self.cols.max(1)) * usize::from(self.rows.max(1))
    }

    /// "1", "2", "2×2", …
    pub fn label(self) -> String {
        match (self.cols, self.rows) {
            (c, 1) => c.to_string(),
            (c, r) => format!("{c}×{r}"),
        }
    }

    pub fn is_preset(self) -> bool {
        GridPreset::PRESETS.contains(&self)
    }
}

impl Default for GridPreset {
    fn default() -> Self {
        GridPreset { cols: 2, rows: 2 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    /// Children side by side (vertical dividers).
    Row,
    /// Children stacked (horizontal dividers).
    Col,
}

/// Free mode's split tree. Ratios are fractions of the parent that sum to 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitNode {
    Leaf { session: SessionId },
    Split { dir: SplitDir, ratios: Vec<f32>, children: Vec<SplitNode> },
}

impl SplitNode {
    pub fn leaf(id: &str) -> SplitNode {
        SplitNode::Leaf { session: id.to_string() }
    }

    /// Every session in the tree, in order.
    pub fn sessions(&self) -> Vec<SessionId> {
        match self {
            SplitNode::Leaf { session } => vec![session.clone()],
            SplitNode::Split { children, .. } => children.iter().flat_map(SplitNode::sessions).collect(),
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        match self {
            SplitNode::Leaf { session } => session == id,
            SplitNode::Split { children, .. } => children.iter().any(|c| c.contains(id)),
        }
    }

    /// The tree without `id`; a split left with one child collapses into it.
    pub fn without(&self, id: &str) -> Option<SplitNode> {
        match self {
            SplitNode::Leaf { session } => (session != id).then(|| self.clone()),
            SplitNode::Split { dir, ratios, children } => {
                let mut kept = Vec::new();
                let mut kept_ratios = Vec::new();
                for (i, c) in children.iter().enumerate() {
                    if let Some(c) = c.without(id) {
                        kept.push(c);
                        kept_ratios.push(ratios.get(i).copied().unwrap_or(0.0).max(0.0));
                    }
                }
                match kept.len() {
                    0 => None,
                    1 => kept.pop(),
                    _ => {
                        let sum: f32 = kept_ratios.iter().sum();
                        let n = kept_ratios.len() as f32;
                        let ratios = kept_ratios.iter().map(|r| if sum > 0.0 { r / sum } else { 1.0 / n }).collect();
                        Some(SplitNode::Split { dir: *dir, ratios, children: kept })
                    }
                }
            }
        }
    }

    /// Puts `id` beside the whole tree (a docked float lands on the right).
    pub fn with_added(self, id: &str, dir: SplitDir) -> SplitNode {
        SplitNode::Split { dir, ratios: vec![0.64, 0.36], children: vec![self, SplitNode::leaf(id)] }
    }

    /// Splits the leaf `target`, putting `id` before or after it.
    pub fn split_leaf(&self, target: &str, id: &str, dir: SplitDir, after: bool) -> SplitNode {
        match self {
            SplitNode::Leaf { session } if session == target => {
                let mut children = vec![self.clone(), SplitNode::leaf(id)];
                if !after {
                    children.reverse();
                }
                SplitNode::Split { dir, ratios: vec![0.5, 0.5], children }
            }
            SplitNode::Leaf { .. } => self.clone(),
            SplitNode::Split { dir: d, ratios, children } => SplitNode::Split {
                dir: *d,
                ratios: ratios.clone(),
                children: children.iter().map(|c| c.split_leaf(target, id, dir, after)).collect(),
            },
        }
    }

    /// Even tiling of `ids` in a row.
    pub fn row(ids: &[SessionId]) -> Option<SplitNode> {
        match ids.len() {
            0 => None,
            1 => ids.first().map(|i| SplitNode::leaf(i)),
            n => Some(SplitNode::Split {
                dir: SplitDir::Row,
                ratios: vec![1.0 / n as f32; n],
                children: ids.iter().map(|i| SplitNode::leaf(i)).collect(),
            }),
        }
    }

    /// Sets the ratio at `path` (child indices from the root) between child `i` and `i + 1`.
    pub fn set_ratio(&mut self, path: &[usize], i: usize, first: f32) {
        match self {
            SplitNode::Leaf { .. } => {}
            SplitNode::Split { ratios, children, .. } => {
                if let Some((&head, rest)) = path.split_first() {
                    if let Some(c) = children.get_mut(head) {
                        c.set_ratio(rest, i, first);
                    }
                } else if i + 1 < ratios.len() {
                    let pair = ratios[i] + ratios[i + 1];
                    let a = first.clamp(0.12 * pair, 0.88 * pair);
                    ratios[i] = a;
                    ratios[i + 1] = pair - a;
                }
            }
        }
    }
}

/// A pane floating inside the main window (Free mode), in points relative to the pane area.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloatPane {
    pub session: SessionId,
    pub rect: [f32; 4],
}

/// The part of a [`Layout`] only Free mode uses. The engine stores it as an opaque object
/// (`layout_set { free }`, `GroupLayout::free_tree`): its shape is the UI's.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FreeLayout {
    pub tree: Option<SplitNode>,
    pub floats: Vec<FloatPane>,
    pub torn: Vec<SessionId>,
}

impl FreeLayout {
    /// Reads a stored blob; anything unreadable is an empty layout (the file is untrusted).
    pub fn from_value(v: Option<&serde_json::Value>) -> FreeLayout {
        v.and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
    }

    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// One group's (or All's) layout. Each group remembers its own.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Layout {
    pub mode: LayoutMode,
    /// Side by side: the pinned sessions, in order.
    pub pinned: Vec<SessionId>,
    pub grid: GridPreset,
    pub tree: Option<SplitNode>,
    pub floats: Vec<FloatPane>,
    /// Panes in their own OS window.
    pub torn: Vec<SessionId>,
}

impl Layout {
    /// The Free-mode part, as `layout_set { free }` takes it.
    pub fn free(&self) -> FreeLayout {
        FreeLayout { tree: self.tree.clone(), floats: self.floats.clone(), torn: self.torn.clone() }
    }

    pub fn set_free(&mut self, f: FreeLayout) {
        self.tree = f.tree;
        self.floats = f.floats;
        self.torn = f.torn;
    }

    /// `pane_float`: switch to Free and float `id` over the split tree (`live` are the scope's
    /// live sessions, to seed a tree when there is none).
    pub fn float_pane(&mut self, id: &str, live: &[SessionId]) {
        self.mode = LayoutMode::Free;
        if self.tree.is_none() {
            let ids: Vec<SessionId> = if self.pinned.is_empty() { live.to_vec() } else { self.pinned.clone() };
            self.tree = SplitNode::row(&ids);
        }
        // A pane already floating docks first, so one floats at a time per click.
        self.tree = self.tree.as_ref().and_then(|t| t.without(id));
        if self.tree.is_none()
            && let Some(other) = live.iter().find(|x| x.as_str() != id)
        {
            self.tree = Some(SplitNode::leaf(other));
        }
        self.floats.retain(|f| f.session != id);
        let [w, h] = crate::theme::size::FLOAT_DEFAULT;
        self.floats.push(FloatPane { session: id.to_string(), rect: [440.0, 220.0, w, h] });
    }

    /// `pane_dock`: bring a floating or torn-off pane back; in Free mode it joins the tree on the
    /// right. Returns whether it was torn off.
    pub fn dock_pane(&mut self, id: &str) -> bool {
        let was_torn = self.torn.iter().any(|t| t == id);
        self.floats.retain(|f| f.session != id);
        self.torn.retain(|t| t != id);
        if self.mode == LayoutMode::Free && !self.tree.as_ref().is_some_and(|t| t.contains(id)) {
            self.tree = Some(match self.tree.take() {
                Some(t) => t.with_added(id, SplitDir::Row),
                None => SplitNode::leaf(id),
            });
        }
        was_torn
    }

    /// `pane_tear_off`: show the pane in its own OS window.
    pub fn tear_off_pane(&mut self, id: &str) {
        self.floats.retain(|f| f.session != id);
        if !self.torn.iter().any(|t| t == id) {
            self.torn.push(id.to_string());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectorTab {
    Notes,
    Runs,
    Git,
    Session,
}

impl InspectorTab {
    pub const ALL: [InspectorTab; 4] = [InspectorTab::Notes, InspectorTab::Runs, InspectorTab::Git, InspectorTab::Session];

    pub fn id(self) -> &'static str {
        match self {
            InspectorTab::Notes => "notes",
            InspectorTab::Runs => "runs",
            InspectorTab::Git => "git",
            InspectorTab::Session => "session",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InspectorTab::Notes => "Notes",
            InspectorTab::Runs => "Runs",
            InspectorTab::Git => "Git",
            InspectorTab::Session => "Session",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            InspectorTab::Notes => "note",
            InspectorTab::Runs => "play",
            InspectorTab::Git => "branch",
            InspectorTab::Session => "sliders",
        }
    }
}

/// The "Needs you" panel: open or closed, and its height. Both persist (`inbox_panel_set`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InboxPanel {
    pub open: bool,
    pub height: f32,
}

impl Default for InboxPanel {
    fn default() -> Self {
        InboxPanel { open: true, height: crate::theme::size::INBOX_DEFAULT_H }
    }
}

/// A folder the quick-open offers.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FolderSuggestion {
    pub name: String,
    pub path: String,
    pub branch: Option<String>,
    /// "Recent · 2 h", "Repo under C:\GitHub".
    pub tag: String,
    /// Sessions open in it.
    pub open: u32,
    pub recent: bool,
}

/// The restore loop's progress (the rail under the title bar and the pill in it).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RestoreProgress {
    pub done: u32,
    pub total: u32,
    pub current: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownRowState {
    Pending,
    Closing,
    Clean,
    Forced,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShutdownRow {
    pub session: SessionId,
    pub state: ShutdownRowState,
    pub elapsed_s: f32,
}

/// The shutdown board: every session with how its exit went, and the wait budget.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ShutdownBoard {
    pub rows: Vec<ShutdownRow>,
    pub budget_used_s: f32,
    pub budget_s: f32,
}

/// Everything the screens draw.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UiModel {
    pub sessions: Vec<SessionView>,
    pub groups: Vec<GroupView>,
    pub group_mode: GroupMode,
    /// The rail's selected tab.
    pub rail: Scope,
    /// Layouts by [`Scope::key`].
    pub layouts: BTreeMap<String, Layout>,
    pub active: Option<SessionId>,
    pub inspector: Option<InspectorTab>,
    pub inbox_panel: InboxPanel,
    pub restore: Option<RestoreProgress>,
    pub shutdown: Option<ShutdownBoard>,
    pub folders: Vec<FolderSuggestion>,
    /// Commands the quick-open cycles with Tab.
    pub launch_commands: Vec<String>,
    /// Notes by folder.
    pub notes: BTreeMap<String, String>,
    pub theme: ThemeKind,
    /// The snapshot's clock (ms since the epoch), for ages.
    pub now_ms: u64,
    /// The WPF app's state file, when one was found at start-up: the empty screen offers to
    /// import it (`state_import_wpf`).
    pub wpf_import: Option<String>,
    /// The Git tab's details per session, from git tool replies (`crate::git_model`).
    pub git: BTreeMap<SessionId, crate::git_model::GitDetail>,
    /// The Open with list (the editor).
    pub open_with: Vec<OpenWithEntryView>,
    /// Closed sessions that can be reopened, newest first (`session_reopen_closed` by index).
    pub recently_closed: Vec<ClosedSessionView>,
}

/// A recently closed session, as the Recent menu lists it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ClosedSessionView {
    pub name: String,
    /// The folder, `user@host` or `distro: folder`.
    pub subtitle: String,
    pub command: String,
}

impl UiModel {
    pub fn session(&self, id: &str) -> Option<&SessionView> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn group(&self, id: &str) -> Option<&GroupView> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn active_session(&self) -> Option<&SessionView> {
        self.active.as_deref().and_then(|id| self.session(id))
    }

    /// The scope the panes show: the rail tab, All in flat view, the active session's group in
    /// headers view.
    pub fn pane_scope(&self) -> Scope {
        match self.group_mode {
            GroupMode::Rail => self.rail.clone(),
            GroupMode::Flat => Scope::All,
            GroupMode::Headers => self.active_session().and_then(|s| s.group.clone()).map(Scope::Group).unwrap_or(Scope::All),
        }
    }

    pub fn scope_name(&self, scope: &Scope) -> String {
        match scope {
            Scope::All => "All sessions".into(),
            Scope::Group(g) => self.group(g).map_or_else(|| "Ungrouped".into(), |g| g.name.clone()),
        }
    }

    /// Sessions in a scope, in sidebar order.
    pub fn scope_sessions(&self, scope: &Scope) -> Vec<&SessionView> {
        self.sessions
            .iter()
            .filter(|s| match scope {
                Scope::All => true,
                Scope::Group(g) => s.group.as_deref() == Some(g.as_str()),
            })
            .collect()
    }

    pub fn layout(&self, scope: &Scope) -> Layout {
        self.layouts.get(&scope.key()).cloned().unwrap_or_default()
    }

    fn age_ms(&self, s: &SessionView) -> u64 {
        s.status_since_ms.map_or(0, |t| self.now_ms.saturating_sub(t))
    }

    /// Sessions that need you, oldest first.
    pub fn needs_you(&self) -> Vec<&SessionView> {
        let mut v: Vec<&SessionView> = self.sessions.iter().filter(|s| s.status.needs_you()).collect();
        v.sort_by_key(|s| std::cmp::Reverse(self.age_ms(s)));
        v
    }

    /// Done or failed and not seen, oldest first.
    pub fn unseen(&self) -> Vec<&SessionView> {
        let mut v: Vec<&SessionView> = self.sessions.iter().filter(|s| s.status.unseen()).collect();
        v.sort_by_key(|s| std::cmp::Reverse(self.age_ms(s)));
        v
    }

    /// "4m", "2h".
    pub fn age_text(&self, s: &SessionView) -> String {
        let min = self.age_ms(s) / 60_000;
        if min < 60 { format!("{min}m") } else { format!("{}h", min / 60) }
    }

    pub fn accent_index(&self, s: &SessionView) -> Accent {
        s.accent
    }

    pub fn note(&self, folder: &str) -> &str {
        self.notes.get(folder).map_or("", String::as_str)
    }
}

/// Initials for a rail tab: two letters for a one-word name ("Od", "Op"), first and last
/// initials otherwise ("CS" for "Code Shell").
pub fn initials(name: &str) -> String {
    let parts: Vec<&str> = name.split(|c: char| c.is_whitespace() || "-_.".contains(c)).filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [] => "?".into(),
        [one] => {
            let mut chars = one.chars();
            let first: String = chars.next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            let second: String = chars.next().map(|c| c.to_lowercase().collect()).unwrap_or_default();
            format!("{first}{second}")
        }
        [first, .., last] => {
            let a: String = first.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            let b: String = last.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            format!("{a}{b}")
        }
    }
}

/// Fuzzy match of `query` in `text` (the mock-up's scorer): every query character in order;
/// runs and word starts score higher. Returns the score and the matched char indices.
pub fn fuzzy(query: &str, text: &str) -> Option<(f32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0.0, Vec::new()));
    }
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut qi = 0;
    let mut score = 0.0f32;
    let mut last: Option<usize> = None;
    let mut idx = Vec::new();
    for (j, &c) in t.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if q.get(qi) == Some(&c) {
            idx.push(j);
            let run = last.is_some_and(|l| l + 1 == j);
            let word_start = j == 0 || t.get(j.wrapping_sub(1)).is_some_and(|p| " .-/\\_".contains(*p));
            score += if run { 3.0 } else { 1.0 } + if word_start { 2.0 } else { 0.0 };
            last = Some(j);
            qi += 1;
        }
    }
    (qi == q.len()).then_some((score - t.len() as f32 * 0.01, idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_follow_the_mock() {
        assert_eq!(initials("Odulodo"), "Od");
        assert_eq!(initials("Ops"), "Op");
        assert_eq!(initials("Umage"), "Um");
        assert_eq!(initials("client X"), "CX");
        assert_eq!(initials(""), "?");
    }

    #[test]
    fn fuzzy_prefers_runs_and_word_starts() {
        let (a, _) = fuzzy("odu", "Odulodo").unwrap();
        let (b, _) = fuzzy("odu", "OptimizelyNewsletter dummy").unwrap_or((0.0, vec![]));
        assert!(a > b);
        assert!(fuzzy("xyz", "Odulodo").is_none());
        assert_eq!(fuzzy("od", "Odulodo").unwrap().1, vec![0, 1]);
    }

    #[test]
    fn split_tree_removes_and_collapses() {
        let t = SplitNode::row(&["a".into(), "b".into(), "c".into()]).unwrap();
        let t2 = t.without("b").unwrap();
        assert_eq!(t2.sessions(), vec!["a", "c"]);
        let SplitNode::Split { ratios, .. } = &t2 else { panic!() };
        assert!((ratios.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        assert_eq!(t2.without("a").unwrap(), SplitNode::leaf("c"));
        let mut t3 = t.clone();
        t3.set_ratio(&[], 0, 0.5);
        let SplitNode::Split { ratios, .. } = &t3 else { panic!() };
        assert!((ratios[0] - 0.5).abs() < 1e-4 && (ratios[0] + ratios[1] - 2.0 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn layout_round_trips_as_json() {
        let l = Layout {
            mode: LayoutMode::Free,
            pinned: vec!["a".into()],
            grid: GridPreset::new(3, 2),
            tree: SplitNode::row(&["a".into(), "b".into()]),
            floats: vec![FloatPane { session: "c".into(), rect: [1.0, 2.0, 3.0, 4.0] }],
            torn: vec![],
        };
        let v = serde_json::to_value(&l).unwrap();
        assert_eq!(v["mode"], "free");
        assert_eq!(serde_json::from_value::<Layout>(v).unwrap(), l);
    }
}

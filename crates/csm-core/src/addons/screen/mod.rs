//! Add-on screens (docs/addons.md, "Screens"): what a process add-on declares, the declarative
//! view tree it sends, the keyed patches that change it, and the events that go back. Pure: no
//! I/O, no clock, no UI.
//!
//! **Everything here reads hostile input** (a plugin's stdout). A tree is validated as a whole
//! before it is kept: node count, depth, string sizes, unique keys, serialized size. A tree that
//! breaks a cap is refused entirely and the previous tree stays. Nothing panics, nothing indexes
//! a value a plugin chose, and recursion is bounded by [`MAX_DEPTH`].
//!
//! The tree is typed ([`Node`], [`Kind`]) so the renderer never touches JSON. Plugins never send
//! colours, sizes or fonts: only semantic choices ([`Tone`], [`Gap`], [`TextStyle`],
//! [`ButtonKind`]) that the renderer maps to theme tokens. `code` and `diff` nodes (the next
//! stream) are [`Kind::Code`] and [`Kind::Diff`] (see [`code`]); the
//! helpers here already treat every node generically (children, keys, size).

pub mod code;
pub mod files;
pub mod markdown;
mod text;

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use code::{Anchor, CodeContent, CodeView, DiffContent, DiffView, Doc, Mark, Overlay, RangeAction, Thread, Withheld};
pub use files::{FileEntry, FileRow, file_rows, normalize_path};
pub use markdown::{Block, Href, Inline};
pub use text::{Seg, clean_label, has_invisible, is_invisible, marker_for, reveal};

use super::manifest::clean_text;
use super::protocol::split_tool_id;
use crate::qr::{self, Matrix};

/// Nodes in one tree.
pub const MAX_NODES: usize = 5_000;
/// Depth of a tree (the root is depth 1).
pub const MAX_DEPTH: usize = 32;
/// A tree in one message, serialized.
pub const MAX_TREE_BYTES: usize = 768 * 1024;
/// One plain string (a text, a value, a comment body).
pub const MAX_STRING: usize = 16 * 1024;
/// A label (a title, a button, a badge), in characters. Longer labels are cut.
pub const MAX_LABEL: usize = 200;
/// A `text_input`'s `max_len`, and the longest value it holds.
pub const MAX_INPUT_LEN: usize = 16 * 1024;
/// Entries in a `list` or a `file_list`.
pub const MAX_LIST_ITEMS: usize = 20_000;
/// Options in a `select` or `segmented`, and tabs in `tabs`.
pub const MAX_OPTIONS: usize = 200;
/// Rows in a `kv`.
pub const MAX_KV_ROWS: usize = 500;
/// Badges on one list item, file or comment.
pub const MAX_BADGES: usize = 8;
/// Operations in one `screen/patch`.
pub const MAX_PATCH_OPS: usize = 256;
/// A node key, in characters.
pub const MAX_KEY: usize = 64;
/// The params of a button's bound tool, serialized.
pub const MAX_ACTION_PARAMS: usize = 8 * 1024;
/// Screens one add-on may declare.
pub const MAX_SCREENS: usize = 4;
/// Open instances per add-on.
pub const MAX_INSTANCES: usize = 8;
/// The `args` of a screen's context, serialized.
pub const MAX_CONTEXT_ARGS: usize = 4 * 1024;
/// A render or patch is applied at most this often per instance (30 a second); the latest wins.
pub const MIN_RENDER_INTERVAL_MS: u64 = 33;

/// Why a tree, a patch or a declaration was refused: one plain sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenError(String);

impl ScreenError {
    pub fn new(s: impl Into<String>) -> Self {
        ScreenError(s.into())
    }
}

impl fmt::Display for ScreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ScreenError {}

type Res<T> = Result<T, ScreenError>;

fn err<T>(s: impl Into<String>) -> Res<T> {
    Err(ScreenError::new(s))
}

// ---------------------------------------------------------------------------------------------
// Declarations
// ---------------------------------------------------------------------------------------------

/// What a screen is about; it decides which `context` it expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenScope {
    Project,
    Session,
    Global,
}

impl ScreenScope {
    pub fn name(self) -> &'static str {
        match self {
            ScreenScope::Project => "project",
            ScreenScope::Session => "session",
            ScreenScope::Global => "global",
        }
    }
}

/// One screen as a plugin declared it in its `initialize` answer, validated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScreenDecl {
    /// `<addon id>.<name>`.
    pub id: String,
    pub title: String,
    /// A name from CSM's own icon set; an unknown one draws a default.
    pub icon: String,
    pub scope: ScreenScope,
    pub summary: String,
}

/// Validates the `screens` of an `initialize` answer. Per entry (reported, the rest still loads):
/// an id that is not `<addon>.<name>` for this add-on, a duplicate, a missing title. At most
/// [`MAX_SCREENS`] are kept.
pub fn parse_screens(v: Option<&Value>, addon: &str) -> (Vec<ScreenDecl>, Vec<String>) {
    let mut out: Vec<ScreenDecl> = Vec::new();
    let mut problems = Vec::new();
    let list = match v {
        None | Some(Value::Null) => return (out, problems),
        Some(Value::Array(a)) => a.as_slice(),
        Some(_) => {
            problems.push("`screens` must be an array; ignored".into());
            return (out, problems);
        }
    };
    if list.len() > MAX_SCREENS {
        problems.push(format!("more than {MAX_SCREENS} screens; {} ignored", list.len() - MAX_SCREENS));
    }
    for entry in list.iter().take(MAX_SCREENS) {
        match parse_screen_decl(entry, addon) {
            Ok(d) => {
                if out.iter().any(|x| x.id == d.id) {
                    problems.push(format!("screen `{}` is declared twice; the second is ignored", d.id));
                } else {
                    out.push(d);
                }
            }
            Err(e) => problems.push(e.to_string()),
        }
    }
    (out, problems)
}

fn parse_screen_decl(entry: &Value, addon: &str) -> Res<ScreenDecl> {
    let obj = entry.as_object().ok_or_else(|| ScreenError::new("a screen entry is not an object"))?;
    let id = obj.get("id").and_then(Value::as_str).ok_or_else(|| ScreenError::new("a screen has no `id`"))?;
    let shown = clean_text(id, 80);
    match split_tool_id(id) {
        Some((a, _)) if a == addon => {}
        _ => return err(format!("screen `{shown}` must be named `{addon}.<name>` (lowercase letters, digits and `_`)")),
    }
    let title = obj.get("title").and_then(Value::as_str).map(|s| clean_label(s, 64)).filter(|s| !s.is_empty()).ok_or_else(|| ScreenError::new(format!("screen `{shown}` has no `title`")))?;
    let icon = obj
        .get("icon")
        .and_then(Value::as_str)
        .map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(32).collect::<String>())
        .unwrap_or_default();
    let scope = match obj.get("scope").and_then(Value::as_str) {
        Some("project") => ScreenScope::Project,
        Some("session") => ScreenScope::Session,
        _ => ScreenScope::Global,
    };
    let summary = obj.get("summary").and_then(Value::as_str).map(|s| clean_label(s, 500)).unwrap_or_default();
    Ok(ScreenDecl { id: id.to_owned(), title, icon, scope, summary })
}

/// What a screen is opened on. Filled by CSM from the tool's params and its own state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScreenContext {
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub folder: Option<String>,
    pub path: Option<String>,
    pub args: Option<Value>,
}

impl ScreenContext {
    /// Refuses a context whose strings or `args` are over their caps.
    pub fn validate(&self) -> Res<()> {
        for (name, v, cap) in [("session_id", &self.session_id, 128), ("project", &self.project, 1024), ("folder", &self.folder, 4096), ("path", &self.path, 4096)] {
            if v.as_ref().is_some_and(|s| s.chars().count() > cap || s.chars().any(char::is_control)) {
                return err(format!("the context's `{name}` is too long or has control characters"));
            }
        }
        if let Some(a) = &self.args
            && serde_json::to_string(a).map_or(true, |s| s.len() > MAX_CONTEXT_ARGS)
        {
            return err(format!("the context's `args` are over {MAX_CONTEXT_ARGS} bytes"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------------------------

/// `default | muted | info | success | warning | danger | accent`, mapped by the renderer to
/// `fg/muted/blue/green/yellow/red/mauve`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Default,
    Muted,
    Info,
    Success,
    Warning,
    Danger,
    Accent,
}

impl Tone {
    fn parse(v: Option<&Value>) -> Tone {
        match v.and_then(Value::as_str) {
            Some("muted") => Tone::Muted,
            Some("info") => Tone::Info,
            Some("success") => Tone::Success,
            Some("warning") => Tone::Warning,
            Some("danger") => Tone::Danger,
            Some("accent") => Tone::Accent,
            _ => Tone::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Gap {
    None,
    Xs,
    #[default]
    Sm,
    Md,
    Lg,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextStyle {
    #[default]
    Body,
    Caption,
    Title,
    Mono,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    #[default]
    Secondary,
    Ghost,
    Danger,
}

/// A chip: a count, a state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub text: String,
    pub tone: Tone,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stack {
    pub children: Vec<Node>,
    pub gap: Gap,
    pub align: Align,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabDef {
    pub key: String,
    pub label: String,
    pub badge: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opt {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub icon: String,
    pub badges: Vec<Badge>,
}

/// Who wrote a comment. `agent` gets the agent mark, so a plugin cannot make an agent look like
/// a person (and the plugin's own `kind` is the only thing it controls).
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub author: String,
    pub agent: bool,
    pub time_ms: u64,
    pub body: Vec<Block>,
    pub badges: Vec<Badge>,
    /// Buttons only.
    pub actions: Vec<Node>,
}

/// What a tool-bound button may do (decided by [`action_policy`] when the tree is parsed, so a
/// plugin cannot change it afterwards).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionPolicy {
    /// Runs as a click on the person's behalf (the add-on's own tools, and harmless core ones).
    Direct,
    /// Runs only after a confirm CSM draws, showing the tool and its exact params.
    Confirm,
    /// Drawn disabled; the reason is its tooltip.
    Refused(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub tool: String,
    pub params: Value,
    pub policy: ActionPolicy,
}

/// What clicking a button does.
#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    /// Sends a `click` event to the plugin.
    Event,
    /// Runs a tool.
    Action(Action),
    /// Neither: the button draws disabled.
    Inert,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub label: String,
    pub kind: ButtonKind,
    pub icon: String,
    pub disabled: bool,
    pub bind: Bind,
}

/// One node of a view tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// 1 to [`MAX_KEY`] characters, unique in the tree. Required on every node that sends events.
    pub key: Option<String>,
    pub kind: Kind,
}

/// The widget set. New kinds (`code`, `diff`) are added here; a plugin that sends a kind this CSM
/// does not know gets [`Kind::Unsupported`], so newer plugins degrade on older CSM.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Column(Stack),
    Row(Stack),
    Split { children: Vec<Node>, ratio: f32, min_px: f32 },
    Scroll { child: Box<Node> },
    Tabs { tabs: Vec<TabDef>, selected: String, child: Option<Box<Node>> },
    Section { title: String, badge: Option<String>, collapsible: bool, collapsed: bool, children: Vec<Node> },
    Divider,
    Spacer,
    Toolbar { children: Vec<Node> },
    Text { text: String, style: TextStyle, tone: Tone, wrap: bool },
    Markdown { blocks: Vec<Block> },
    Badge(Badge),
    Icon { name: String, tone: Tone },
    Kv { rows: Vec<(String, String)> },
    Empty { title: String, body: String, icon: String },
    Progress { value: Option<f32>, label: String },
    Comment(Comment),
    Button(Button),
    TextInput { value: String, placeholder: String, multiline: bool, max_len: usize, submit_label: Option<String>, on_change: bool, reset: bool },
    Checkbox { label: String, checked: bool },
    Select { options: Vec<Opt>, selected: String },
    Segmented { options: Vec<Opt>, selected: String },
    List { items: Vec<ListItem>, selected: Option<String> },
    FileList { files: Vec<FileEntry>, tree: bool, selected: Option<String> },
    /// Text with line numbers, highlighting, marks and inline threads.
    Code(Box<CodeView>),
    /// A unified patch, as unified or side-by-side.
    Diff(Box<DiffView>),
    /// A QR code of a short text (the phone remote design note in umage-ai/csm-relay, section 8.2). The text itself is
    /// not kept: only its matrix, computed when the tree is parsed. `code` is `None` when the text
    /// was refused, and the node then draws a notice in its place.
    Qr { label: String, size: QrSize, code: Option<Matrix> },
    /// A node type this CSM does not know (drawn as a small box naming it).
    Unsupported { type_name: String },
}

/// The side of a `qr` node: small, medium (the default) or large.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrSize {
    Sm,
    Md,
    Lg,
}

impl Node {
    /// The nodes directly below this one (in document order).
    pub fn children(&self) -> Vec<&Node> {
        match &self.kind {
            Kind::Column(s) | Kind::Row(s) => s.children.iter().collect(),
            Kind::Split { children, .. } | Kind::Toolbar { children } | Kind::Section { children, .. } => children.iter().collect(),
            Kind::Scroll { child } => vec![&**child],
            Kind::Tabs { child: Some(c), .. } => vec![&**c],
            Kind::Comment(c) => c.actions.iter().collect(),
            Kind::Code(c) => c.overlay.threads.iter().map(|t| &*t.child).collect(),
            Kind::Diff(d) => d.overlay.threads.iter().map(|t| &*t.child).collect(),
            _ => Vec::new(),
        }
    }

    pub fn children_mut(&mut self) -> Vec<&mut Node> {
        match &mut self.kind {
            Kind::Column(s) | Kind::Row(s) => s.children.iter_mut().collect(),
            Kind::Split { children, .. } | Kind::Toolbar { children } | Kind::Section { children, .. } => children.iter_mut().collect(),
            Kind::Scroll { child } => vec![&mut **child],
            Kind::Tabs { child: Some(c), .. } => vec![&mut **c],
            Kind::Comment(c) => c.actions.iter_mut().collect(),
            Kind::Code(c) => c.overlay.threads.iter_mut().map(|t| &mut *t.child).collect(),
            Kind::Diff(d) => d.overlay.threads.iter_mut().map(|t| &mut *t.child).collect(),
            _ => Vec::new(),
        }
    }

    /// The inline threads of a `code` or `diff` node.
    pub fn threads(&self) -> &[Thread] {
        match &self.kind {
            Kind::Code(c) => &c.overlay.threads,
            Kind::Diff(d) => &d.overlay.threads,
            _ => &[],
        }
    }

    /// The child list a patch may remove from and append to (stacks, toolbars, sections).
    fn child_vec_mut(&mut self) -> Option<&mut Vec<Node>> {
        match &mut self.kind {
            Kind::Column(s) | Kind::Row(s) => Some(&mut s.children),
            Kind::Toolbar { children } | Kind::Section { children, .. } => Some(children),
            _ => None,
        }
    }

    /// The type name as a plugin writes it (`column`, `text_input`, ...).
    pub fn type_name(&self) -> &str {
        match &self.kind {
            Kind::Column(_) => "column",
            Kind::Row(_) => "row",
            Kind::Split { .. } => "split",
            Kind::Scroll { .. } => "scroll",
            Kind::Tabs { .. } => "tabs",
            Kind::Section { .. } => "section",
            Kind::Divider => "divider",
            Kind::Spacer => "spacer",
            Kind::Toolbar { .. } => "toolbar",
            Kind::Text { .. } => "text",
            Kind::Markdown { .. } => "markdown",
            Kind::Badge(_) => "badge",
            Kind::Icon { .. } => "icon",
            Kind::Kv { .. } => "kv",
            Kind::Empty { .. } => "empty",
            Kind::Progress { .. } => "progress",
            Kind::Comment(_) => "comment",
            Kind::Button(_) => "button",
            Kind::TextInput { .. } => "text_input",
            Kind::Checkbox { .. } => "checkbox",
            Kind::Select { .. } => "select",
            Kind::Segmented { .. } => "segmented",
            Kind::List { .. } => "list",
            Kind::FileList { .. } => "file_list",
            Kind::Code(_) => "code",
            Kind::Diff(_) => "diff",
            Kind::Qr { .. } => "qr",
            Kind::Unsupported { type_name } => type_name,
        }
    }

    /// A rough size of this node's own content, so a tree's size can be checked again after a
    /// patch without serializing it.
    fn own_bytes(&self) -> usize {
        let key = self.key.as_ref().map_or(0, String::len);
        let own = match &self.kind {
            Kind::Text { text, .. } => text.len(),
            Kind::Markdown { blocks } => blocks_bytes(blocks),
            Kind::Badge(b) => b.text.len(),
            Kind::Icon { name, .. } => name.len(),
            Kind::Kv { rows } => rows.iter().map(|(a, b)| a.len() + b.len() + 8).sum(),
            Kind::Empty { title, body, icon } => title.len() + body.len() + icon.len(),
            Kind::Progress { label, .. } => label.len(),
            Kind::Comment(c) => c.author.len() + blocks_bytes(&c.body) + c.badges.iter().map(|b| b.text.len()).sum::<usize>(),
            Kind::Button(b) => b.label.len() + b.icon.len() + if let Bind::Action(a) = &b.bind { a.tool.len() + a.params.to_string().len() } else { 0 },
            Kind::TextInput { value, placeholder, .. } => value.len() + placeholder.len(),
            Kind::Checkbox { label, .. } => label.len(),
            Kind::Select { options, selected } | Kind::Segmented { options, selected } => options.iter().map(|o| o.value.len() + o.label.len() + 8).sum::<usize>() + selected.len(),
            Kind::List { items, .. } => items.iter().map(|i| i.key.len() + i.title.len() + i.subtitle.len() + i.icon.len() + 16).sum(),
            Kind::FileList { files, .. } => files.iter().map(|f| f.path.len() + f.old_path.as_ref().map_or(0, String::len) + 16).sum(),
            Kind::Tabs { tabs, selected, .. } => tabs.iter().map(|t| t.key.len() + t.label.len() + 8).sum::<usize>() + selected.len(),
            Kind::Section { title, badge, .. } => title.len() + badge.as_ref().map_or(0, String::len),
            Kind::Code(c) => c.bytes(),
            Kind::Diff(d) => d.bytes(),
            Kind::Unsupported { type_name } => type_name.len(),
            _ => 0,
        };
        32 + key + own
    }
}

fn blocks_bytes(blocks: &[Block]) -> usize {
    fn inl(v: &[Inline]) -> usize {
        v.iter()
            .map(|i| match i {
                Inline::Text(t) | Inline::Code(t) => t.len(),
                Inline::Emph(k) | Inline::Strong(k) => inl(k),
                Inline::Link { kids, .. } => 64 + inl(kids),
                Inline::Break => 1,
            })
            .sum()
    }
    blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(v) | Block::Heading(_, v) => inl(v),
            Block::Code { text, .. } => text.len(),
            Block::List { items, .. } => items.iter().map(|i| blocks_bytes(i)).sum(),
            Block::Quote(b) => blocks_bytes(b),
            Block::Rule => 1,
        })
        .sum()
}

/// What a button's bound tool may do, by tool id. The add-on's own tools run directly. A short
/// list of core tools is allowed, and `session_send_text`, `open_url` and `open_with` ask first
/// (CSM draws the confirm). Everything else (`addon_*`, `plugin_*`, `consent_*`, `settings_set`,
/// anything that enables an extension or may discard data) is refused.
pub fn action_policy(addon: &str, tool: &str) -> ActionPolicy {
    if let Some((owner, _)) = split_tool_id(tool) {
        return if owner == addon {
            ActionPolicy::Direct
        } else {
            ActionPolicy::Refused(format!("`{tool}` belongs to another add-on; a screen button may only run its own add-on's tools"))
        };
    }
    match tool {
        "session_focus" | "screen_open" | "screen_close" | "inspector_show" => ActionPolicy::Direct,
        "session_send_text" | "open_url" | "open_with" => ActionPolicy::Confirm,
        t if t.starts_with("addon_") || t.starts_with("plugin_") || t.starts_with("consent_") || t == "settings_set" => {
            ActionPolicy::Refused(format!("`{tool}` manages add-ons or settings; a screen button may not run it"))
        }
        _ => ActionPolicy::Refused(format!("`{tool}` is not one of the core tools a screen button may run (session_focus, screen_open, screen_close, inspector_show, open_url, session_send_text, open_with)")),
    }
}

/// A validated, numbered view tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenTree {
    pub rev: u64,
    pub root: Node,
    /// Nodes in the tree.
    pub nodes: usize,
}

/// Why a render or patch was not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// `rev` is not newer than the tree's: dropped silently.
    Stale { have: u64, got: u64 },
    /// A patch whose `base` is not the tree's revision: dropped, and CSM asks for a full render.
    BaseMismatch { have: u64, base: u64 },
    /// Broke a cap or was malformed: refused, the previous tree stays.
    Invalid(ScreenError),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::Stale { have, got } => write!(f, "revision {got} is not newer than {have}"),
            UpdateError::BaseMismatch { have, base } => write!(f, "the patch is based on revision {base}, and the screen is at {have}"),
            UpdateError::Invalid(e) => e.fmt(f),
        }
    }
}

impl From<ScreenError> for UpdateError {
    fn from(e: ScreenError) -> Self {
        UpdateError::Invalid(e)
    }
}

impl ScreenTree {
    /// A full tree from `{"rev": n, "root": {...}}` (the answer to `screen/open`, or the params
    /// of `screen/render`). When `have` is given, `rev` must be newer than it.
    pub fn from_render(v: &Value, addon: &str, have: Option<u64>) -> Result<ScreenTree, UpdateError> {
        let obj = v.as_object().ok_or_else(|| ScreenError::new("a render is not an object"))?;
        let rev = obj.get("rev").and_then(Value::as_u64).ok_or_else(|| ScreenError::new("a render needs a numeric `rev`"))?;
        if let Some(h) = have
            && rev <= h
        {
            return Err(UpdateError::Stale { have: h, got: rev });
        }
        let root = obj.get("root").ok_or_else(|| ScreenError::new("a render needs a `root`"))?;
        if serde_json::to_string(root).map_or(true, |s| s.len() > MAX_TREE_BYTES) {
            return Err(ScreenError::new(format!("the tree is over {} KiB", MAX_TREE_BYTES / 1024)).into());
        }
        let root = TreeParser::new(addon).node(root, 1)?;
        let nodes = check_tree(&root)?;
        Ok(ScreenTree { rev, root, nodes })
    }

    /// Applies `{"rev": n, "base": m, "ops": [...]}` and returns the new tree; `self` is not
    /// changed. Ops are `replace` (`key`, `node`), `remove` (`key`) and `append` (`key` of a
    /// stack, toolbar or section, `node`). The patched tree is validated against every cap again.
    pub fn apply_patch(&self, v: &Value, addon: &str) -> Result<ScreenTree, UpdateError> {
        let obj = v.as_object().ok_or_else(|| ScreenError::new("a patch is not an object"))?;
        let rev = obj.get("rev").and_then(Value::as_u64).ok_or_else(|| ScreenError::new("a patch needs a numeric `rev`"))?;
        if rev <= self.rev {
            return Err(UpdateError::Stale { have: self.rev, got: rev });
        }
        let base = obj.get("base").and_then(Value::as_u64).ok_or_else(|| ScreenError::new("a patch needs a numeric `base`"))?;
        if base != self.rev {
            return Err(UpdateError::BaseMismatch { have: self.rev, base });
        }
        let ops = obj.get("ops").and_then(Value::as_array).ok_or_else(|| ScreenError::new("a patch needs an `ops` array"))?;
        if ops.len() > MAX_PATCH_OPS {
            return Err(ScreenError::new(format!("a patch has more than {MAX_PATCH_OPS} operations")).into());
        }
        let mut root = self.root.clone();
        for op in ops {
            apply_op(&mut root, op, addon)?;
        }
        let nodes = check_tree(&root)?;
        Ok(ScreenTree { rev, root, nodes })
    }

    /// The node with this key.
    pub fn find(&self, key: &str) -> Option<&Node> {
        fn go<'a>(n: &'a Node, key: &str) -> Option<&'a Node> {
            if n.key.as_deref() == Some(key) {
                return Some(n);
            }
            n.children().into_iter().find_map(|c| go(c, key))
        }
        go(&self.root, key)
    }
}

fn find_mut<'a>(node: &'a mut Node, key: &str) -> Option<&'a mut Node> {
    if node.key.as_deref() == Some(key) {
        return Some(node);
    }
    for c in node.children_mut() {
        if let Some(f) = find_mut(c, key) {
            return Some(f);
        }
    }
    None
}

/// Removes the child with `key` from its stack, toolbar or section. `Ok(false)`: not found.
fn remove_key(node: &mut Node, key: &str) -> Res<bool> {
    if let Some(v) = node.child_vec_mut()
        && let Some(pos) = v.iter().position(|c| c.key.as_deref() == Some(key))
    {
        v.remove(pos);
        return Ok(true);
    }
    if node.children().iter().any(|c| c.key.as_deref() == Some(key)) {
        return err(format!("`{}` cannot be removed from a {} (replace it instead)", clean_label(key, 40), node.type_name()));
    }
    for c in node.children_mut() {
        if remove_key(c, key)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn apply_op(root: &mut Node, op: &Value, addon: &str) -> Res<()> {
    let o = op.as_object().ok_or_else(|| ScreenError::new("a patch operation is not an object"))?;
    let kind = o.get("op").and_then(Value::as_str).ok_or_else(|| ScreenError::new("a patch operation has no `op`"))?;
    let key = o.get("key").and_then(Value::as_str).ok_or_else(|| ScreenError::new("a patch operation has no `key`"))?;
    let shown = clean_label(key, 40);
    let new_node = |p: &mut TreeParser<'_>| -> Res<Node> { p.node(o.get("node").ok_or_else(|| ScreenError::new("this patch operation needs a `node`"))?, 1) };
    match kind {
        "replace" => {
            let n = new_node(&mut TreeParser::new(addon))?;
            let slot = find_mut(root, key).ok_or_else(|| ScreenError::new(format!("no node with key `{shown}`")))?;
            *slot = n;
        }
        "remove" => {
            if root.key.as_deref() == Some(key) {
                return err("the root cannot be removed");
            }
            if !remove_key(root, key)? {
                return err(format!("no node with key `{shown}`"));
            }
        }
        "append" => {
            let n = new_node(&mut TreeParser::new(addon))?;
            let slot = find_mut(root, key).ok_or_else(|| ScreenError::new(format!("no node with key `{shown}`")))?;
            let kind = slot.type_name().to_owned();
            slot.child_vec_mut().ok_or_else(|| ScreenError::new(format!("a {kind} cannot take appended children")))?.push(n);
        }
        other => return err(format!("`{}` is not a patch operation (replace, remove or append)", clean_label(other, 20))),
    }
    Ok(())
}

/// Checks a whole tree against every cap and invariant, iteratively: node count, depth, unique
/// keys, estimated size, a split's two children. Returns the node count.
fn check_tree(root: &Node) -> Res<usize> {
    let mut stack: Vec<(&Node, usize)> = vec![(root, 1)];
    let mut keys: HashSet<&str> = HashSet::new();
    let (mut nodes, mut bytes) = (0usize, 0usize);
    while let Some((n, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES {
            return err(format!("the tree has more than {MAX_NODES} nodes"));
        }
        if depth > MAX_DEPTH {
            return err(format!("the tree is deeper than {MAX_DEPTH} levels"));
        }
        bytes = bytes.saturating_add(n.own_bytes());
        if bytes > MAX_TREE_BYTES {
            return err(format!("the tree is over {} KiB", MAX_TREE_BYTES / 1024));
        }
        if let Some(k) = n.key.as_deref()
            && !keys.insert(k)
        {
            return err(format!("two nodes have the key `{}`", clean_label(k, 40)));
        }
        for t in n.threads() {
            if !keys.insert(t.key.as_str()) {
                return err(format!("two nodes or threads have the key `{}`", clean_label(&t.key, 40)));
            }
        }
        if let Kind::Split { children, .. } = &n.kind
            && children.len() != 2
        {
            return err("a split needs exactly two children");
        }
        stack.extend(n.children().into_iter().map(|c| (c, depth + 1)));
    }
    Ok(nodes)
}

// ---------------------------------------------------------------------------------------------
// Parsing one node
// ---------------------------------------------------------------------------------------------

struct TreeParser<'a> {
    addon: &'a str,
    nodes: usize,
    bytes: usize,
    keys: HashSet<String>,
}

fn plain(o: &Map<String, Value>, k: &str) -> Res<String> {
    match o.get(k).and_then(Value::as_str) {
        None => Ok(String::new()),
        Some(s) if s.len() > MAX_STRING => err(format!("`{k}` is over {} KiB", MAX_STRING / 1024)),
        Some(s) => Ok(s.to_owned()),
    }
}

fn label(o: &Map<String, Value>, k: &str) -> String {
    o.get(k).and_then(Value::as_str).map(|s| clean_label(s, MAX_LABEL)).unwrap_or_default()
}

fn flag(o: &Map<String, Value>, k: &str, default: bool) -> bool {
    o.get(k).and_then(Value::as_bool).unwrap_or(default)
}

fn gap(v: Option<&Value>) -> Gap {
    match v.and_then(Value::as_str) {
        Some("none") => Gap::None,
        Some("xs") => Gap::Xs,
        Some("md") => Gap::Md,
        Some("lg") => Gap::Lg,
        _ => Gap::Sm,
    }
}

fn icon_name(o: &Map<String, Value>) -> String {
    o.get("icon").or_else(|| o.get("name")).and_then(Value::as_str).map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(32).collect()).unwrap_or_default()
}

fn badges(v: Option<&Value>) -> Res<Vec<Badge>> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let a = v.as_array().ok_or_else(|| ScreenError::new("`badges` must be an array"))?;
    Ok(a
        .iter()
        .take(MAX_BADGES)
        .filter_map(|b| match b {
            Value::String(s) => Some(Badge { text: clean_label(s, 40), tone: Tone::Default }),
            Value::Object(o) => Some(Badge { text: label(o, "text").chars().take(40).collect(), tone: Tone::parse(o.get("tone")) }),
            _ => None,
        })
        .filter(|b| !b.text.is_empty())
        .collect())
}

fn options(o: &Map<String, Value>) -> Res<Vec<Opt>> {
    let Some(v) = o.get("options") else { return Ok(Vec::new()) };
    let a = v.as_array().ok_or_else(|| ScreenError::new("`options` must be an array"))?;
    if a.len() > MAX_OPTIONS {
        return err(format!("more than {MAX_OPTIONS} options"));
    }
    Ok(a
        .iter()
        .filter_map(|x| match x {
            Value::String(s) => Some(Opt { value: s.chars().take(MAX_LABEL).collect(), label: clean_label(s, MAX_LABEL) }),
            Value::Object(m) => {
                let value: String = m.get("value").and_then(Value::as_str)?.chars().take(MAX_LABEL).collect();
                let l = label(m, "label");
                Some(Opt { label: if l.is_empty() { clean_label(&value, MAX_LABEL) } else { l }, value })
            }
            _ => None,
        })
        .collect())
}

fn item_key(m: &Map<String, Value>, what: &str) -> Res<String> {
    let k = m.get("key").and_then(Value::as_str).ok_or_else(|| ScreenError::new(format!("a {what} entry needs a `key`")))?;
    valid_key(k)?;
    Ok(k.to_owned())
}

fn valid_key(k: &str) -> Res<()> {
    let n = k.chars().count();
    if n == 0 || n > MAX_KEY || k.chars().any(|c| c.is_control() || is_invisible(c)) {
        return err(format!("a key must be 1 to {MAX_KEY} characters with no control characters (got `{}`)", clean_label(k, 40)));
    }
    Ok(())
}

impl<'a> TreeParser<'a> {
    fn new(addon: &'a str) -> Self {
        TreeParser { addon, nodes: 0, bytes: 0, keys: HashSet::new() }
    }

    fn children(&mut self, o: &Map<String, Value>, k: &str, depth: usize) -> Res<Vec<Node>> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(a)) => {
                if a.len() > MAX_NODES {
                    return err(format!("more than {MAX_NODES} children in one node"));
                }
                a.iter().map(|c| self.node(c, depth + 1)).collect()
            }
            Some(_) => err(format!("`{k}` must be an array of nodes")),
        }
    }

    fn one(&mut self, o: &Map<String, Value>, k: &str, depth: usize) -> Res<Option<Box<Node>>> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => Ok(Some(Box::new(self.node(v, depth + 1)?))),
        }
    }

    fn action(&self, v: &Value) -> Res<Action> {
        let o = v.as_object().ok_or_else(|| ScreenError::new("a button's `action` must be an object"))?;
        let tool = o.get("tool").and_then(Value::as_str).ok_or_else(|| ScreenError::new("an action needs a `tool`"))?;
        if tool.is_empty() || tool.len() > 96 || !tool.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')) {
            return err(format!("`{}` is not a tool id", clean_label(tool, 40)));
        }
        let params = match o.get("params") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(p @ Value::Object(_)) => p.clone(),
            Some(_) => return err("an action's `params` must be an object"),
        };
        if serde_json::to_string(&params).map_or(true, |s| s.len() > MAX_ACTION_PARAMS) {
            return err(format!("an action's params are over {} KiB", MAX_ACTION_PARAMS / 1024));
        }
        Ok(Action { tool: tool.to_owned(), params, policy: action_policy(self.addon, tool) })
    }

    fn node(&mut self, v: &Value, depth: usize) -> Res<Node> {
        if depth > MAX_DEPTH {
            return err(format!("the tree is deeper than {MAX_DEPTH} levels"));
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return err(format!("the tree has more than {MAX_NODES} nodes"));
        }
        let o = v.as_object().ok_or_else(|| ScreenError::new("a node is not an object"))?;
        let ty = o.get("type").and_then(Value::as_str).ok_or_else(|| ScreenError::new("a node has no `type`"))?;
        let key = match o.get("key") {
            None | Some(Value::Null) => None,
            Some(Value::String(k)) => {
                valid_key(k)?;
                if !self.keys.insert(k.clone()) {
                    return err(format!("two nodes have the key `{}`", clean_label(k, 40)));
                }
                Some(k.clone())
            }
            Some(_) => return err("a node's `key` must be a string"),
        };
        let mut needs_key = false;
        let kind = match ty {
            "column" | "row" => {
                let s = Stack { children: self.children(o, "children", depth)?, gap: gap(o.get("gap")), align: parse_align(o.get("align")) };
                if ty == "column" { Kind::Column(s) } else { Kind::Row(s) }
            }
            "split" => {
                let children = self.children(o, "children", depth)?;
                if children.len() != 2 {
                    return err("a split needs exactly two children");
                }
                let ratio = o.get("ratio").and_then(Value::as_f64).unwrap_or(0.5).clamp(0.15, 0.85) as f32;
                let min_px = o.get("min_px").and_then(Value::as_f64).unwrap_or(120.0).clamp(0.0, 2000.0) as f32;
                Kind::Split { children, ratio, min_px }
            }
            "scroll" => Kind::Scroll { child: self.one(o, "child", depth)?.ok_or_else(|| ScreenError::new("a scroll needs a `child`"))? },
            "tabs" => {
                needs_key = true;
                let a = match o.get("tabs") {
                    None => &[][..],
                    Some(Value::Array(a)) => a.as_slice(),
                    Some(_) => return err("`tabs` must be an array"),
                };
                if a.len() > MAX_OPTIONS {
                    return err(format!("more than {MAX_OPTIONS} tabs"));
                }
                let mut tabs: Vec<TabDef> = Vec::new();
                for t in a {
                    let m = t.as_object().ok_or_else(|| ScreenError::new("a tab is not an object"))?;
                    let k = item_key(m, "tab")?;
                    if tabs.iter().any(|x| x.key == k) {
                        return err(format!("two tabs have the key `{}`", clean_label(&k, 40)));
                    }
                    let l = label(m, "label");
                    tabs.push(TabDef { label: if l.is_empty() { clean_label(&k, MAX_LABEL) } else { l }, key: k, badge: m.get("badge").and_then(Value::as_str).map(|b| clean_label(b, 40)).filter(|b| !b.is_empty()) });
                }
                let selected = o.get("selected").and_then(Value::as_str).filter(|s| tabs.iter().any(|t| t.key == *s)).map(str::to_owned).or_else(|| tabs.first().map(|t| t.key.clone())).unwrap_or_default();
                Kind::Tabs { tabs, selected, child: self.one(o, "child", depth)? }
            }
            "section" => {
                let collapsible = flag(o, "collapsible", false);
                needs_key = collapsible;
                Kind::Section {
                    title: label(o, "title"),
                    badge: o.get("badge").and_then(Value::as_str).map(|b| clean_label(b, 40)).filter(|b| !b.is_empty()),
                    collapsible,
                    collapsed: collapsible && flag(o, "collapsed", false),
                    children: self.children(o, "children", depth)?,
                }
            }
            "divider" => Kind::Divider,
            "spacer" => Kind::Spacer,
            "toolbar" => Kind::Toolbar { children: self.children(o, "children", depth)? },
            "text" => Kind::Text {
                text: plain(o, "text")?,
                style: match o.get("style").and_then(Value::as_str) {
                    Some("caption") => TextStyle::Caption,
                    Some("title") => TextStyle::Title,
                    Some("mono") => TextStyle::Mono,
                    _ => TextStyle::Body,
                },
                tone: Tone::parse(o.get("tone")),
                wrap: flag(o, "wrap", true),
            },
            "markdown" => Kind::Markdown { blocks: markdown::parse(o.get("source").and_then(Value::as_str).unwrap_or(""))? },
            "badge" => Kind::Badge(Badge { text: label(o, "text"), tone: Tone::parse(o.get("tone")) }),
            "icon" => Kind::Icon { name: icon_name(o), tone: Tone::parse(o.get("tone")) },
            "kv" => {
                let a = match o.get("rows") {
                    None => &[][..],
                    Some(Value::Array(a)) => a.as_slice(),
                    Some(_) => return err("`rows` must be an array"),
                };
                if a.len() > MAX_KV_ROWS {
                    return err(format!("more than {MAX_KV_ROWS} rows in a kv"));
                }
                let mut rows = Vec::new();
                for r in a {
                    let Some(pair) = r.as_array() else { continue };
                    let (Some(k), Some(val)) = (pair.first().and_then(Value::as_str), pair.get(1).and_then(Value::as_str)) else { continue };
                    if val.len() > MAX_STRING {
                        return err("a kv value is over 16 KiB");
                    }
                    rows.push((clean_label(k, MAX_LABEL), val.to_owned()));
                }
                Kind::Kv { rows }
            }
            "empty" => Kind::Empty { title: label(o, "title"), body: plain(o, "body")?, icon: icon_name(o) },
            "progress" => Kind::Progress { value: o.get("value").and_then(Value::as_f64).map(|f| f.clamp(0.0, 1.0) as f32), label: label(o, "label") },
            "comment" => {
                let author = o.get("author").and_then(Value::as_object);
                let actions = self.children(o, "actions", depth)?;
                if actions.iter().any(|a| !matches!(a.kind, Kind::Button(_))) {
                    return err("a comment's `actions` must be buttons");
                }
                Kind::Comment(Comment {
                    author: author.map(|a| label(a, "name")).filter(|n| !n.is_empty()).unwrap_or_else(|| "Unknown".into()),
                    agent: author.and_then(|a| a.get("kind")).and_then(Value::as_str) == Some("agent"),
                    time_ms: o.get("time_ms").and_then(Value::as_u64).unwrap_or(0),
                    body: markdown::parse(o.get("body").and_then(Value::as_str).unwrap_or(""))?,
                    badges: badges(o.get("badges"))?,
                    actions,
                })
            }
            "button" => {
                let bind = match (o.get("action").filter(|a| !a.is_null()), flag(o, "event", false)) {
                    (Some(_), true) => return err("a button has both an `action` and `event`; use one"),
                    (Some(a), false) => Bind::Action(self.action(a)?),
                    (None, true) => {
                        needs_key = true;
                        Bind::Event
                    }
                    (None, false) => Bind::Inert,
                };
                let kind = match o.get("kind").and_then(Value::as_str) {
                    Some("primary") => ButtonKind::Primary,
                    Some("ghost") => ButtonKind::Ghost,
                    Some("danger") => ButtonKind::Danger,
                    _ => ButtonKind::Secondary,
                };
                Kind::Button(Button { label: label(o, "label"), kind, icon: icon_name(o), disabled: flag(o, "disabled", false), bind })
            }
            "text_input" => {
                needs_key = true;
                let max_len = o.get("max_len").and_then(Value::as_u64).map_or(MAX_INPUT_LEN, |n| (n as usize).clamp(1, MAX_INPUT_LEN));
                Kind::TextInput {
                    value: plain(o, "value")?.chars().take(max_len).collect(),
                    placeholder: label(o, "placeholder"),
                    multiline: flag(o, "multiline", false),
                    max_len,
                    submit_label: o.get("submit_label").and_then(Value::as_str).map(|s| clean_label(s, 40)).filter(|s| !s.is_empty()),
                    on_change: flag(o, "on_change", false),
                    reset: flag(o, "reset", false),
                }
            }
            "checkbox" => {
                needs_key = true;
                Kind::Checkbox { label: label(o, "label"), checked: flag(o, "checked", false) }
            }
            "select" | "segmented" => {
                needs_key = true;
                let options = options(o)?;
                let selected = o.get("selected").and_then(Value::as_str).filter(|s| options.iter().any(|x| x.value == *s)).map(str::to_owned).unwrap_or_default();
                if ty == "select" { Kind::Select { options, selected } } else { Kind::Segmented { options, selected } }
            }
            "list" => {
                needs_key = true;
                let a = match o.get("items") {
                    None => &[][..],
                    Some(Value::Array(a)) => a.as_slice(),
                    Some(_) => return err("`items` must be an array"),
                };
                if a.len() > MAX_LIST_ITEMS {
                    return err(format!("more than {MAX_LIST_ITEMS} list items"));
                }
                let mut seen: HashSet<String> = HashSet::new();
                let mut items = Vec::with_capacity(a.len());
                for it in a {
                    let m = it.as_object().ok_or_else(|| ScreenError::new("a list item is not an object"))?;
                    let k = item_key(m, "list")?;
                    if !seen.insert(k.clone()) {
                        return err(format!("two list items have the key `{}`", clean_label(&k, 40)));
                    }
                    items.push(ListItem { key: k, title: label(m, "title"), subtitle: label(m, "subtitle"), icon: icon_name(m), badges: badges(m.get("badges"))? });
                }
                Kind::List { items, selected: o.get("selected").and_then(Value::as_str).map(str::to_owned) }
            }
            "file_list" => {
                needs_key = true;
                let a = match o.get("files") {
                    None => &[][..],
                    Some(Value::Array(a)) => a.as_slice(),
                    Some(_) => return err("`files` must be an array"),
                };
                if a.len() > MAX_LIST_ITEMS {
                    return err(format!("more than {MAX_LIST_ITEMS} files"));
                }
                let mut files = Vec::with_capacity(a.len());
                for f in a {
                    let m = f.as_object().ok_or_else(|| ScreenError::new("a file entry is not an object"))?;
                    let path = normalize_path(m.get("path").and_then(Value::as_str).ok_or_else(|| ScreenError::new("a file entry needs a `path`"))?);
                    if path.chars().count() > 1024 {
                        return err("a file path is over 1024 characters");
                    }
                    let path = path.chars().filter(|c| !c.is_control() && !is_invisible(*c)).collect::<String>();
                    if path.is_empty() {
                        continue;
                    }
                    files.push(FileEntry {
                        path,
                        status: m.get("status").and_then(Value::as_str).and_then(|s| s.chars().next()).filter(|c| "MADRCU?".contains(*c)),
                        old_path: m.get("old_path").and_then(Value::as_str).map(|p| clean_label(&normalize_path(p), 1024)).filter(|p| !p.is_empty()),
                        badges: badges(m.get("badges"))?,
                    });
                }
                let tree = o.get("view").and_then(Value::as_str) != Some("flat");
                Kind::FileList { files, tree, selected: o.get("selected").and_then(Value::as_str).map(normalize_path) }
            }
            "qr" => {
                // A bad text is a bad prop, not a bad tree: the node draws its notice and the rest
                // of the screen is unaffected. The text is read here and not kept.
                let text = o.get("text").and_then(Value::as_str).unwrap_or("");
                let size = match o.get("size").and_then(Value::as_str) {
                    Some("sm") => QrSize::Sm,
                    Some("lg") => QrSize::Lg,
                    _ => QrSize::Md,
                };
                Kind::Qr { label: label(o, "label"), size, code: qr::node_code(text).ok() }
            }
            "code" => {
                let (view, events) = self.code(o, depth)?;
                needs_key = events;
                Kind::Code(Box::new(view))
            }
            "diff" => {
                let (view, events) = self.diff(o, depth)?;
                needs_key = events;
                Kind::Diff(Box::new(view))
            }
            other => Kind::Unsupported { type_name: clean_label(other, 40) },
        };
        if needs_key && key.is_none() {
            return err(format!("a `{ty}` that sends events needs a `key`"));
        }
        let node = Node { key, kind };
        self.bytes = self.bytes.saturating_add(node.own_bytes());
        if self.bytes > MAX_TREE_BYTES {
            return err(format!("the tree is over {} KiB", MAX_TREE_BYTES / 1024));
        }
        Ok(node)
    }
}

fn parse_align(v: Option<&Value>) -> Align {
    match v.and_then(Value::as_str) {
        Some("center") => Align::Center,
        Some("end") => Align::End,
        Some("stretch") => Align::Stretch,
        _ => Align::Start,
    }
}

// ---------------------------------------------------------------------------------------------
// Events, and the render rate
// ---------------------------------------------------------------------------------------------

/// What happened in a screen, sent to its plugin as a `screen/event` notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Click,
    Select,
    Toggle,
    Submit,
    Change,
    /// Reserved for the code and diff widgets.
    RangeAction,
    /// Reserved for the code and diff widgets.
    ThreadToggle,
    /// A `csm:` link inside the screen's text.
    Link,
    /// Reserved for blob ranges.
    Visible,
}

impl EventKind {
    pub fn name(self) -> &'static str {
        match self {
            EventKind::Click => "click",
            EventKind::Select => "select",
            EventKind::Toggle => "toggle",
            EventKind::Submit => "submit",
            EventKind::Change => "change",
            EventKind::RangeAction => "range_action",
            EventKind::ThreadToggle => "thread_toggle",
            EventKind::Link => "link",
            EventKind::Visible => "visible",
        }
    }

    /// Events that may be coalesced when the plugin's queue is full (latest wins).
    pub fn coalesces(self) -> bool {
        matches!(self, EventKind::Change | EventKind::Visible)
    }
}

/// An event raised by a widget. `key` is the node's key (or a list item's, for `select`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenEvent {
    pub key: String,
    pub kind: EventKind,
    pub value: Value,
}

impl ScreenEvent {
    pub fn new(key: impl Into<String>, kind: EventKind, value: Value) -> Self {
        ScreenEvent { key: key.into(), kind, value }
    }

    /// The `params` of the `screen/event` notification. `rev` is the revision the person saw.
    pub fn params(&self, instance: &str, rev: u64) -> Value {
        serde_json::json!({"instance": instance, "rev": rev, "key": self.key, "kind": self.kind.name(), "value": self.value})
    }
}

/// Lets at most one render in [`MIN_RENDER_INTERVAL_MS`] through; the caller keeps the latest
/// one it held back and applies it when due. Time is passed in, so this is testable.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateGate {
    last: Option<u64>,
}

impl RateGate {
    pub const fn new() -> Self {
        RateGate { last: None }
    }

    /// Whether an update may be published at `now_ms` (and records it if so).
    pub fn admit(&mut self, now_ms: u64) -> bool {
        match self.last {
            Some(l) if now_ms.saturating_sub(l) < MIN_RENDER_INTERVAL_MS && now_ms >= l => false,
            _ => {
                self.last = Some(now_ms);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod code_tests;

#[cfg(test)]
mod qr_tests;

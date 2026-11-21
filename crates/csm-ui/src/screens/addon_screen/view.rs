//! The `code` and `diff` widgets: rows (a code line, a unified diff line, a side-by-side pair, a
//! file or hunk banner) over a fixed row height, drawn only for what is on screen, with line
//! numbers, a line-range selection, gutter marks, inline comment threads and range actions.
//!
//! The plugin's text is hostile and is only ever *shown*: control, escape, bidi and zero-width
//! characters become visible markers (`reveal`), a line is cut at [`MAX_COLS`] columns with a
//! marker, and nothing in the text is interpreted. Highlight colours come from the worker
//! ([`super::hl`]); a frame that has none yet draws plain text.
//!
//! What a frame asks of the plugin is an event: `range_action {id, side, start, end}` when a
//! range action is clicked, `thread_toggle {key}` for a closed thread's badge. The selection
//! itself is UI state and is never sent.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use csm_core::addons::screen::{CodeContent, CodeView, DiffContent, DiffView, Doc, EventKind, Mark, Overlay, Tone, Withheld, reveal};
use csm_core::diff::{FileStatus, LineKind, Patch, Side, align_split};
use csm_services::highlight::{Span, TokenClass};
use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Align2, Color32, CornerRadius, Id, Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo, WidgetType, pos2, vec2};
use serde_json::json;

use super::hl::{Hl, Reason};
use super::rows::RowLayout;
use super::tree;
use super::Cx;
use crate::theme::{self, Tokens, alpha, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

/// A line is drawn up to this many columns; the rest is cut and the cut is marked.
pub const MAX_COLS: usize = 4_000;
/// What an open thread is assumed to be tall until it has been drawn once.
const THREAD_GUESS_H: f32 = 140.0;
/// The tallest an open thread's block is laid out.
const THREAD_MAX_H: f32 = 3_000.0;

#[derive(Clone, Copy, Debug)]
struct Src {
    doc: u32,
    line: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BannerKind {
    File,
    Hunk,
    Notice,
}

#[derive(Clone, Copy, Debug)]
struct Cell {
    no: u32,
    src: Src,
    kind: LineKind,
    no_eol: bool,
    cut: bool,
}

#[derive(Clone, Copy, Debug)]
struct LineRow {
    old: Option<u32>,
    new: Option<u32>,
    kind: LineKind,
    src: Src,
    no_eol: bool,
    cut: bool,
}

#[derive(Clone, Debug)]
enum Row {
    Banner(Arc<str>, BannerKind),
    Line(LineRow),
    Pair(Option<Cell>, Option<Cell>),
}

impl Row {
    /// The line number this row has on `side`.
    fn no(&self, side: Side) -> Option<u32> {
        match self {
            Row::Line(l) => match side {
                Side::Old => l.old,
                Side::New => l.new,
            },
            Row::Pair(l, r) => match side {
                Side::Old => l.map(|c| c.no),
                Side::New => r.map(|c| c.no),
            },
            Row::Banner(..) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gutter {
    /// One number column (code).
    Single,
    /// Old and new number columns (unified diff).
    Double,
    /// A number column in each half (side by side).
    Split,
}

struct DocRef {
    doc: Arc<Doc>,
    language: String,
    path: String,
}

enum Body {
    /// Row `i` is line `i` of document 0, numbered from `first`.
    Code { first: u32, len: usize },
    Table(Vec<Row>),
}

/// Everything about a code or diff node the frame needs, built once per content (not per frame).
pub(crate) struct Model {
    id: u64,
    gutter: Gutter,
    docs: Vec<DocRef>,
    body: Body,
    /// Table rows by `(side, number)` (first row that has it).
    by_line: HashMap<(Side, u32), usize>,
    max_no: u32,
    max_cols: usize,
    /// The patch a diff model was built from. Held here so its address (the model's id) cannot
    /// be reused by a later patch while this model is cached.
    _patch: Option<Arc<Patch>>,
}

fn cols(s: &str) -> usize {
    s.chars().map(|c| if c == '\t' { 4 } else { 1 }).sum::<usize>().min(MAX_COLS + 1)
}

impl Model {
    fn n_rows(&self) -> usize {
        match &self.body {
            Body::Code { len, .. } => *len,
            Body::Table(r) => r.len(),
        }
    }

    fn row(&self, i: usize) -> Option<Row> {
        match &self.body {
            Body::Code { first, len } => (i < *len).then(|| {
                let no = first.saturating_add(i as u32);
                Row::Line(LineRow { old: None, new: Some(no), kind: LineKind::Context, src: Src { doc: 0, line: i as u32 }, no_eol: false, cut: false })
            }),
            Body::Table(r) => r.get(i).cloned(),
        }
    }

    fn row_of(&self, side: Side, no: u32) -> Option<usize> {
        match &self.body {
            Body::Code { first, len } => {
                let i = no.checked_sub(*first)? as usize;
                (side == Side::New && i < *len).then_some(i)
            }
            Body::Table(_) => self.by_line.get(&(side, no)).copied(),
        }
    }

    fn line(&self, s: Src) -> &str {
        self.docs.get(s.doc as usize).map_or("", |d| d.doc.line(s.line as usize))
    }

    /// The next line number on `side` after `from` going `down` or up, if there is one.
    fn step(&self, side: Side, from: u32, down: bool) -> Option<u32> {
        match &self.body {
            Body::Code { first, len } => {
                let last = first.saturating_add((*len as u32).saturating_sub(1));
                if down { (from < last).then(|| from + 1) } else { (from > *first).then(|| from - 1) }
            }
            Body::Table(rows) => {
                let start = self.row_of(side, from)?;
                if down {
                    rows.iter().skip(start + 1).find_map(|r| r.no(side))
                } else {
                    rows.iter().take(start).rev().find_map(|r| r.no(side))
                }
            }
        }
    }

    /// The text of the lines `start..=end` on `side`, for the clipboard.
    fn selected_text(&self, side: Side, start: u32, end: u32) -> String {
        let mut out: Vec<&str> = Vec::new();
        match &self.body {
            Body::Code { .. } => {
                if let (Some(a), Some(b)) = (self.row_of(side, start), self.row_of(side, end)) {
                    for i in a..=b.min(a + 200_000) {
                        out.push(self.line(Src { doc: 0, line: i as u32 }));
                    }
                }
            }
            Body::Table(rows) => {
                for r in rows {
                    let cell = match r {
                        Row::Line(l) => l.no_of(side).map(|n| (n, l.src)),
                        Row::Pair(a, b) => match side {
                            Side::Old => a.map(|c| (c.no, c.src)),
                            Side::New => b.map(|c| (c.no, c.src)),
                        },
                        Row::Banner(..) => None,
                    };
                    if let Some((n, src)) = cell
                        && n >= start
                        && n <= end
                    {
                        out.push(self.line(src));
                    }
                }
            }
        }
        out.join("\n")
    }
}

impl LineRow {
    fn no_of(&self, side: Side) -> Option<u32> {
        match side {
            Side::Old => self.old,
            Side::New => self.new,
        }
    }
}

fn hash_of<T: Hash>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn code_model(v: &CodeView, doc: &Arc<Doc>) -> Model {
    let max_cols = (0..doc.len()).map(|i| cols(doc.line(i))).max().unwrap_or(0);
    let last = v.first_line.saturating_add((doc.len() as u32).saturating_sub(1));
    Model {
        id: code_id(v, doc),
        gutter: Gutter::Single,
        docs: vec![DocRef { doc: doc.clone(), language: v.language.clone(), path: v.path.clone() }],
        body: Body::Code { first: v.first_line, len: doc.len() },
        by_line: HashMap::new(),
        max_no: last,
        max_cols,
        _patch: None,
    }
}

fn code_id(v: &CodeView, doc: &Arc<Doc>) -> u64 {
    hash_of(&(doc.hash(), doc.text().len(), v.first_line, &v.language, &v.path))
}

fn file_banner(f: &csm_core::diff::FileDiff) -> String {
    let (a, d) = f.counts();
    let name = match (f.status, &f.old_path, &f.new_path) {
        (FileStatus::Renamed | FileStatus::Copied, Some(o), Some(n)) => format!("{o} \u{2192} {n}"),
        _ => f.path().to_owned(),
    };
    format!("{}  {name}   +{a} \u{2212}{d}", f.status.letter())
}

fn diff_model(v: &DiffView, patch: &Arc<Patch>) -> Model {
    let mut docs: Vec<DocRef> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut by_line: HashMap<(Side, u32), usize> = HashMap::new();
    let mut max_no = 0u32;
    let mut max_cols = 0usize;
    let split = v.split;
    if patch.files.is_empty() {
        rows.push(Row::Banner("No changes to show.".into(), BannerKind::Notice));
    }
    for f in &patch.files {
        rows.push(Row::Banner(file_banner(f).into(), BannerKind::File));
        if f.binary {
            rows.push(Row::Banner("Binary file: not shown.".into(), BannerKind::Notice));
            continue;
        }
        if f.hunks.is_empty() {
            rows.push(Row::Banner("No line changes (a rename, a copy or a mode change).".into(), BannerKind::Notice));
            continue;
        }
        // Two documents per file (old text, new text) so each side highlights as the code it is.
        let (mut old, mut new) = (String::new(), String::new());
        let (mut old_src, mut new_src): (HashMap<u32, u32>, HashMap<u32, u32>) = (HashMap::new(), HashMap::new());
        let (mut old_n, mut new_n) = (0u32, 0u32);
        for h in &f.hunks {
            for l in &h.lines {
                if l.kind != LineKind::Add {
                    old.push_str(&l.text);
                    old.push('\n');
                    if let Some(no) = l.old_no {
                        old_src.insert(no, old_n);
                    }
                    old_n += 1;
                }
                if l.kind != LineKind::Del {
                    new.push_str(&l.text);
                    new.push('\n');
                    if let Some(no) = l.new_no {
                        new_src.insert(no, new_n);
                    }
                    new_n += 1;
                }
                max_cols = max_cols.max(cols(&l.text));
            }
        }
        let base = docs.len() as u32;
        let path = f.path().to_owned();
        docs.push(DocRef { doc: Arc::new(Doc::new(old)), language: v.language.clone(), path: path.clone() });
        docs.push(DocRef { doc: Arc::new(Doc::new(new)), language: v.language.clone(), path });
        let src_old = |no: u32| Src { doc: base, line: old_src.get(&no).copied().unwrap_or(0) };
        let src_new = |no: u32| Src { doc: base + 1, line: new_src.get(&no).copied().unwrap_or(0) };
        for h in &f.hunks {
            rows.push(Row::Banner(format!("@@ -{},{} +{},{} @@ {}", h.old_start, h.old_len, h.new_start, h.new_len, h.heading).trim_end().into(), BannerKind::Hunk));
            if split {
                for r in align_split(h) {
                    let cell = |c: csm_core::diff::SplitCell<'_>, old_side: bool| Cell {
                        no: c.no,
                        src: if old_side { src_old(c.no) } else { src_new(c.no) },
                        kind: c.line.kind,
                        no_eol: c.line.no_eol,
                        cut: c.line.cut,
                    };
                    let (l, rr) = (r.old.map(|c| cell(c, true)), r.new.map(|c| cell(c, false)));
                    let idx = rows.len();
                    if let Some(c) = l {
                        by_line.entry((Side::Old, c.no)).or_insert(idx);
                        max_no = max_no.max(c.no);
                    }
                    if let Some(c) = rr {
                        by_line.entry((Side::New, c.no)).or_insert(idx);
                        max_no = max_no.max(c.no);
                    }
                    rows.push(Row::Pair(l, rr));
                }
            } else {
                for l in &h.lines {
                    let idx = rows.len();
                    let src = match (l.kind, l.new_no, l.old_no) {
                        (LineKind::Del, _, Some(o)) => src_old(o),
                        (_, Some(n), _) => src_new(n),
                        _ => Src { doc: base + 1, line: 0 },
                    };
                    if let Some(o) = l.old_no {
                        by_line.entry((Side::Old, o)).or_insert(idx);
                        max_no = max_no.max(o);
                    }
                    if let Some(n) = l.new_no {
                        by_line.entry((Side::New, n)).or_insert(idx);
                        max_no = max_no.max(n);
                    }
                    rows.push(Row::Line(LineRow { old: l.old_no, new: l.new_no, kind: l.kind, src, no_eol: l.no_eol, cut: l.cut }));
                }
            }
        }
    }
    if patch.truncated {
        rows.push(Row::Banner("The diff was cut short: it has more files or lines than CSM shows.".into(), BannerKind::Notice));
    }
    Model { id: diff_id(v, patch), gutter: if split { Gutter::Split } else { Gutter::Double }, docs, body: Body::Table(rows), by_line, max_no, max_cols, _patch: Some(patch.clone()) }
}

fn diff_id(v: &DiffView, patch: &Arc<Patch>) -> u64 {
    hash_of(&(Arc::as_ptr(patch) as usize, v.split, &v.language))
}

// ---------------------------------------------------------------------------------------------
// UI state
// ---------------------------------------------------------------------------------------------

/// The line range the person selected: one side, from `anchor` to `head` (either order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sel {
    pub side: Side,
    pub anchor: u32,
    pub head: u32,
    dragging: bool,
}

impl Sel {
    pub fn range(&self) -> (u32, u32) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

/// Per-view UI state kept by [`super::ScreenUi`], keyed by (instance, node key).
#[derive(Default)]
pub(crate) struct ViewState {
    pub(crate) sels: HashMap<(String, String), Sel>,
    pub(crate) hscroll: HashMap<(String, String), f32>,
    /// A thread block's measured height, by (instance, node key, thread key): (width, height).
    pub(crate) thread_h: HashMap<(String, String, String), (f32, f32)>,
    pub(crate) models: HashMap<(String, String), Arc<Model>>,
    pub(crate) scrolled: HashMap<(String, String), u32>,
    /// Views that had keyboard focus last frame.
    pub(crate) had_focus: std::collections::HashSet<(String, String)>,
}

impl ViewState {
    pub(crate) fn sync(&mut self, alive: &dyn Fn(&str) -> bool) {
        self.sels.retain(|(i, _), _| alive(i));
        self.hscroll.retain(|(i, _), _| alive(i));
        self.thread_h.retain(|(i, _, _), _| alive(i));
        self.models.retain(|(i, _), _| alive(i));
        self.scrolled.retain(|(i, _), _| alive(i));
        self.had_focus.retain(|(i, _)| alive(i));
    }
}

// ---------------------------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------------------------

/// The state key of a view. A keyless node sends nothing, so it only needs its own slot: its
/// position in the layout tells it apart from the other keyless views of the same screen.
fn state_key(ui: &mut Ui, key: &str) -> String {
    if key.is_empty() { format!("~{}", ui.next_auto_id().value()) } else { key.to_owned() }
}

pub(super) fn code(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, v: &CodeView) {
    let key = state_key(ui, key);
    match &v.content {
        CodeContent::Withheld(w) => notice(ui, cx, w),
        CodeContent::Text(doc) if doc.is_empty() => notice(ui, cx, &Withheld("This file is empty.".into())),
        CodeContent::Text(doc) => {
            let m = cached(cx, &key, code_id(v, doc), || code_model(v, doc));
            show(ui, cx, &key, &m, &v.overlay, &v.highlight_lines);
        }
    }
}

pub(super) fn diff(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, v: &DiffView) {
    let key = state_key(ui, key);
    match &v.content {
        DiffContent::Withheld(w) => notice(ui, cx, w),
        DiffContent::Patch(p) => {
            let m = cached(cx, &key, diff_id(v, p), || diff_model(v, p));
            show(ui, cx, &key, &m, &v.overlay, &[]);
        }
    }
}

fn cached(cx: &mut Cx<'_>, key: &str, id: u64, build: impl FnOnce() -> Model) -> Arc<Model> {
    let k = (cx.info.instance.clone(), key.to_owned());
    if let Some(m) = cx.st.view.models.get(&k)
        && m.id == id
    {
        return m.clone();
    }
    let m = Arc::new(build());
    cx.st.view.models.insert(k, m.clone());
    m
}

/// "Too large", "empty": a CSM-drawn notice in place of the view.
fn notice(ui: &mut Ui, cx: &Cx<'_>, w: &Withheld) {
    let t = cx.t;
    egui::Frame::new().fill(t.mantle).stroke(Stroke::new(size::HAIRLINE, t.surface0)).corner_radius(CornerRadius::same(radius::MD)).inner_margin(egui::Margin::same(space::MD as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let r = ui.add(egui::Label::new(egui::RichText::new(&w.0).font(theme::regular(text::BODY)).color(t.muted)).wrap());
        r.widget_info({
            let l = w.0.clone();
            move || WidgetInfo::labeled(WidgetType::Label, true, l.clone())
        });
    });
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

pub(super) fn syn_color(t: &Tokens, c: TokenClass) -> Color32 {
    match c {
        TokenClass::Plain => t.fg,
        TokenClass::Keyword => t.syn_keyword,
        TokenClass::String => t.syn_string,
        TokenClass::Comment => t.syn_comment,
        TokenClass::Number => t.syn_number,
        TokenClass::Type => t.syn_type,
        TokenClass::Function => t.syn_function,
        TokenClass::Constant => t.syn_constant,
        TokenClass::Macro => t.syn_macro,
        TokenClass::Operator => t.syn_operator,
        TokenClass::Punctuation => t.syn_punct,
        TokenClass::Tag => t.syn_tag,
        TokenClass::Attribute => t.syn_attribute,
    }
}

fn tone_color(t: &Tokens, tone: Tone) -> Color32 {
    tree::tone_color(t, tone)
}

fn push_revealed(job: &mut LayoutJob, t: &Tokens, s: &str, font: &egui::FontId, color: Color32) {
    for seg in reveal(s) {
        let fmt = TextFormat {
            font_id: font.clone(),
            color: if seg.marker { t.yellow } else { color },
            background: if seg.marker { Tokens::tint(t.yellow, alpha::PILL) } else { Color32::TRANSPARENT },
            ..TextFormat::default()
        };
        job.append(&seg.text, 0.0, fmt);
    }
}

/// One line as a layout job: highlight spans coloured, invisible characters marked, cut at
/// [`MAX_COLS`] with a marker.
fn line_job(t: &Tokens, line: &str, spans: &[Span], cut: bool, font: &egui::FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let end = line.char_indices().nth(MAX_COLS).map_or(line.len(), |(i, _)| i);
    let truncated = end < line.len() || cut;
    let mut pos = 0usize;
    let plain_to = |job: &mut LayoutJob, to: usize, pos: &mut usize| {
        if to > *pos
            && let Some(s) = line.get(*pos..to.min(end))
        {
            push_revealed(job, t, s, font, t.fg);
        }
        *pos = (*pos).max(to);
    };
    for sp in spans {
        let (a, b) = (sp.start as usize, (sp.end as usize).min(end));
        if a >= end || a < pos || b <= a {
            continue;
        }
        plain_to(&mut job, a, &mut pos);
        if let Some(s) = line.get(a..b) {
            push_revealed(&mut job, t, s, font, syn_color(t, sp.class));
        }
        pos = b;
    }
    plain_to(&mut job, end, &mut pos);
    if truncated {
        let fmt = TextFormat { font_id: font.clone(), color: t.muted, background: Tokens::tint(t.muted, alpha::PILL), ..TextFormat::default() };
        job.append("\u{27E8}\u{2026} line cut\u{27E9}", 0.0, fmt);
    }
    if job.text.is_empty() {
        job.append(" ", 0.0, TextFormat { font_id: font.clone(), color: t.fg, ..TextFormat::default() });
    }
    job
}

/// The plain text of a line as drawn (for the accessibility tree).
fn shown(line: &str) -> String {
    let end = line.char_indices().nth(MAX_COLS).map_or(line.len(), |(i, _)| i);
    reveal(line.get(..end).unwrap_or("")).into_iter().map(|g| g.text).collect()
}

struct Geo {
    gutter: Gutter,
    x0: f32,
    w: f32,
    num_w: f32,
    mark_w: f32,
    bar_w: f32,
    sign_w: f32,
}

impl Geo {
    /// The x range of one half (the whole width unless side by side).
    fn half(&self, side: Side) -> (f32, f32) {
        match self.gutter {
            Gutter::Split => {
                let mid = self.x0 + (self.w / 2.0).floor();
                match side {
                    Side::Old => (self.x0, mid),
                    Side::New => (mid, self.x0 + self.w),
                }
            }
            _ => (self.x0, self.x0 + self.w),
        }
    }

    fn gutter_w(&self) -> f32 {
        self.mark_w + self.bar_w + if self.gutter == Gutter::Double { self.num_w * 2.0 } else { self.num_w }
    }

    /// Where the text starts in the half for `side`.
    fn text_x(&self, side: Side) -> f32 {
        self.half(side).0 + self.gutter_w() + if self.gutter == Gutter::Double { self.sign_w } else { 0.0 }
    }

    /// The number cell for `side` in a row at `top`.
    fn num_cell(&self, side: Side, top: f32, h: f32) -> Rect {
        let (x0, _) = self.half(side);
        let x = match (self.gutter, side) {
            (Gutter::Double, Side::New) => x0 + self.mark_w + self.num_w,
            _ => x0 + self.mark_w,
        };
        Rect::from_min_size(pos2(x, top), vec2(self.num_w, h))
    }

    fn mark_cell(&self, side: Side, top: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(self.half(side).0, top), vec2(self.mark_w, h))
    }
}

fn digits(n: u32) -> usize {
    n.max(1).to_string().len()
}

#[allow(clippy::too_many_lines)]
fn show(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, m: &Arc<Model>, overlay: &Overlay, highlight_lines: &[u32]) {
    let t = cx.t;
    let instance = cx.info.instance.clone();
    let vkey = (instance.clone(), key.to_owned());
    let font = theme::mono(text::MONO);
    let cw = widgets::text_width(ui, "M", font.clone()).max(1.0);
    let lh = ui.ctx().fonts_mut(|f| f.layout_no_wrap("M".into(), font.clone(), Color32::PLACEHOLDER).size().y);
    let row_h = (lh + space::XS).ceil();
    let width = ui.available_width().max(120.0);
    let height = {
        let h = ui.available_height();
        if h.is_finite() && h > 80.0 { h.min(4000.0) } else { 480.0 }
    };
    let outer = Rect::from_min_size(ui.cursor().min, vec2(width, height));
    ui.allocate_rect(outer, Sense::hover());
    let mut inner = ui.new_child(UiBuilder::new().max_rect(outer).layout(egui::Layout::top_down(Align::Min)));
    inner.set_clip_rect(outer.intersect(ui.clip_rect()));
    inner.painter().rect_filled(outer, 0.0, t.bg);
    // A keyed node keeps its scroll position and focus; a node without a key (it sends nothing) is told apart by where it sits.
    let uniq = if key.is_empty() { ui.next_auto_id().value() } else { 0 };
    let view_id = Id::new(("code-view", &instance, key, uniq));

    let n = m.n_rows();
    let last_row = n.saturating_sub(1);
    // Where each thread sits: the row of its last line, or the end of the view when that line is not shown.
    let placed: Vec<usize> = overlay.threads.iter().map(|th| m.row_of(th.anchor.side, th.anchor.end).unwrap_or(last_row)).collect();
    let mut open: Vec<usize> = (0..overlay.threads.len()).filter(|i| overlay.threads.get(*i).is_some_and(|th| th.open)).collect();
    open.sort_by_key(|i| placed.get(*i).copied().unwrap_or(0));
    let inserts: Vec<(usize, f32)> = open
        .iter()
        .filter_map(|i| {
            let th = overlay.threads.get(*i)?;
            let h = cx.st.view.thread_h.get(&(instance.clone(), key.to_owned(), th.key.clone())).map_or(THREAD_GUESS_H, |(_, h)| *h);
            Some((placed.get(*i).copied().unwrap_or(0), h))
        })
        .collect();
    let layout = RowLayout::new(n, row_h, inserts);

    let digits = digits(m.max_no).max(2);
    let geo = Geo { gutter: m.gutter, x0: 0.0, w: width, num_w: digits as f32 * cw + space::SM * 2.0, mark_w: size::GUTTER_MARK, bar_w: size::GUTTER_BAR, sign_w: cw + space::XS };
    let min_half_text = {
        let (a, b) = geo.half(Side::Old);
        let w1 = b - a - geo.gutter_w() - if m.gutter == Gutter::Double { geo.sign_w } else { 0.0 };
        if m.gutter == Gutter::Split { w1 } else { width - geo.gutter_w() - if m.gutter == Gutter::Double { geo.sign_w } else { 0.0 } }
    };
    let text_w_all = m.max_cols as f32 * cw + cw * 8.0;
    let max_h = (text_w_all - min_half_text).max(0.0);
    let mut hscroll = cx.st.view.hscroll.get(&vkey).copied().unwrap_or(0.0).clamp(0.0, max_h);

    // Selection, keyboard, copy.
    let mut sel = cx.st.view.sels.get(&vkey).copied();
    let focused = ui.memory(|mem| mem.has_focus(view_id));
    // egui drops focus on Escape before we look, so Escape counts when the view had it last frame.
    let had_focus = cx.st.view.had_focus.contains(&vkey);
    if focused {
        cx.st.view.had_focus.insert(vkey.clone());
    } else {
        cx.st.view.had_focus.remove(&vkey);
    }
    if focused || had_focus {
        let (up, down, esc, shift, copy) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::Escape), i.modifiers.shift, i.events.iter().any(|e| matches!(e, egui::Event::Copy))));
        if let Some(s) = sel.as_mut()
            && focused
        {
            if shift && (up || down)
                && let Some(next) = m.step(s.side, s.head, down)
            {
                s.head = next;
            }
            if copy {
                let (a, b) = s.range();
                let txt = m.selected_text(s.side, a, b);
                ui.ctx().copy_text(txt);
            }
        }
        if esc {
            sel = None;
        }
    }

    let marks_by: HashMap<(Side, u32), Vec<&Mark>> = {
        let mut map: HashMap<(Side, u32), Vec<&Mark>> = HashMap::new();
        for mk in &overlay.marks {
            map.entry((mk.side, mk.line)).or_default().push(mk);
        }
        map
    };
    let mut closed_at: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, th) in overlay.threads.iter().enumerate() {
        if !th.open {
            closed_at.entry(placed.get(i).copied().unwrap_or(0)).or_default().push(i);
        }
    }

    let mut events: Vec<(EventKind, serde_json::Value)> = Vec::new();
    let mut new_sel = sel;
    let mut scroll_to: Option<Rect> = None; // set when a scroll was asked for this frame
    let scroll_target = overlay.scroll_to.filter(|n| cx.st.view.scrolled.get(&vkey) != Some(n));
    let mut measured: Vec<(String, f32, f32)> = Vec::new();

    let area = egui::ScrollArea::vertical().id_salt(("code-scroll", &instance, key, uniq)).auto_shrink([false, false]).show_viewport(&mut inner, |ui, vp| {
        let w = ui.available_width();
        let (content, _) = ui.allocate_exact_size(vec2(w, layout.total().max(row_h)), Sense::hover());
        let geo = Geo { x0: content.min.x, w, ..geo };
        let top = content.min.y;
        let ytop = |y: f32| top + y;
        let vis = Rect::from_min_max(pos2(content.min.x, top + vp.min.y), pos2(content.max.x, top + vp.max.y));
        let painter = ui.painter().clone();
        // The body: focus on click, lowest in the stack.
        let body = ui.interact(vis, view_id, Sense::click());
        if body.clicked() {
            body.request_focus();
        }
        let first = layout.row_at(vp.min.y).unwrap_or(0);
        let last = layout.row_at(vp.max.y).unwrap_or(0);
        let mut hl_cache: HashMap<u32, Hl> = HashMap::new();
        let mut spans_for = |cx: &mut Cx<'_>, ui: &Ui, src: Src| -> Option<Arc<csm_services::highlight::Highlighted>> {
            let d = m.docs.get(src.doc as usize)?;
            let h = hl_cache.entry(src.doc).or_insert_with(|| cx.st.hl.get(ui.ctx(), &d.doc, &d.language, &d.path)).clone();
            match h {
                Hl::Ready(r) => Some(r),
                Hl::Pending | Hl::Off(Reason::NoSyntax | Reason::TooLarge) => None,
            }
        };
        let sel_range = sel.map(|s| (s.side, s.range()));
        let in_sel = |side: Side, no: Option<u32>| -> bool { matches!((sel_range, no), (Some((s, (a, b))), Some(n)) if s == side && n >= a && n <= b) };

        // Thread bars in the gutter, under everything else.
        for (i, th) in overlay.threads.iter().enumerate() {
            let (Some(rs), Some(re)) = (m.row_of(th.anchor.side, th.anchor.start), m.row_of(th.anchor.side, th.anchor.end)) else { continue };
            let (y0, y1) = (ytop(layout.row_top(rs.min(re))), ytop(layout.row_top(rs.max(re)) + row_h));
            if y1 < vis.min.y || y0 > vis.max.y {
                continue;
            }
            let (hx0, _) = geo.half(th.anchor.side);
            let x = hx0 + geo.gutter_w() - geo.bar_w + 2.0;
            let _ = i;
            painter.rect_filled(Rect::from_min_max(pos2(x, y0), pos2(x + size::THREAD_BAR, y1)), 0.0, t.thread_bar);
        }

        let y_row = |i: usize| ytop(layout.row_top(i));
        for i in first..=last.min(last_row) {
            let Some(row) = m.row(i) else { break };
            let ry = y_row(i);
            let rrect = Rect::from_min_size(pos2(content.min.x, ry), vec2(w, row_h));
            if rrect.max.y < vis.min.y || rrect.min.y > vis.max.y {
                continue;
            }
            match &row {
                Row::Banner(txt, kind) => {
                    let (bg, font, color) = match kind {
                        BannerKind::File => (t.mantle, semibold(text::BODY), t.fg2),
                        BannerKind::Hunk => (t.diff_hunk_bg, theme::mono(text::MONO_SM), t.muted),
                        BannerKind::Notice => (Color32::TRANSPARENT, theme::regular(text::CAPTION), t.muted),
                    };
                    painter.rect_filled(rrect, 0.0, bg);
                    if *kind == BannerKind::File {
                        painter.hline(rrect.x_range(), rrect.max.y - 0.5, Stroke::new(size::HAIRLINE, t.surface0));
                    }
                    // Banners carry names and headings from the plugin: shown with invisible characters marked.
                    let shown_txt = shown(txt);
                    painter.with_clip_rect(rrect).text(pos2(rrect.min.x + space::MD, rrect.center().y), Align2::LEFT_CENTER, &shown_txt, font, color);
                    let r = ui.interact(rrect, Id::new((view_id, "banner", i)), Sense::hover());
                    r.widget_info({
                        let l = shown_txt.clone();
                        move || WidgetInfo::labeled(WidgetType::Label, true, l.clone())
                    });
                }
                Row::Line(lr) => {
                    let side = if lr.old.is_some() && lr.new.is_none() { Side::Old } else { Side::New };
                    let bg = match lr.kind {
                        LineKind::Add => Some(t.diff_add_bg),
                        LineKind::Del => Some(t.diff_del_bg),
                        LineKind::Context => None,
                    };
                    if let Some(c) = bg {
                        painter.rect_filled(rrect, 0.0, c);
                    }
                    if lr.new.is_some_and(|n| highlight_lines.binary_search(&n).is_ok()) {
                        painter.rect_filled(rrect, 0.0, Tokens::tint(t.yellow, alpha::SOFT));
                    }
                    let selected = in_sel(Side::Old, lr.old) || in_sel(Side::New, lr.new);
                    if selected {
                        painter.rect_filled(rrect, 0.0, t.range_sel_bg);
                    }
                    // Gutter numbers.
                    let nums: &[(Side, Option<u32>)] = &[(Side::Old, lr.old), (Side::New, lr.new)];
                    for (s, no) in nums {
                        let Some(no) = no else { continue };
                        if geo.gutter == Gutter::Single && *s == Side::Old {
                            continue;
                        }
                        let cell = geo.num_cell(*s, ry, row_h);
                        let on = sel.is_some_and(|x| x.side == *s && x.range().0 <= *no && *no <= x.range().1);
                        painter.text(pos2(cell.max.x - space::XS, cell.center().y), Align2::RIGHT_CENTER, no.to_string(), theme::mono(text::MONO_SM), if on { t.fg } else { t.gutter_fg });
                        gutter_hit(ui, cx, view_id, &mut new_sel, &layout, top, m, &geo, *s, *no, cell, i);
                    }
                    marks_and_badges(ui, &painter, &t, &geo, side, ry, row_h, lr.old, lr.new, &marks_by, closed_at.get(&i), overlay, key, &mut events);
                    // Sign and text.
                    let tx = geo.text_x(Side::New);
                    if geo.gutter == Gutter::Double {
                        let (sign, c) = match lr.kind {
                            LineKind::Add => ("+", t.green),
                            LineKind::Del => ("\u{2212}", t.red),
                            LineKind::Context => (" ", t.muted),
                        };
                        painter.text(pos2(tx - geo.sign_w + 2.0, rrect.center().y), Align2::LEFT_CENTER, sign, font.clone(), c);
                    }
                    let clip = Rect::from_min_max(pos2(tx, rrect.min.y), pos2(rrect.max.x, rrect.max.y));
                    let spans = spans_for(cx, ui, lr.src);
                    let line = m.line(lr.src);
                    let sp = spans.as_ref().map_or(&[][..], |h| h.line(lr.src.line as usize));
                    let job = line_job(&t, line, sp, lr.cut, &font);
                    let galley = painter.layout_job(job);
                    painter.with_clip_rect(clip.intersect(vis)).galley(pos2(tx - hscroll, rrect.center().y - galley.size().y / 2.0), galley, t.fg);
                    if lr.no_eol {
                        painter.with_clip_rect(clip).text(pos2(clip.max.x - space::SM, rrect.center().y), Align2::RIGHT_CENTER, "no newline at end", theme::regular(text::TINY), t.muted);
                    }
                    let r = ui.interact(Rect::from_min_max(pos2(tx, rrect.min.y), rrect.max), Id::new((view_id, "row", i)), Sense::hover());
                    r.widget_info({
                        let kind = match lr.kind {
                            LineKind::Add => "Added: ",
                            LineKind::Del => "Removed: ",
                            LineKind::Context => "",
                        };
                        let l = format!("{kind}{}", shown(line));
                        move || WidgetInfo::labeled(WidgetType::Label, true, l.clone())
                    });
                }
                Row::Pair(l, r) => {
                    for (side, cell) in [(Side::Old, l), (Side::New, r)] {
                        let (hx0, hx1) = geo.half(side);
                        let half = Rect::from_min_max(pos2(hx0, ry), pos2(hx1, ry + row_h));
                        match cell {
                            None => {
                                painter.rect_filled(half, 0.0, Tokens::tint(t.surface0, alpha::CODE_FILLER));
                            }
                            Some(c) => {
                                match c.kind {
                                    LineKind::Add => {
                                        painter.rect_filled(half, 0.0, t.diff_add_bg);
                                    }
                                    LineKind::Del => {
                                        painter.rect_filled(half, 0.0, t.diff_del_bg);
                                    }
                                    LineKind::Context => {}
                                }
                                if in_sel(side, Some(c.no)) {
                                    painter.rect_filled(half, 0.0, t.range_sel_bg);
                                }
                                let ncell = geo.num_cell(side, ry, row_h);
                                let on = sel.is_some_and(|x| x.side == side && x.range().0 <= c.no && c.no <= x.range().1);
                                painter.text(pos2(ncell.max.x - space::XS, ncell.center().y), Align2::RIGHT_CENTER, c.no.to_string(), theme::mono(text::MONO_SM), if on { t.fg } else { t.gutter_fg });
                                gutter_hit(ui, cx, view_id, &mut new_sel, &layout, top, m, &geo, side, c.no, ncell, i);
                                let tx = geo.text_x(side);
                                let clip = Rect::from_min_max(pos2(tx, half.min.y), pos2(half.max.x, half.max.y));
                                let spans = spans_for(cx, ui, c.src);
                                let line = m.line(c.src);
                                let sp = spans.as_ref().map_or(&[][..], |h| h.line(c.src.line as usize));
                                let job = line_job(&t, line, sp, c.cut, &font);
                                let galley = painter.layout_job(job);
                                painter.with_clip_rect(clip.intersect(vis)).galley(pos2(tx - hscroll, half.center().y - galley.size().y / 2.0), galley, t.fg);
                                if c.no_eol {
                                    painter.with_clip_rect(clip).text(pos2(clip.max.x - space::SM, half.center().y), Align2::RIGHT_CENTER, "no newline", theme::regular(text::TINY), t.muted);
                                }
                                let rr = ui.interact(Rect::from_min_max(pos2(tx, half.min.y), half.max), Id::new((view_id, "cell", i, side as u8)), Sense::hover());
                                rr.widget_info({
                                    let kind = match c.kind {
                                        LineKind::Add => "Added: ",
                                        LineKind::Del => "Removed: ",
                                        LineKind::Context => "",
                                    };
                                    let l = format!("{kind}{}", shown(line));
                                    move || WidgetInfo::labeled(WidgetType::Label, true, l.clone())
                                });
                            }
                        }
                    }
                    let mid = geo.half(Side::New).0;
                    painter.vline(mid, rrect.y_range(), Stroke::new(size::HAIRLINE, t.surface0));
                    let (lo, ln) = (l.map(|c| c.no), r.map(|c| c.no));
                    marks_and_badges(ui, &painter, &t, &geo, Side::Old, ry, row_h, lo, None, &marks_by, None, overlay, key, &mut events);
                    marks_and_badges(ui, &painter, &t, &geo, Side::New, ry, row_h, None, ln, &marks_by, None, overlay, key, &mut events);
                    if let Some(list) = closed_at.get(&i) {
                        for k in list {
                            if let Some(th) = overlay.threads.get(*k) {
                                badge(ui, &painter, &t, &geo, th.anchor.side, ry, row_h, th.count, Id::new((view_id, "thread", &th.key)), &th.key, key, &mut events);
                            }
                        }
                    }
                }
            }
            // Pairs drew their badges above; a line or a banner (a thread on a line the diff does not show) draws here.
            if !matches!(row, Row::Pair(..))
                && let Some(list) = closed_at.get(&i)
            {
                for k in list {
                    if let Some(th) = overlay.threads.get(*k) {
                        badge(ui, &painter, &t, &geo, Side::New, ry, row_h, th.count, Id::new((view_id, "thread", &th.key)), &th.key, key, &mut events);
                    }
                }
            }
        }

        // Open threads: a block under the row of their last line.
        for (k, th_idx) in open.iter().enumerate() {
            let Some(th) = overlay.threads.get(*th_idx) else { continue };
            let Some(&(_, bh)) = layout.inserts().get(k) else { continue };
            let y = ytop(layout.insert_top(k));
            if y + bh < vis.min.y || y > vis.max.y {
                continue;
            }
            let block = Rect::from_min_size(pos2(content.min.x, y), vec2(w, bh));
            painter.rect_filled(block, 0.0, t.mantle);
            painter.hline(block.x_range(), block.min.y, Stroke::new(size::HAIRLINE, t.surface0));
            painter.hline(block.x_range(), block.max.y - 0.5, Stroke::new(size::HAIRLINE, t.surface0));
            let pad = geo.gutter_w().min(w / 3.0);
            painter.rect_filled(Rect::from_min_size(pos2(content.min.x + pad - space::XS, block.min.y), vec2(size::THREAD_BAR, bh)), 0.0, t.thread_bar);
            let inner_rect = Rect::from_min_size(pos2(content.min.x + pad, y + space::SM), vec2((w - pad - space::SM).max(40.0), THREAD_MAX_H));
            let mut child = ui.new_child(UiBuilder::new().max_rect(inner_rect).layout(egui::Layout::top_down(Align::Min)));
            child.set_clip_rect(Rect::from_min_max(block.min, pos2(block.max.x, block.max.y)).intersect(ui.clip_rect()));
            tree::node(&mut child, cx, &th.child);
            let used = child.min_rect().height() + space::SM * 2.0;
            measured.push((th.key.clone(), inner_rect.width(), used.clamp(row_h, THREAD_MAX_H)));
        }

        // The range-action bar, under the selected range's last line.
        if let (Some(s), false) = (new_sel, overlay.range_actions.is_empty()) {
            let (_, b) = s.range();
            if let Some(re) = m.row_of(s.side, b)
                && !s.dragging
            {
                let by = ytop(layout.row_top(re) + row_h) + 2.0;
                let widths: Vec<f32> = overlay.range_actions.iter().map(|a| widgets::button_width(ui, Some(a.icon.as_str()).filter(|n| !n.is_empty() && crate::icons::exists(n)), &a.label, None)).collect();
                let total = widths.iter().sum::<f32>() + space::XS * (widths.len().saturating_sub(1)) as f32 + space::XS * 2.0;
                let bar = Rect::from_min_size(pos2((content.max.x - total - space::LG).max(content.min.x), by), vec2(total, size::CONTROL + space::XS * 2.0));
                if bar.max.y >= vis.min.y && bar.min.y <= vis.max.y {
                    painter.rect(bar, CornerRadius::same(radius::MD), t.pop, Stroke::new(size::HAIRLINE, t.surface1), egui::StrokeKind::Inside);
                    let mut x = bar.min.x + space::XS;
                    for (a, aw) in overlay.range_actions.iter().zip(&widths) {
                        let r = Rect::from_min_size(pos2(x, bar.min.y + space::XS), vec2(*aw, size::CONTROL));
                        x += aw + space::XS;
                        let icon_name = Some(a.icon.as_str()).filter(|n| !n.is_empty() && crate::icons::exists(n));
                        let resp = ui.scope_builder(UiBuilder::new().max_rect(r), |ui| widgets::button(ui, ButtonKind::Primary, icon_name, &a.label, None)).inner;
                        if resp.clicked() {
                            let (a0, b0) = s.range();
                            events.push((EventKind::RangeAction, json!({"id": a.id, "side": s.side.name(), "start": a0, "end": b0})));
                            new_sel = None;
                        }
                    }
                }
            }
        }

        if let Some(n) = scroll_target {
            let row = m.row_of(Side::New, n).or_else(|| m.row_of(Side::Old, n));
            if let Some(r) = row {
                ui.scroll_to_rect(Rect::from_min_size(pos2(content.min.x, ytop(layout.row_top(r))), vec2(1.0, row_h)), Some(Align::Center));
                scroll_to = Some(Rect::NOTHING);
            }
        }
        (content, vis, body)
    });
    let (content, vis, _body) = area.inner;
    let _ = content;
    if scroll_to.is_some() {
        ui.ctx().request_repaint();
    }
    if scroll_target.is_some() {
        cx.st.view.scrolled.insert(vkey.clone(), scroll_target.unwrap_or(0));
    }
    let mut changed = false;
    for (tk, w, h) in measured {
        let k = (instance.clone(), key.to_owned(), tk);
        let old = cx.st.view.thread_h.get(&k).copied();
        if old.is_none_or(|(ow, oh)| (oh - h).abs() > 0.5 || (ow - w).abs() > 0.5) {
            cx.st.view.thread_h.insert(k, (w, h));
            changed = true;
        }
    }
    if changed {
        ui.ctx().request_repaint();
    }

    // Horizontal scroll: the wheel or a trackpad over the view, and a thin bar when the text is wider.
    let hover = ui.rect_contains_pointer(outer);
    if hover && max_h > 0.0 {
        let dx = ui.input(|i| i.smooth_scroll_delta.x);
        if dx != 0.0 {
            hscroll = (hscroll - dx).clamp(0.0, max_h);
        }
    }
    if max_h > 0.0 {
        let track = Rect::from_min_max(pos2(outer.min.x + geo.gutter_w(), outer.max.y - size::HBAR_H - size::HBAR_GAP), pos2(outer.max.x - size::HBAR_SIDE, outer.max.y - size::HBAR_GAP));
        let ratio = (min_half_text / text_w_all).clamp(0.05, 1.0);
        let thumb_w = (track.width() * ratio).max(size::HBAR_MIN);
        let thumb_x = track.min.x + (track.width() - thumb_w) * (hscroll / max_h);
        let thumb = Rect::from_min_size(pos2(thumb_x, track.min.y), vec2(thumb_w, track.height()));
        let resp = ui.interact(track, Id::new((view_id, "hbar")), Sense::drag());
        if resp.dragged() && track.width() > thumb_w {
            hscroll = (hscroll + resp.drag_delta().x * max_h / (track.width() - thumb_w)).clamp(0.0, max_h);
        }
        inner.painter().rect_filled(thumb, CornerRadius::same(radius::SM), if resp.hovered() || resp.dragged() { t.surface2 } else { t.surface1 });
        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Slider, true, "Scroll sideways"));
    }
    cx.st.view.hscroll.insert(vkey.clone(), hscroll);
    match new_sel {
        Some(s) => {
            cx.st.view.sels.insert(vkey, s);
        }
        None => {
            cx.st.view.sels.remove(&vkey);
        }
    }
    for (k, v) in events {
        cx.event(key, k, v);
    }
    let _ = vis;
}

/// Marks (dots and counts) for a row, and nothing else: closed-thread badges are drawn by the caller.
#[allow(clippy::too_many_arguments)]
fn marks_and_badges(ui: &Ui, painter: &egui::Painter, t: &Tokens, geo: &Geo, side: Side, top: f32, h: f32, old: Option<u32>, new: Option<u32>, marks_by: &HashMap<(Side, u32), Vec<&Mark>>, closed: Option<&Vec<usize>>, overlay: &Overlay, _key: &str, _events: &mut Vec<(EventKind, serde_json::Value)>) {
    let _ = (ui, overlay);
    if closed.is_some_and(|c| !c.is_empty()) {
        return;
    }
    let cell = geo.mark_cell(side, top, h);
    let mut x = cell.min.x + space::XS + size::MARK_DOT;
    for (s, no) in [(Side::Old, old), (Side::New, new)] {
        let Some(no) = no else { continue };
        if geo.gutter != Gutter::Double && s != side {
            continue;
        }
        for mk in marks_by.get(&(s, no)).map(Vec::as_slice).unwrap_or(&[]) {
            let c = if mk.tone == Tone::Default { t.blue } else { tone_color(t, mk.tone) };
            match mk.count {
                Some(n) => {
                    let label = n.to_string();
                    painter.rect_filled(Rect::from_center_size(pos2(x + space::XS, cell.center().y), vec2(size::MARK_COUNT, size::MARK_COUNT)), CornerRadius::same(radius::PILL), Tokens::tint(c, alpha::MARK));
                    painter.text(pos2(x + space::XS, cell.center().y), Align2::CENTER_CENTER, label, theme::medium(text::TINY), c);
                }
                None => {
                    painter.circle_filled(pos2(x, cell.center().y), size::MARK_DOT, c);
                }
            }
            x += size::MARK_STEP;
            if x > cell.max.x - space::XS {
                break;
            }
        }
    }
}

/// A closed thread's badge in the gutter: its comment count; a click asks the plugin to open it.
#[allow(clippy::too_many_arguments)]
fn badge(ui: &mut Ui, painter: &egui::Painter, t: &Tokens, geo: &Geo, side: Side, top: f32, h: f32, count: u32, id: Id, thread_key: &str, _node_key: &str, events: &mut Vec<(EventKind, serde_json::Value)>) {
    let cell = geo.mark_cell(side, top, h);
    let r = Rect::from_center_size(cell.center(), vec2(size::THREAD_BADGE_W, size::THREAD_BADGE_H));
    let resp = ui.interact(r, id, Sense::click());
    let c = t.thread_bar;
    painter.rect(r, CornerRadius::same(radius::PILL), Tokens::tint(c, if resp.hovered() { alpha::THREAD_BADGE_HOVER } else { alpha::THREAD_BADGE }), Stroke::new(size::HAIRLINE, c), egui::StrokeKind::Inside);
    painter.text(r.center(), Align2::CENTER_CENTER, count.to_string(), theme::medium(text::TINY), t.fg);
    resp.widget_info({
        let l = format!("Open thread ({count})");
        move || WidgetInfo::labeled(WidgetType::Button, true, l.clone())
    });
    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        events.push((EventKind::ThreadToggle, json!({"key": thread_key})));
    }
}

/// The click, shift-click and drag of one gutter number: selects a line or extends the range.
#[allow(clippy::too_many_arguments)]
fn gutter_hit(ui: &mut Ui, cx: &mut Cx<'_>, view_id: Id, sel: &mut Option<Sel>, layout: &RowLayout, top: f32, m: &Model, geo: &Geo, side: Side, no: u32, cell: Rect, row: usize) {
    let _ = (cx, row);
    let id = Id::new((view_id, "gutter", side as u8, no));
    let resp = ui.interact(cell, id, Sense::click_and_drag());
    let label = match (geo.gutter, side) {
        (Gutter::Single, _) => format!("Line {no}"),
        (_, Side::Old) => format!("Old line {no}"),
        (_, Side::New) => format!("New line {no}"),
    };
    resp.widget_info({
        let l = label.clone();
        move || WidgetInfo::labeled(WidgetType::Button, true, l.clone())
    });
    let shift = ui.input(|i| i.modifiers.shift);
    if resp.clicked() || resp.drag_started() {
        resp.request_focus();
        ui.memory_mut(|mem| mem.request_focus(view_id));
        *sel = Some(match (*sel, shift) {
            (Some(s), true) if s.side == side => Sel { head: no, dragging: false, ..s },
            _ => Sel { side, anchor: no, head: no, dragging: false },
        });
        if resp.drag_started()
            && let Some(s) = sel.as_mut()
        {
            s.dragging = true;
        }
    }
    if resp.dragged()
        && let (Some(s), Some(p)) = (sel.as_mut(), ui.input(|i| i.pointer.latest_pos()))
        && s.side == side
        && let Some(r) = layout.row_at(p.y - top)
        && let Some(n) = m.row(r).and_then(|row| row.no(side))
    {
        s.head = n;
    }
    if resp.drag_stopped()
        && let Some(s) = sel.as_mut()
    {
        s.dragging = false;
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
}

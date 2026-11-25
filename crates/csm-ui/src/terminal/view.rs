//! The terminal widget: measures the grid from its rect every frame, turns egui input into PTY
//! bytes, and paints a cached mesh that is rebuilt only when something changed.
//!
//! Moved from the Phase 0 spike (`apps/term-spike/src/view.rs`). One [`TerminalView`] per place a
//! terminal is shown (its widget state: selection drag, wheel remainder, IME preedit, the mesh);
//! one [`TerminalRenderer`] for all of them (fonts and the glyph atlas).

use std::sync::Arc;
use std::time::Instant;

use csm_term::alacritty_terminal::grid::Dimensions;
use csm_term::alacritty_terminal::index::{Column, Line, Point, Side};
use csm_term::alacritty_terminal::selection::{Selection, SelectionType};
use csm_term::alacritty_terminal::term::TermMode;
use csm_term::input::{self, Key, Mods, MouseAction, MouseButton};
use csm_term::links::Link;
use csm_term::{GridSize, Terminal};
use egui::{Color32, Event, Id, ImeEvent, Mesh, Pos2, Rect, Response, Sense, Shape, Ui, Vec2, pos2, vec2};

use super::atlas::GlyphAtlas;
use super::fonts::FontSet;
use super::render::{self, TermStyle};
use super::source::{SourceState, TerminalSource};

#[derive(Default, Debug, Clone, Copy)]
pub struct FrameCounters {
    pub views: u32,
    pub rebuilds: u32,
    pub lock_misses: u32,
    /// Time spent building meshes (ms).
    pub build_ms: f32,
}

/// Render resources shared by every terminal in every window: fonts and the glyph atlas.
pub struct TerminalRenderer {
    pub fonts: FontSet,
    pub atlas: Option<GlyphAtlas>,
    pub frame: FrameCounters,
}

impl TerminalRenderer {
    pub fn new(fonts: FontSet) -> Self {
        Self { fonts, atlas: None, frame: FrameCounters::default() }
    }
}

/// How a view behaves this frame.
#[derive(Debug, Clone)]
pub struct ViewOptions {
    /// Interactive: measures the grid and reports it to the source (the process gets exactly
    /// that size), takes focus on click and sends input. Not interactive: a preview (Overview
    /// mini-terminals) that draws the terminal at its current grid, scaled down to fit, and
    /// never resizes it.
    pub interactive: bool,
    /// Take keyboard focus this frame (the pane became the active one).
    pub request_focus: bool,
    pub style: TermStyle,
    /// Inner padding between the rect and the cells, in points.
    pub padding: Vec2,
    /// A session's own padding (its look), in points: top, right, bottom, left. Wins over
    /// [`ViewOptions::padding`].
    pub edges: Option<[f32; 4]>,
    /// A session's own font size (its look), in points; `None` is the renderer's size. Drawn from
    /// the shared atlas at that pixel size (glyphs are keyed by size): no second font is loaded.
    pub font_pt: Option<f32>,
    /// The pane runs Claude Code: Ctrl+Shift+C with nothing selected asks for Claude's last reply
    /// ([`ViewResponse::copy_reply`]; the tool answers "not available" for WSL and SSH).
    pub claude: bool,
    /// Copies of a selection are cleaned up (`claude_copy::clean`): a Claude Code pane with
    /// `CleanClaudeCopies` on. Alt while copying gives the raw text.
    pub clean_copy: bool,
    /// `ShiftEnterNewline`: Shift+Enter sends a newline key (`input::encode_key_with`).
    pub shift_enter_newline: bool,
    /// The pane belongs to a remote window: a pasted image is not passed on (the office's clipboard
    /// is not the laptop's; [`ViewResponse::image_paste`] says so instead).
    pub remote: bool,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            interactive: true,
            request_focus: false,
            style: TermStyle::default(),
            padding: vec2(8.0, 6.0),
            edges: None,
            font_pt: None,
            claude: false,
            clean_copy: false,
            shift_enter_newline: true,
            remote: false,
        }
    }
}

/// The cell area inside `rect`: the session's edges when it has them, else the symmetric padding.
/// Never inverted: a padding larger than the rect leaves an empty area at its centre.
fn inset(rect: Rect, opts: &ViewOptions) -> Rect {
    let Some([t, r, b, l]) = opts.edges else { return rect.shrink2(opts.padding) };
    let clean = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
    let min = pos2(rect.min.x + clean(l), rect.min.y + clean(t));
    let max = pos2((rect.max.x - clean(r)).max(min.x), (rect.max.y - clean(b)).max(min.y));
    if min.x > rect.max.x || min.y > rect.max.y { Rect::from_center_size(rect.center(), Vec2::ZERO) } else { Rect::from_min_max(min, max) }
}

/// The smallest scale a preview is drawn at (Overview mini-terminals).
const PREVIEW_MIN_ZOOM: f32 = 0.66;

/// UI → terminal changes, applied when the view next gets the emulator's lock.
#[derive(Debug, Clone)]
enum Op {
    Scroll(i32),
    ScrollBottom,
    SelStart { col: usize, row: usize, right_half: bool, clicks: u8 },
    SelUpdate { col: usize, row: usize, right_half: bool },
    SelClear,
    /// Copy the selection; `raw`: as it is, never cleaned (Alt held).
    Copy { raw: bool },
}

struct RenderCache {
    mesh: Arc<Mesh>,
    key: CacheKey,
    cursor_rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CacheKey {
    generation: u64,
    origin: Pos2,
    ppp: f32,
    focused: bool,
    epoch: u64,
    px: f32,
    grid: (u16, u16),
    style: u64,
}

/// What the view did this frame.
#[derive(Debug, Clone)]
pub struct ViewResponse {
    pub response: Response,
    /// The grid it measured (interactive) or drew (preview).
    pub grid: Option<GridSize>,
    pub focused: bool,
    /// The pointer went down on it (activate the pane).
    pub clicked: bool,
    /// A link was Ctrl+clicked (Cmd+click on macOS): the caller opens it (`open_url`).
    pub open_link: Option<String>,
    /// Ctrl+Shift+C with nothing selected in a Claude pane ([`ViewOptions::claude`]): the caller
    /// copies Claude's last reply (`claude_reply_get`).
    pub copy_reply: bool,
    /// Ctrl+V found an image and no text in a remote window's pane: nothing was sent (the office
    /// cannot read this laptop's clipboard). The caller uploads the image instead.
    pub image_paste: bool,
    /// A paste the view did not send itself: it needs the person's yes first (several lines into a
    /// program that did not ask for bracketed paste), was cut to [`PASTE_MAX_BYTES`], or is large
    /// enough that cleaning it belongs off the UI thread. The caller asks or hands it to
    /// [`TerminalSource::paste`] on a worker.
    pub paste: Option<PasteRequest>,
}

/// The most text one paste carries (8 MiB). More is cut, and the person is told.
pub const PASTE_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Pastes up to this size are cleaned and sent from the frame; bigger ones go to a worker.
const PASTE_INLINE_BYTES: usize = 64 * 1024;

/// A paste the view hands up instead of sending ([`ViewResponse::paste`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteRequest {
    /// Already cut to [`PASTE_MAX_BYTES`].
    pub text: String,
    /// More than one line, and the program did not ask for bracketed paste: in a shell each line
    /// would run at once. Ask first.
    pub confirm: bool,
    /// The clipboard held more than [`PASTE_MAX_BYTES`].
    pub truncated: bool,
}

/// Decides what a clipboard paste becomes: `Ok(text)` is safe to send from the frame now,
/// `Err(request)` goes up to the app.
pub fn plan_paste(mut text: String, mode: TermMode) -> Result<String, PasteRequest> {
    let truncated = text.len() > PASTE_MAX_BYTES;
    if truncated {
        let mut at = PASTE_MAX_BYTES;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
    }
    let confirm = !mode.contains(TermMode::BRACKETED_PASTE) && text.trim_end_matches(['\r', '\n']).contains(['\r', '\n']);
    if confirm || truncated || text.len() > PASTE_INLINE_BYTES { Err(PasteRequest { text, confirm, truncated }) } else { Ok(text) }
}

/// The link under a clicked cell, read from the terminal now (not the hover cache of an earlier
/// frame). `None` when there is none, or the emulator is busy (a click is not worth waiting for).
fn link_at_click(source: &dyn TerminalSource, col: usize, row: usize) -> Option<String> {
    let term = source.terminal()?;
    let t = match term.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    let link = t.link_at(col, row)?;
    link.spans.iter().any(|&(r, a, b)| r == row && (a..=b).contains(&col)).then_some(link.uri)
}

/// The link under the pointer, looked up again only when the cell or the screen changes.
struct Hover {
    /// (terminal generation, column, row).
    key: (u64, usize, usize),
    link: Option<Link>,
}

impl Hover {
    fn link_at(&self, col: usize, row: usize) -> Option<&Link> {
        self.link.as_ref().filter(|l| l.spans.iter().any(|&(r, a, b)| r == row && (a..=b).contains(&col)))
    }
}

/// The cell under `pos`: (column, row, in its right half), clamped to the grid.
#[allow(clippy::too_many_arguments)]
fn cell_of(pos: Pos2, origin: Pos2, ppp: f32, cell_w: f32, cell_h: f32, cols: u16, rows: u16) -> (usize, usize, bool) {
    let x = ((pos.x - origin.x) * ppp / cell_w).max(0.0);
    let y = ((pos.y - origin.y) * ppp / cell_h).max(0.0);
    let col = (x.floor() as usize).min(usize::from(cols).saturating_sub(1));
    let row = (y.floor() as usize).min(usize::from(rows).saturating_sub(1));
    (col, row, x.fract() >= 0.5)
}

/// Widget state for one place a terminal is shown.
pub struct TerminalView {
    id: Id,
    cache: Option<RenderCache>,
    ops: Vec<Op>,
    was_focused: bool,
    has_selection: bool,
    mouse_down: Option<MouseButton>,
    /// Whether the press that began the current drag was reported to the program (as opposed to
    /// selecting locally). Latched at press: the release and the drag follow it even if Shift
    /// changed since.
    mouse_reported: bool,
    drag_origin: Option<(usize, usize)>,
    last_click: Option<(Instant, usize, usize, u8)>,
    wheel_accum: f32,
    preedit: String,
    grid: Option<GridSize>,
    /// Ctrl+V bookkeeping: egui-winit drops the press when the clipboard holds no text (a
    /// screenshot), but the release still arrives. See [`PasteWatch`].
    paste: PasteWatch,
    /// The link under the pointer (interactive views only).
    hover: Option<Hover>,
    /// This frame's [`ViewOptions::claude`] and [`ViewOptions::clean_copy`].
    claude: bool,
    clean_copy: bool,
    shift_enter_newline: bool,
    /// This frame's [`ViewOptions::remote`], and whether an image paste was refused this frame.
    remote: bool,
    image_paste: bool,
    /// A paste to pass up this frame ([`ViewResponse::paste`]).
    paste_req: Option<PasteRequest>,
    /// Ctrl+Shift+C asked for Claude's last reply this frame.
    copy_reply: bool,
}

fn mods(m: egui::Modifiers) -> Mods {
    Mods { shift: m.shift, alt: m.alt, ctrl: m.ctrl }
}

/// egui key → terminal key. Plain text keys return `None`: their text arrives as `Event::Text`.
fn map_key(key: egui::Key, m: egui::Modifiers) -> Option<Key> {
    use egui::Key as K;
    Some(match key {
        K::Enter => Key::Enter,
        K::Tab => Key::Tab,
        K::Backspace => Key::Backspace,
        K::Escape => Key::Escape,
        K::ArrowUp => Key::Up,
        K::ArrowDown => Key::Down,
        K::ArrowLeft => Key::Left,
        K::ArrowRight => Key::Right,
        K::Home => Key::Home,
        K::End => Key::End,
        K::PageUp => Key::PageUp,
        K::PageDown => Key::PageDown,
        K::Insert => Key::Insert,
        K::Delete => Key::Delete,
        K::F1 => Key::F(1),
        K::F2 => Key::F(2),
        K::F3 => Key::F(3),
        K::F4 => Key::F(4),
        K::F5 => Key::F(5),
        K::F6 => Key::F(6),
        K::F7 => Key::F(7),
        K::F8 => Key::F(8),
        K::F9 => Key::F(9),
        K::F10 => Key::F(10),
        K::F11 => Key::F(11),
        K::F12 => Key::F(12),
        _ => {
            // Ctrl+x / Alt+x only. Ctrl+Alt is AltGr on Windows: that one types text.
            if m.ctrl == m.alt {
                return None;
            }
            let name = key.name();
            let c = match key {
                K::Space => ' ',
                K::OpenBracket => '[',
                K::CloseBracket => ']',
                K::Backslash => '\\',
                K::Minus => '-',
                K::Slash => '/',
                K::Period => '.',
                K::Comma => ',',
                K::Semicolon => ';',
                K::Equals => '=',
                _ if name.len() == 1 => {
                    let c = name.chars().next()?.to_ascii_lowercase();
                    if m.shift && m.alt { c.to_ascii_uppercase() } else { c }
                }
                _ => return None,
            };
            Key::Char(c)
        }
    })
}

fn style_key(style: &TermStyle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let p = &style.palette;
    for c in p.ansi.iter().chain([&p.foreground, &p.background, &p.cursor]) {
        (c.r, c.g, c.b).hash(&mut h);
    }
    style.selection.to_array().hash(&mut h);
    h.finish()
}

impl TerminalView {
    /// `id` must be unique among the views shown in one frame.
    pub fn new(id: Id) -> Self {
        Self {
            id,
            cache: None,
            ops: Vec::new(),
            was_focused: false,
            has_selection: false,
            mouse_down: None,
            mouse_reported: false,
            drag_origin: None,
            last_click: None,
            wheel_accum: 0.0,
            preedit: String::new(),
            grid: None,
            paste: PasteWatch::default(),
            hover: None,
            claude: false,
            clean_copy: false,
            shift_enter_newline: true,
            remote: false,
            image_paste: false,
            paste_req: None,
            copy_reply: false,
        }
    }

    /// The egui id the view takes focus under (for tests and focus requests).
    pub fn id(&self) -> Id {
        self.id
    }

    /// The address of the link under the pointer, if any.
    pub fn hovered_link(&self) -> Option<&str> {
        self.hover.as_ref().and_then(|h| h.link.as_ref()).map(|l| l.uri.as_str())
    }

    /// The last grid this view measured or drew.
    pub fn grid(&self) -> Option<GridSize> {
        self.grid
    }

    /// Draws `source` into `rect`.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        source: &dyn TerminalSource,
        renderer: &mut TerminalRenderer,
        rect: Rect,
        opts: &ViewOptions,
    ) -> ViewResponse {
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let inner = inset(rect, opts);
        // The session's font size as a scale on the renderer's: metrics and glyphs at that size.
        let scale = opts.font_pt.filter(|p| p.is_finite() && *p > 0.0).map_or(1.0, |p| (p / renderer.fonts.size_pt.max(1.0)).clamp(0.25, 4.0));
        // The UI zoom (`UiZoomPercent`) scales points; the terminal keeps its own font size, so
        // its cells are measured at the native scale.
        let ui_zoom = ctx.zoom_factor();
        let scale = if ui_zoom.is_finite() && ui_zoom > 0.0 { scale / ui_zoom } else { scale };
        let sense = if opts.interactive { Sense::click_and_drag() } else { Sense::click() };
        let resp = ui.interact(rect, self.id, sense);
        let clicked = resp.is_pointer_button_down_on() || resp.clicked();

        let term = source.terminal();
        // Interactive: the grid follows the rect. Preview: the grid stays; the font shrinks.
        let (grid, zoom) = if opts.interactive {
            let m = renderer.fonts.metrics(ppp * scale);
            let cols = ((inner.width() * ppp) / m.cell_w).floor().clamp(2.0, 1000.0) as u16;
            let rows = ((inner.height() * ppp) / m.cell_h).floor().clamp(1.0, 500.0) as u16;
            let grid = GridSize { cols, rows, cell_width: m.cell_w as u16, cell_height: m.cell_h as u16 };
            // Before layout settles a rect can be degenerate; never report that as a size.
            if inner.width() > 8.0 && inner.height() > 8.0 {
                source.set_grid(grid);
            }
            (Some(grid), 1.0)
        } else {
            let size = term.as_ref().and_then(|t| t.try_lock().ok().map(|t| t.size()));
            let m = renderer.fonts.metrics(ppp * scale);
            // Previews are small but readable (the mock's 8.5 px), showing the bottom of the
            // screen where the prompt is; what does not fit is clipped at the top.
            let zoom = size.map_or(1.0, |g| (inner.width() / (f32::from(g.cols) * m.cell_w / ppp)).min(1.0));
            (size, zoom.clamp(PREVIEW_MIN_ZOOM, 1.0))
        };
        self.grid = grid.or(self.grid);
        // Metrics at the zoomed scale: cells in physical pixels of a smaller font.
        let m = renderer.fonts.metrics(ppp * zoom * scale);
        let mut origin = pos2((inner.min.x * ppp).round() / ppp, (inner.min.y * ppp).round() / ppp);
        if !opts.interactive
            && let Some(g) = grid
        {
            let h = f32::from(g.rows) * m.cell_h / ppp;
            if h > inner.height() {
                origin.y = ((inner.max.y - h) * ppp).round() / ppp;
            }
        }

        if opts.interactive && (opts.request_focus || resp.is_pointer_button_down_on()) && !resp.has_focus() {
            resp.request_focus();
        }
        let focused = opts.interactive && resp.has_focus();
        if focused {
            ui.memory_mut(|mem| {
                mem.set_focus_lock_filter(self.id, egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true });
            });
        }
        let mode = source.mode();
        if opts.interactive && focused != self.was_focused {
            self.was_focused = focused;
            source.focus_changed(focused);
        }
        let mut open_link = None;
        self.claude = opts.claude;
        self.clean_copy = opts.clean_copy;
        self.shift_enter_newline = opts.shift_enter_newline;
        self.copy_reply = false;
        self.remote = opts.remote;
        self.image_paste = false;
        self.paste_req = None;
        if opts.interactive
            && let Some(g) = grid
        {
            open_link = self.handle_input(ui, source, &resp, focused, mode, origin, ppp, m.cell_w, m.cell_h, g.cols, g.rows);
        }
        // The cell under the pointer, for links (interactive views, pointer on the cells).
        let pointer_cell = match grid {
            Some(g) if opts.interactive && resp.contains_pointer() => ui
                .input(|i| i.pointer.hover_pos())
                .filter(|p| inner.contains(*p))
                .map(|p| cell_of(p, origin, ppp, m.cell_w, m.cell_h, g.cols, g.rows)),
            _ => None,
        };
        if pointer_cell.is_none() {
            self.hover = None;
        }

        // Build or reuse the mesh.
        let now = Instant::now();
        if let Some(term) = term {
            let guard = match term.try_lock() {
                Ok(g) => Some(g),
                Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            };
            match guard {
                Some(mut t) => {
                    self.apply_ops(&mut t, &ctx);
                    t.flush_sync_if_due(now);
                    // One logical line, and only when the cell or the screen changed.
                    if let Some((col, row, _)) = pointer_cell {
                        let key = (t.generation(), col, row);
                        if self.hover.as_ref().map(|h| h.key) != Some(key) {
                            self.hover = Some(Hover { key, link: t.link_at(col, row) });
                        }
                    }
                    if let Some(deadline) = t.sync_deadline() {
                        ctx.request_repaint_after(deadline.saturating_duration_since(now));
                    }
                    let size = t.size();
                    let atlas = renderer.atlas.get_or_insert_with(|| GlyphAtlas::new(&ctx));
                    let key = CacheKey {
                        generation: t.generation(),
                        origin,
                        ppp,
                        focused,
                        epoch: atlas.epoch,
                        px: m.px,
                        grid: (size.cols, size.rows),
                        style: style_key(&opts.style),
                    };
                    if self.cache.as_ref().map(|c| c.key) != Some(key) {
                        let tb = Instant::now();
                        let mut key = key;
                        let mut built = render::build(&t, &mut renderer.fonts, atlas, origin, ppp, m, focused, &opts.style);
                        renderer.frame.rebuilds += 1;
                        if atlas.full {
                            // The atlas ran out of room mid-build: evict everything and build
                            // again on the empty atlas, in this same frame. If it overflows even
                            // then (more glyphs on this pane than one atlas holds) keep what fits
                            // and do not clear again: clearing on every frame would re-rasterise
                            // the lot at full frame rate. Other panes' cached meshes point into
                            // the evicted glyphs; the epoch change rebuilds them, and the repaint
                            // below shows that (throttled when overflows repeat).
                            atlas.clear();
                            built = render::build(&t, &mut renderer.fonts, atlas, origin, ppp, m, focused, &opts.style);
                            renderer.frame.rebuilds += 1;
                            atlas.full = false;
                            key.epoch = atlas.epoch;
                            ctx.request_repaint_after(atlas.overflow_repaint_delay(now));
                        }
                        renderer.frame.build_ms += tb.elapsed().as_secs_f32() * 1000.0;
                        self.cache = Some(RenderCache { mesh: Arc::new(built.mesh), key, cursor_rect: built.cursor_rect });
                    }
                }
                None => {
                    renderer.frame.lock_misses += 1;
                    ctx.request_repaint();
                }
            }
        }
        renderer.frame.views += 1;

        // Paint.
        let painter = ui.painter_at(rect);
        let bg = opts.style.palette.background;
        painter.rect_filled(rect, 0.0, Color32::from_rgb(bg.r, bg.g, bg.b));
        if let Some(cache) = &self.cache {
            painter.add(Shape::Mesh(cache.mesh.clone()));
        }
        // The link under the pointer: underlined, its address in a tooltip, a hand with Ctrl.
        if let Some((col, row, _)) = pointer_cell
            && let Some(link) = self.hover.as_ref().and_then(|h| h.link_at(col, row))
        {
            let fg = opts.style.palette.foreground;
            let stroke = egui::Stroke::new(crate::theme::size::HAIRLINE, Color32::from_rgb(fg.r, fg.g, fg.b));
            let (cw, ch) = (m.cell_w / ppp, m.cell_h / ppp);
            for &(r, a, b) in &link.spans {
                let y = origin.y + (r as f32 + 1.0) * ch - crate::theme::size::HAIRLINE;
                painter.hline((origin.x + a as f32 * cw)..=(origin.x + (b as f32 + 1.0) * cw), y, stroke);
            }
            let tip = if link_opens(&link.uri) {
                format!("{}\n{} to open", link.uri, if cfg!(target_os = "macos") { "Cmd+click" } else { "Ctrl+click" })
            } else {
                format!("{}\nOnly http and https links open from here", link.uri)
            };
            let _ = resp.clone().on_hover_text_at_pointer(tip);
            if ctx.input(|i| i.modifiers.command) {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        if focused && let Some(cache) = &self.cache {
            let cursor_rect = cache.cursor_rect;
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput { purpose: egui::IMEPurpose::Terminal, rect, cursor_rect, should_interrupt_composition: false });
            });
            if !self.preedit.is_empty() {
                let fg = opts.style.palette.foreground;
                let fg = Color32::from_rgb(fg.r, fg.g, fg.b);
                let font = egui::FontId::monospace(renderer.fonts.size_pt * scale);
                let galley = painter.layout_no_wrap(self.preedit.clone(), font, fg);
                let r = Rect::from_min_size(cursor_rect.min, galley.size());
                painter.rect_filled(r.expand(1.0), 0.0, Color32::from_rgb(bg.r, bg.g, bg.b));
                painter.galley(r.min, galley, fg);
                painter.hline(r.x_range(), r.max.y, egui::Stroke::new(1.0, fg));
            }
        }
        if opts.interactive && (opts.request_focus || resp.is_pointer_button_down_on()) && !resp.has_focus() {
            resp.request_focus();
        }
        if false {
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "terminal"));
        }
        ViewResponse { response: resp, grid, focused, clicked, open_link, copy_reply: std::mem::take(&mut self.copy_reply), image_paste: std::mem::take(&mut self.image_paste), paste: self.paste_req.take() }
    }

    /// The state text to draw over an empty view (`None` while running).
    pub fn placeholder(source: &dyn TerminalSource) -> Option<String> {
        match source.state() {
            SourceState::Starting => Some("Starting…".into()),
            SourceState::Running | SourceState::Stale => None,
            SourceState::Failed(e) => Some(format!("Could not start: {e}")),
            SourceState::Exited(code) => Some(match code {
                Some(c) => format!("The process exited with code {c}."),
                None => "The process exited.".into(),
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_input(
        &mut self,
        ui: &Ui,
        source: &dyn TerminalSource,
        resp: &Response,
        focused: bool,
        mode: TermMode,
        origin: Pos2,
        ppp: f32,
        cell_w: f32,
        cell_h: f32,
        cols: u16,
        rows: u16,
    ) -> Option<String> {
        let events = ui.input(|i| i.events.clone());
        let body = resp.rect;
        let hovered = resp.hovered() || resp.contains_pointer();
        let to_cell = |pos: Pos2| cell_of(pos, origin, ppp, cell_w, cell_h, cols, rows);
        let mut open_link = None;
        let mut typed = false;
        if focused && self.paste.step(&events, ui.input(|i| i.modifiers)) {
            // A paste of something that isn't text (an image): pass the raw ^V through, as a
            // terminal does, so the program (Claude Code) reads the clipboard itself. A remote
            // pane cannot: the office would read its own clipboard, so nothing is sent.
            if self.remote {
                self.image_paste = true;
            } else {
                source.send_key(&[0x16]);
                typed = true;
            }
        }
        for ev in events {
            match ev {
                Event::Text(text) if focused => {
                    let alt_only = ui.input(|i| i.modifiers.alt && !i.modifiers.ctrl);
                    if !alt_only {
                        source.send_key(&input::typed(&text, false).0);
                        typed = true;
                    }
                }
                Event::Key { key, pressed: true, modifiers, .. } if focused => {
                    if let Some(k) = map_key(key, modifiers)
                        && let Some(bytes) = input::encode_key_with(k, mods(modifiers), mode, self.shift_enter_newline)
                    {
                        source.send_key(&bytes.0);
                        typed = true;
                    }
                }
                Event::Copy if focused => {
                    // egui sends no modifiers with a copy: read them now. Ctrl+C and
                    // Ctrl+Shift+C both arrive here.
                    let m = ui.input(|i| i.modifiers);
                    if self.has_selection {
                        self.ops.push(Op::Copy { raw: m.alt });
                    } else if self.claude && m.shift {
                        // Ctrl+Shift+C on nothing in a Claude pane: its last reply (Ctrl+C
                        // still interrupts).
                        self.copy_reply = true;
                    } else {
                        source.send_key(&[0x03]);
                        typed = true;
                    }
                }
                Event::Cut if focused => {
                    source.send_key(&[0x18]);
                    typed = true;
                }
                Event::Paste(text) if focused => {
                    // Clipboard text is untrusted: bracketed paste, never typed; several lines into
                    // a program without bracketed paste ask first; the size is capped.
                    match plan_paste(text, mode) {
                        Ok(text) => source.paste(&text),
                        Err(req) => self.paste_req = Some(req),
                    }
                    typed = true;
                }
                Event::Ime(ImeEvent::Preedit { text, .. }) if focused => self.preedit = text,
                Event::Ime(ImeEvent::Commit(text)) if focused => {
                    self.preedit.clear();
                    source.send_key(&input::typed(&text, false).0);
                    typed = true;
                }
                Event::PointerButton { pos, button, pressed, modifiers } => {
                    let b = match button {
                        egui::PointerButton::Primary => MouseButton::Left,
                        egui::PointerButton::Secondary => MouseButton::Right,
                        egui::PointerButton::Middle => MouseButton::Middle,
                        _ => continue,
                    };
                    let report = input::mouse_reporting(mode) && !modifiers.shift;
                    let (col, row, right_half) = to_cell(pos);
                    // Ctrl+click (Cmd+click) on a link opens it, even where the program reads
                    // the mouse; it starts no selection and reaches no program.
                    if pressed
                        && b == MouseButton::Left
                        && modifiers.command
                        && body.contains(pos)
                        && hovered
                        && let Some(uri) = link_at_click(source, col, row)
                    {
                        // Looked up now, under the terminal lock, from the clicked cell: the
                        // hover cache is the previous frame's and the screen may have moved.
                        open_link = Some(uri);
                        continue;
                    }
                    if pressed && body.contains(pos) && hovered {
                        if report {
                            if let Some(bytes) = input::encode_mouse(MouseAction::Press(b), col as u32, row as u32, mods(modifiers), mode) {
                                source.send_mouse(&bytes.0);
                            }
                            self.mouse_down = Some(b);
                            self.mouse_reported = true;
                        } else if b == MouseButton::Left {
                            let now = Instant::now();
                            let clicks = match self.last_click {
                                Some((t, c, r, n)) if now.duration_since(t).as_millis() < 400 && c == col && r == row => (n % 3) + 1,
                                _ => 1,
                            };
                            self.last_click = Some((now, col, row, clicks));
                            self.ops.push(Op::SelStart { col, row, right_half, clicks });
                            self.has_selection = clicks > 1;
                            self.mouse_down = Some(b);
                            self.mouse_reported = false;
                            self.drag_origin = Some((col, row));
                        }
                    } else if !pressed && self.mouse_down == Some(b) {
                        // The release goes where the press went, whatever Shift says now.
                        let report = self.mouse_reported && input::mouse_reporting(mode);
                        if report && let Some(bytes) = input::encode_mouse(MouseAction::Release(b), col as u32, row as u32, mods(modifiers), mode) {
                            source.send_mouse(&bytes.0);
                        }
                        if !self.mouse_reported && !self.has_selection {
                            self.ops.push(Op::SelClear);
                        }
                        self.mouse_down = None;
                        self.drag_origin = None;
                    }
                }
                Event::PointerMoved(pos) => {
                    let (col, row, right_half) = to_cell(pos);
                    let shift = ui.input(|i| i.modifiers.shift);
                    let report = input::mouse_reporting(mode) && !shift;
                    match self.mouse_down {
                        Some(b) if self.mouse_reported => {
                            if let Some(bytes) =
                                input::encode_mouse(MouseAction::Motion { held: Some(b) }, col as u32, row as u32, Mods::default(), mode)
                            {
                                source.send_mouse(&bytes.0);
                            }
                        }
                        Some(MouseButton::Left) => {
                            if self.drag_origin != Some((col, row)) {
                                self.has_selection = true;
                            }
                            self.ops.push(Op::SelUpdate { col, row, right_half });
                        }
                        Some(_) => {}
                        None if report && hovered && body.contains(pos) => {
                            if let Some(bytes) =
                                input::encode_mouse(MouseAction::Motion { held: None }, col as u32, row as u32, Mods::default(), mode)
                            {
                                source.send_mouse(&bytes.0);
                            }
                        }
                        None => {}
                    }
                }
                Event::MouseWheel { unit, delta, modifiers, .. } if hovered => {
                    let lines = match unit {
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Point => delta.y * ppp / cell_h,
                        egui::MouseWheelUnit::Page => delta.y * f32::from(rows),
                    };
                    self.wheel_accum += lines;
                    let n = self.wheel_accum.trunc() as i32;
                    self.wheel_accum -= n as f32;
                    if n == 0 {
                        continue;
                    }
                    let pos = ui.input(|i| i.pointer.hover_pos()).unwrap_or(body.center());
                    let (col, row, _) = to_cell(pos);
                    if input::mouse_reporting(mode) && !modifiers.shift {
                        let action = if n > 0 { MouseAction::WheelUp } else { MouseAction::WheelDown };
                        for _ in 0..n.unsigned_abs().min(10) {
                            if let Some(bytes) = input::encode_mouse(action, col as u32, row as u32, mods(modifiers), mode) {
                                source.send_mouse(&bytes.0);
                            }
                        }
                    } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                        let key = if n > 0 { Key::Up } else { Key::Down };
                        for _ in 0..n.unsigned_abs().min(10) {
                            if let Some(bytes) = input::encode_key(key, Mods::default(), mode) {
                                source.send_mouse(&bytes.0);
                            }
                        }
                    } else {
                        self.ops.push(Op::Scroll(n * 3));
                    }
                }
                _ => {}
            }
        }
        if typed {
            self.ops.push(Op::ScrollBottom);
            if self.has_selection {
                self.ops.push(Op::SelClear);
                self.has_selection = false;
            }
        }
        open_link
    }

    fn apply_ops(&mut self, t: &mut Terminal, ctx: &egui::Context) {
        for op in std::mem::take(&mut self.ops) {
            let offset = t.term().grid().display_offset() as i32;
            let point = |col: usize, row: usize| Point::new(Line(row as i32 - offset), Column(col));
            match op {
                Op::Scroll(n) => t.scroll(n),
                Op::ScrollBottom => t.scroll_to_bottom(),
                Op::SelStart { col, row, right_half, clicks } => {
                    let ty = match clicks {
                        1 => SelectionType::Simple,
                        2 => SelectionType::Semantic,
                        _ => SelectionType::Lines,
                    };
                    let side = if right_half { Side::Right } else { Side::Left };
                    t.term_mut().selection = Some(Selection::new(ty, point(col, row), side));
                    t.touch();
                }
                Op::SelUpdate { col, row, right_half } => {
                    let side = if right_half { Side::Right } else { Side::Left };
                    if let Some(sel) = t.term_mut().selection.as_mut() {
                        sel.update(point(col, row), side);
                    }
                    t.touch();
                }
                Op::SelClear => {
                    if t.term().selection.is_some() {
                        t.term_mut().selection = None;
                        t.touch();
                    }
                    self.has_selection = false;
                }
                Op::Copy { raw } => {
                    if let Some(text) = super::selection::visible_text(t)
                        && !text.is_empty()
                    {
                        let text = if self.clean_copy && !raw {
                            // The column the selection starts at: its first row may be partial.
                            let first_col = t.term().selection.as_ref().and_then(|s| s.to_range(t.term())).map_or(0, |r| r.start.column.0);
                            let clean = super::claude_copy::clean(&text, t.term().columns(), first_col);
                            if clean.is_empty() { text } else { clean }
                        } else {
                            text
                        };
                        ctx.copy_text(text);
                    }
                }
            }
        }
    }
}

/// Whether `open_url` would open it (the tooltip says so up front; the engine decides).
fn link_opens(uri: &str) -> bool {
    let lower = uri.trim().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// Ctrl+V (Cmd+V) with no text on the clipboard: egui-winit turns the shortcut into a text paste
/// and drops the key press when there is no text, so a copied image never shows up as an event.
/// The release still does: a V released with Ctrl held, with no press egui saw and no text
/// pasted, is a paste of something else. (After craft-anything's `ui-egui/src/paste.rs`.)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PasteWatch {
    down: bool,
    text_pasted: bool,
    command: bool,
}

impl PasteWatch {
    fn step(&mut self, events: &[Event], held: egui::Modifiers) -> bool {
        self.command |= held.command;
        let mut wanted = false;
        for e in events {
            match e {
                Event::Key { key: egui::Key::V, pressed: true, .. } | Event::Copy | Event::Cut => self.down = true,
                Event::Paste(_) => self.text_pasted = true,
                Event::Key { key: egui::Key::V, pressed: false, modifiers, .. } => {
                    wanted |= (self.command || modifiers.command) && !self.down && !self.text_pasted;
                    *self = PasteWatch::default();
                }
                Event::Key { modifiers, .. } => self.command |= modifiers.command,
                _ => {}
            }
        }
        if !held.command && !self.down {
            self.command = false;
        }
        wanted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(pressed: bool, command: bool) -> Event {
        let modifiers = if command { egui::Modifiers::COMMAND } else { egui::Modifiers::NONE };
        Event::Key { key: egui::Key::V, physical_key: None, pressed, repeat: false, modifiers }
    }

    #[test]
    fn image_paste_is_a_release_without_press_or_text() {
        let mut w = PasteWatch::default();
        // Text on the clipboard: egui saw the paste; no raw ^V.
        assert!(!w.step(&[Event::Paste("x".into()), key(false, true)], egui::Modifiers::COMMAND));
        // Image on the clipboard: only the release arrives.
        assert!(w.step(&[key(false, true)], egui::Modifiers::COMMAND));
        // Typing a plain v is no paste.
        assert!(!w.step(&[key(true, false), key(false, false)], egui::Modifiers::NONE));
    }
}

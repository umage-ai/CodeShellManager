//! The terminal widget: measures the grid from its rect every frame, turns egui input into PTY
//! bytes, and paints the pane's cached mesh (rebuilt only when something changed).

use std::sync::Arc;
use std::time::Instant;

use csm_term::alacritty_terminal::index::{Column, Line, Point, Side};
use csm_term::alacritty_terminal::selection::{Selection, SelectionType};
use csm_term::alacritty_terminal::term::TermMode;
use csm_term::input::{self, Key, MouseAction, MouseButton, Mods, Untrusted};
use csm_term::{GridSize, load_mode};
use egui::{
    Align2, Color32, Event, FontId, Id, ImeEvent, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
    pos2, vec2,
};

use crate::atlas::GlyphAtlas;
use crate::fonts::FontSet;
use crate::pane::{Op, Pane, RenderCache, lock};
use crate::render;
use crate::theme;

#[derive(Default, Debug, Clone, Copy)]
pub struct FrameCounters {
    pub panes: u32,
    pub rebuilds: u32,
    pub lock_misses: u32,
    /// Time spent building meshes (ms).
    pub build_ms: f32,
    /// Time inside the torn-off viewports' immediate callbacks, which also render and present
    /// those windows (ms).
    pub torn_ms: f32,
    pub grid_ms: f32,
    pub float_ms: f32,
    /// eframe's CPU time for the PREVIOUS frame: ui() + tessellation + rendering, excluding
    /// vsync waiting (ms).
    pub prev_frame_cpu_ms: f32,
}

/// Render resources shared by every pane in every window.
pub struct Shared {
    pub fonts: FontSet,
    pub atlas: Option<GlyphAtlas>,
    pub frame: FrameCounters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAction {
    Float,
    TearOff,
    Dock,
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

/// Draws one pane (header + terminal) into `rect`. Returns a header action, if any.
pub fn pane_ui(ui: &mut Ui, pane: &mut Pane, shared: &mut Shared, rect: Rect) -> Option<PaneAction> {
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let m = shared.fonts.metrics(ppp);
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), theme::PANE_HEADER));
    let body = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max);
    let inner = body.shrink(theme::PANE_PADDING);
    let cols = ((inner.width() * ppp) / m.cell_w).floor().clamp(2.0, 1000.0) as u16;
    let rows = ((inner.height() * ppp) / m.cell_h).floor().clamp(1.0, 500.0) as u16;
    let grid = GridSize { cols, rows, cell_width: m.cell_w as u16, cell_height: m.cell_h as u16 };
    let origin = pos2((inner.min.x * ppp).round() / ppp, (inner.min.y * ppp).round() / ppp);

    if pane.term.is_none() {
        pane.start(grid, &ctx);
    } else {
        pane.set_grid(grid);
    }
    pane.poll();

    let id = Id::new(("csm-term-pane", pane.id));
    let resp = ui.interact(body, id, Sense::click_and_drag());
    if resp.is_pointer_button_down_on() && !resp.has_focus() {
        resp.request_focus();
    }
    let focused = resp.has_focus();
    if focused {
        ui.memory_mut(|mem| {
            mem.set_focus_lock_filter(
                id,
                egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
            );
        });
    }
    let mode = pane.mode.as_ref().map(|m| load_mode(m)).unwrap_or_default();
    if focused != pane.was_focused {
        pane.was_focused = focused;
        if let Some(b) = input::focus(focused, mode) {
            pane.send(&b.0);
        }
    }

    handle_input(ui, pane, &resp, focused, mode, origin, ppp, m.cell_w, m.cell_h, cols, rows);

    // Build or reuse the mesh.
    let now = Instant::now();
    if let Some(term) = pane.term.clone() {
        let guard = match term.try_lock() {
            Ok(g) => Some(g),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        match guard {
            Some(mut t) => {
                apply_ops(&mut t, std::mem::take(&mut pane.ops), &mut pane.has_selection, &ctx);
                t.flush_sync_if_due(now);
                if let Some(deadline) = t.sync_deadline() {
                    ctx.request_repaint_after(deadline.saturating_duration_since(now));
                }
                let atlas = shared.atlas.get_or_insert_with(|| GlyphAtlas::new(&ctx));
                let key = (
                    t.generation(),
                    (u64::from(origin.x.to_bits()) << 32) | u64::from(origin.y.to_bits()),
                    ppp.to_bits(),
                    focused,
                    atlas.epoch,
                    m.px.to_bits(),
                    u32::from(cols) << 16 | u32::from(rows),
                );
                if pane.cache.as_ref().map(|c| c.key) != Some(key) {
                    let tb = Instant::now();
                    let built = render::build(&t, &mut shared.fonts, atlas, origin, ppp, m, focused);
                    shared.frame.rebuilds += 1;
                    shared.frame.build_ms += tb.elapsed().as_secs_f32() * 1000.0;
                    if atlas.full {
                        atlas.clear();
                        ctx.request_repaint();
                    }
                    pane.cache = Some(RenderCache { mesh: Arc::new(built.mesh), key, cursor_rect: built.cursor_rect });
                }
            }
            None => {
                shared.frame.lock_misses += 1;
                ctx.request_repaint();
            }
        }
    }
    shared.frame.panes += 1;

    // Paint.
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG);
    if let Some(cache) = &pane.cache {
        painter.add(Shape::Mesh(cache.mesh.clone()));
    }
    if let Some(e) = &pane.failed {
        painter.text(inner.center(), Align2::CENTER_CENTER, format!("failed to start: {e}"), FontId::proportional(13.0), theme::ALERT);
    } else if pane.pty().is_none() {
        painter.text(inner.center(), Align2::CENTER_CENTER, "starting…", FontId::proportional(13.0), theme::MUTED);
    }
    if focused
        && let Some(cache) = &pane.cache {
            let cursor_rect = cache.cursor_rect;
            ctx.output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    purpose: egui::IMEPurpose::Terminal,
                    rect: body,
                    cursor_rect,
                    should_interrupt_composition: false,
                });
            });
            if !pane.preedit.is_empty() {
                let font = FontId::monospace(shared.fonts.size_pt);
                let galley = painter.layout_no_wrap(pane.preedit.clone(), font, theme::FG);
                let r = Rect::from_min_size(cursor_rect.min, galley.size());
                painter.rect_filled(r.expand(1.0), 0.0, theme::PANEL);
                painter.galley(r.min, galley, theme::FG);
                painter.hline(r.x_range(), r.max.y, Stroke::new(1.0, theme::ACCENT));
            }
        }
    painter.rect_stroke(rect, 0.0, Stroke::new(theme::RING, if focused { theme::ACCENT } else { theme::BORDER }), StrokeKind::Inside);
    header_ui(ui, pane, header, grid)
}

fn header_ui(ui: &mut Ui, pane: &Pane, header: Rect, grid: GridSize) -> Option<PaneAction> {
    let painter = ui.painter_at(header);
    painter.rect_filled(header, 0.0, theme::TOOLBAR);
    let st = lock(&pane.status).clone();
    let backend = pane.backend.map_or("…", |b| b.label());
    let mut text = format!("{}  ·  {}  ·  {}x{}", pane.name, backend, grid.cols, grid.rows);
    if let Some(ps) = &st.program_status {
        let state = ps.split(':').next().unwrap_or("");
        text.push_str(&format!("  ·  7501 {state}"));
    }
    if let Some(a) = st.probe_answered {
        text.push_str(if a { "  ·  probe answered" } else { "  ·  probe not answered" });
    }
    if let Some(code) = st.exit {
        text.push_str(&format!("  ·  exited {code:?}"));
    }
    if !st.title.is_empty() {
        text.push_str(&format!("  ·  {}", st.title));
    }
    painter.text(header.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, text, FontId::proportional(theme::HEADER_TEXT), theme::FG);
    let mut action = None;
    let buttons: &[(PaneAction, &str)] = match pane.placement {
        crate::pane::Placement::Grid => &[(PaneAction::Float, "float"), (PaneAction::TearOff, "tear off")],
        _ => &[(PaneAction::Dock, "dock")],
    };
    let mut x = header.max.x - 4.0;
    for (a, label) in buttons.iter().rev() {
        let w = 58.0;
        let r = Rect::from_min_max(pos2(x - w, header.min.y + 1.0), pos2(x, header.max.y - 1.0));
        if ui.put(r, egui::Button::new(egui::RichText::new(*label).size(theme::HEADER_TEXT)).small()).clicked() {
            action = Some(*a);
        }
        x -= w + 4.0;
    }
    action
}

#[allow(clippy::too_many_arguments)]
fn handle_input(
    ui: &Ui,
    pane: &mut Pane,
    resp: &Response,
    focused: bool,
    mode: TermMode,
    origin: Pos2,
    ppp: f32,
    cell_w: f32,
    cell_h: f32,
    cols: u16,
    rows: u16,
) {
    let events = ui.input(|i| i.events.clone());
    let body = resp.rect;
    let hovered = resp.hovered() || resp.contains_pointer();
    let to_cell = |pos: Pos2| -> (usize, usize, bool) {
        let x = ((pos.x - origin.x) * ppp / cell_w).max(0.0);
        let y = ((pos.y - origin.y) * ppp / cell_h).max(0.0);
        let col = (x.floor() as usize).min(usize::from(cols).saturating_sub(1));
        let row = (y.floor() as usize).min(usize::from(rows).saturating_sub(1));
        (col, row, x.fract() >= 0.5)
    };
    let mut typed = false;
    for ev in events {
        match ev {
            Event::Text(text) if focused => {
                let alt_only = ui.input(|i| i.modifiers.alt && !i.modifiers.ctrl);
                if !alt_only {
                    pane.send(&input::typed(&text, false).0);
                    typed = true;
                }
            }
            Event::Key { key, pressed: true, modifiers, .. } if focused => {
                if let Some(k) = map_key(key, modifiers)
                    && let Some(bytes) = input::encode_key(k, mods(modifiers), mode)
                {
                    pane.send(&bytes.0);
                    typed = true;
                }
            }
            Event::Copy if focused => {
                if pane.has_selection {
                    pane.ops.push(Op::Copy);
                } else {
                    pane.send(&[0x03]);
                    typed = true;
                }
            }
            Event::Cut if focused => {
                pane.send(&[0x18]);
                typed = true;
            }
            Event::Paste(text) if focused => {
                pane.send(&input::paste(Untrusted(&text), mode));
                typed = true;
            }
            Event::Ime(ImeEvent::Preedit { text, .. }) if focused => pane.preedit = text,
            Event::Ime(ImeEvent::Commit(text)) if focused => {
                pane.preedit.clear();
                pane.send(&input::typed(&text, false).0);
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
                if pressed && body.contains(pos) {
                    if report {
                        if let Some(bytes) = input::encode_mouse(MouseAction::Press(b), col as u32, row as u32, mods(modifiers), mode) {
                            pane.send(&bytes.0);
                        }
                        pane.mouse_down = Some(b);
                    } else if b == MouseButton::Left {
                        let now = Instant::now();
                        let clicks = match pane.last_click {
                            Some((t, c, r, n)) if now.duration_since(t).as_millis() < 400 && c == col && r == row => (n % 3) + 1,
                            _ => 1,
                        };
                        pane.last_click = Some((now, col, row, clicks));
                        pane.ops.push(Op::SelStart { col, row, right_half, clicks });
                        pane.has_selection = clicks > 1;
                        pane.mouse_down = Some(b);
                        pane.drag_origin = Some((col, row));
                    }
                } else if !pressed && pane.mouse_down == Some(b) {
                    if report && let Some(bytes) = input::encode_mouse(MouseAction::Release(b), col as u32, row as u32, mods(modifiers), mode) {
                        pane.send(&bytes.0);
                    }
                    if !report && !pane.has_selection {
                        pane.ops.push(Op::SelClear);
                    }
                    pane.mouse_down = None;
                    pane.drag_origin = None;
                }
            }
            Event::PointerMoved(pos) => {
                let (col, row, right_half) = to_cell(pos);
                let shift = ui.input(|i| i.modifiers.shift);
                let report = input::mouse_reporting(mode) && !shift;
                match pane.mouse_down {
                    Some(b) if report => {
                        if let Some(bytes) = input::encode_mouse(MouseAction::Motion { held: Some(b) }, col as u32, row as u32, Mods::default(), mode) {
                            pane.send(&bytes.0);
                        }
                    }
                    Some(MouseButton::Left) => {
                        if pane.drag_origin != Some((col, row)) {
                            pane.has_selection = true;
                        }
                        pane.ops.push(Op::SelUpdate { col, row, right_half });
                    }
                    Some(_) => {}
                    None if report && hovered && body.contains(pos) => {
                        if let Some(bytes) = input::encode_mouse(MouseAction::Motion { held: None }, col as u32, row as u32, Mods::default(), mode) {
                            pane.send(&bytes.0);
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
                pane.wheel_accum += lines;
                let n = pane.wheel_accum.trunc() as i32;
                pane.wheel_accum -= n as f32;
                if n == 0 {
                    continue;
                }
                let pos = ui.input(|i| i.pointer.hover_pos()).unwrap_or(body.center());
                let (col, row, _) = to_cell(pos);
                if input::mouse_reporting(mode) && !modifiers.shift {
                    let action = if n > 0 { MouseAction::WheelUp } else { MouseAction::WheelDown };
                    for _ in 0..n.unsigned_abs().min(10) {
                        if let Some(bytes) = input::encode_mouse(action, col as u32, row as u32, mods(modifiers), mode) {
                            pane.send(&bytes.0);
                        }
                    }
                } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
                    let key = if n > 0 { Key::Up } else { Key::Down };
                    for _ in 0..n.unsigned_abs().min(10) {
                        if let Some(bytes) = input::encode_key(key, Mods::default(), mode) {
                            pane.send(&bytes.0);
                        }
                    }
                } else {
                    pane.ops.push(Op::Scroll(n * 3));
                }
            }
            _ => {}
        }
    }
    if typed {
        pane.ops.push(Op::ScrollBottom);
        if pane.has_selection {
            pane.ops.push(Op::SelClear);
            pane.has_selection = false;
        }
    }
}

fn apply_ops(t: &mut csm_term::Terminal, ops: Vec<Op>, has_selection: &mut bool, ctx: &egui::Context) {
    for op in ops {
        let offset = t.term().grid().display_offset() as i32;
        let point = |col: usize, row: usize| Point::new(Line(row as i32 - offset), Column(col));
        match op {
            Op::Resize(g) => t.resize(g),
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
                *has_selection = false;
            }
            Op::Copy => {
                if let Some(text) = t.term().selection_to_string()
                    && !text.is_empty()
                {
                    ctx.copy_text(text);
                }
            }
        }
    }
}

/// Unused-colour guard so theme tokens referenced only conditionally don't warn.
#[allow(dead_code)]
const _TOKENS: [Color32; 2] = [theme::GREEN, theme::PEACH];

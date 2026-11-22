//! The Usage dashboard: a page over the work area (like Settings) with Claude Code's tokens and
//! API-equivalent cost over a time range, from `usage_report` — figures (cost, tokens, cache hit
//! rate, cache-miss cost, saved by caching), a daily stacked bar chart split by token class, and
//! a table by session, project, model or day that drills down (a session → its models and days;
//! a model → its sessions). Opened from the palette (`usage_report`), the inspector's Usage tab
//! (on that session) and Settings → Usage.
//!
//! Every number comes from the tool's reply (`crate::usage_model::ReportView`); this module
//! decides what to ask (a poll, never a wait), and draws. Sorting the table is local.

use egui::{Align, Align2, CornerRadius, Id, Layout, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};
use serde_json::{Value, json};

use csm_core::claude_usage::{format_gap, format_tokens, format_usd};
use csm_core::usage_report::{DayRow, GroupBy, GroupRow, Metrics, MissEvent};

use crate::app::{Action, CsmApp};
use crate::icons;
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

/// How often an open dashboard asks again (real engine only).
pub const REPORT_EVERY_S: f64 = 30.0;

/// The time range picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Range {
    Today,
    #[default]
    Week,
    Month,
    All,
    Custom,
}

impl Range {
    pub const ALL: [Range; 5] = [Range::Today, Range::Week, Range::Month, Range::All, Range::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Range::Today => "Today",
            Range::Week => "7 days",
            Range::Month => "30 days",
            Range::All => "All",
            Range::Custom => "Custom",
        }
    }

    /// The tool's `from` word (a custom range sends its dates).
    fn word(self) -> &'static str {
        match self {
            Range::Today => "today",
            Range::Week | Range::Custom => "7d",
            Range::Month => "30d",
            Range::All => "all",
        }
    }
}

/// One drill-down step: the table's rows narrowed to a session, a model or a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Drill {
    Session { key: String, label: String },
    Model(String),
    Project(String),
}

impl Drill {
    fn label(&self) -> String {
        match self {
            Drill::Session { label, .. } => label.clone(),
            Drill::Model(m) => model_label(m),
            Drill::Project(p) if p.is_empty() => "(unknown)".to_owned(),
            Drill::Project(p) => p.clone(),
        }
    }
}

/// The table's sort column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortCol {
    Name,
    #[default]
    Cost,
    Tokens,
    HitRate,
    MissCost,
    Requests,
}

/// What the chart stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    Cost,
    Tokens,
}

/// The dashboard's state (UI only: what it shows is the tool's answer).
#[derive(Debug, Clone, Default)]
pub struct PageState {
    pub open: bool,
    pub range: Range,
    /// The custom range being typed, and the one sent.
    pub custom_from: String,
    pub custom_to: String,
    pub custom_applied: Option<(String, String)>,
    pub custom_error: Option<String>,
    pub drill: Vec<Drill>,
    /// The table's grouping; `None`: the drill level's default.
    pub group: Option<GroupBy>,
    /// Column, and ascending (off by default: the costliest first).
    pub sort: (SortCol, bool),
    pub metric: Metric,
    /// The params last asked, and when (UI time).
    pub asked: Option<(Value, f64)>,
}

impl PageState {
    /// The groupings the table offers at this drill level: not the dimensions already chosen
    /// (and not the project inside one session).
    pub fn groups(&self) -> Vec<GroupBy> {
        let has = |f: fn(&Drill) -> bool| self.drill.iter().any(f);
        let session = has(|d| matches!(d, Drill::Session { .. }));
        let model = has(|d| matches!(d, Drill::Model(_)));
        let project = has(|d| matches!(d, Drill::Project(_)));
        [GroupBy::Session, GroupBy::Project, GroupBy::Model, GroupBy::Day]
            .into_iter()
            .filter(|g| match g {
                GroupBy::Session => !session,
                GroupBy::Project => !project && !session,
                GroupBy::Model => !model,
                GroupBy::Day => true,
            })
            .collect()
    }

    pub fn group(&self) -> GroupBy {
        let groups = self.groups();
        self.group.filter(|g| groups.contains(g)).or_else(|| groups.first().copied()).unwrap_or(GroupBy::Day)
    }

    /// The `usage_report` params for what is shown. Pure.
    pub fn params(&self) -> Value {
        let mut p = json!({"group_by": self.group().id()});
        match (&self.range, &self.custom_applied) {
            (Range::Custom, Some((f, t))) => {
                p["from"] = json!(f);
                p["to"] = json!(t);
            }
            (r, _) => p["from"] = json!(r.word()),
        }
        for d in &self.drill {
            match d {
                Drill::Session { key, .. } => p["session"] = json!(key),
                Drill::Model(m) => p["model"] = json!(m),
                Drill::Project(pr) => p["project"] = json!(pr),
            }
        }
        p
    }

    /// A table row was clicked: narrow to it (a day: that day as the range).
    pub fn drill_into(&mut self, by: GroupBy, row: &GroupRow) {
        match by {
            GroupBy::Session => self.drill.push(Drill::Session { key: row.key.clone(), label: row.label.clone() }),
            GroupBy::Model => self.drill.push(Drill::Model(row.key.clone())),
            GroupBy::Project => self.drill.push(Drill::Project(row.key.clone())),
            GroupBy::Day => {
                self.range = Range::Custom;
                self.custom_from.clone_from(&row.key);
                self.custom_to.clone_from(&row.key);
                self.custom_applied = Some((row.key.clone(), row.key.clone()));
                self.custom_error = None;
                self.group = if self.groups().contains(&GroupBy::Session) { Some(GroupBy::Session) } else { None };
                return;
            }
        }
        self.group = None;
    }

    /// Back to the first `depth` drill steps (0: everything).
    pub fn pop_to(&mut self, depth: usize) {
        self.drill.truncate(depth);
        self.group = None;
    }

    /// Applies the custom range typed: both `YYYY-MM-DD`, `from` not after `to`.
    pub fn apply_custom(&mut self) {
        let (f, t) = (self.custom_from.trim().to_owned(), self.custom_to.trim().to_owned());
        let to = if t.is_empty() { f.clone() } else { t };
        if !is_date(&f) || !is_date(&to) {
            self.custom_error = Some("Dates are YYYY-MM-DD.".into());
        } else if to < f {
            self.custom_error = Some("The end is before the start.".into());
        } else {
            self.custom_error = None;
            self.custom_to.clone_from(&to);
            self.custom_applied = Some((f, to));
        }
    }

    pub fn sorted<'a>(&self, rows: &'a [GroupRow]) -> Vec<&'a GroupRow> {
        let mut v: Vec<&GroupRow> = rows.iter().collect();
        let (col, ascending) = self.sort;
        let key = |r: &GroupRow| -> f64 {
            match col {
                SortCol::Name => 0.0,
                SortCol::Cost => r.metrics.cost_usd,
                SortCol::Tokens => r.metrics.tokens.total() as f64,
                SortCol::HitRate => r.metrics.cache_hit_rate.unwrap_or(-1.0),
                SortCol::MissCost => r.metrics.cache_miss_cost_usd,
                SortCol::Requests => r.metrics.requests as f64,
            }
        };
        v.sort_by(|a, b| {
            let o = if col == SortCol::Name { a.label.to_lowercase().cmp(&b.label.to_lowercase()) } else { key(a).total_cmp(&key(b)) };
            let o = if ascending { o } else { o.reverse() };
            o.then_with(|| a.key.cmp(&b.key))
        });
        v
    }
}

/// `YYYY-MM-DD`, digits in place (the engine checks the calendar).
fn is_date(s: &str) -> bool {
    s.len() == 10 && s.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() })
}

/// `claude-opus-5-5` → `opus-5-5`.
pub fn model_label(m: &str) -> String {
    m.strip_prefix("claude-").unwrap_or(m).to_owned()
}

/// `2026-11-20` → `Nov 20`.
pub fn short_day(day: &str) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let month = day.get(5..7).and_then(|m| m.parse::<usize>().ok()).and_then(|m| m.checked_sub(1)).and_then(|i| M.get(i));
    let d = day.get(8..10).and_then(|d| d.parse::<u32>().ok());
    match (month, d) {
        (Some(m), Some(d)) => format!("{m} {d}"),
        _ => day.to_owned(),
    }
}

fn pct(v: Option<f64>) -> String {
    v.map_or_else(|| "–".to_owned(), |r| format!("{:.0}%", (r * 100.0).clamp(0.0, 100.0)))
}

/// Opens the dashboard (closing Settings), on a session when given.
pub fn open(app: &mut CsmApp, drill: Option<Drill>) {
    let p = &mut app.ui.usage_page;
    p.open = true;
    p.drill = drill.into_iter().collect();
    p.group = None;
    p.asked = None;
    app.ui.settings.open = false;
}

/// The token classes, bottom of the stack first: (label, colour, cost, tokens).
fn classes(t: &Tokens, m: &Metrics) -> [(&'static str, egui::Color32, f64, u64); 5] {
    let c = &m.cost_by_class;
    let k = &m.tokens;
    [
        ("Cache read", t.green, c.cache_read, k.cache_read),
        ("Cache write 5m", t.peach, c.cache_write_5m, k.cache_write_5m),
        ("Cache write 1h", t.yellow, c.cache_write_1h, k.cache_write_1h),
        ("Input", t.blue, c.input, k.input),
        ("Output", t.mauve, c.output, k.output),
    ]
}

pub fn show(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, t.bg);

    // Ask when what is shown changed, and now and then while open (the real engine).
    let now = ui.input(|i| i.time);
    let params = app.ui.usage_page.params();
    let live = app.model.usage.live;
    let due = match &app.ui.usage_page.asked {
        Some((p, at)) => *p != params || (live && now - at >= REPORT_EVERY_S),
        None => true,
    };
    if due {
        app.ui.usage_page.asked = Some((params.clone(), now));
        actions.push(Action::Tool("usage_report", params.clone()));
    }
    if live {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REPORT_EVERY_S));
    }

    let col_w = (rect.width() - space::XL * 2.0).clamp(size::MENU_W, size::USAGE_PAGE_MAX_W);
    let col = Rect::from_min_max(pos2(rect.min.x + space::XL, rect.min.y + space::LG), pos2(rect.min.x + space::XL + col_w, rect.max.y));
    ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
        egui::ScrollArea::vertical().id_salt("usage-page").auto_shrink([false, false]).show(ui, |ui| {
            let w = col_w - space::SM;
            ui.set_width(w);
            ui.set_max_width(w);
            ui.spacing_mut().item_spacing.y = space::SM;
            header(ui, app, actions);
            toolbar(ui, app);
            breadcrumb(ui, app);
            let usage = app.model.usage.clone();
            if let Some(e) = &usage.report_error {
                note(ui, e, t.red);
            }
            let Some(view) = usage.report.as_ref() else {
                note(ui, if usage.report_error.is_some() { "" } else { "Reading Claude's transcripts…" }, t.muted);
                return;
            };
            if view.params != params {
                note(ui, "Updating…", t.muted);
            }
            let r = &view.report;
            figures(ui, &r.total, r.sessions);
            if r.total.unpriced_requests > 0 {
                let n = r.total.unpriced_requests;
                let s = if n == 1 { "request is" } else { "requests are" };
                note(ui, &format!("No price for {}: {n} {s} counted, not costed.", r.total.unpriced_models.join(", ")), t.muted);
            }
            ui.add_space(space::XS);
            chart_card(ui, app, &r.days);
            ui.add_space(space::XS);
            table_card(ui, app, &r.rows, r.truncated);
            if !r.misses.is_empty() {
                ui.add_space(space::XS);
                let drilled = app.ui.usage_page.drill.iter().find_map(|d| match d {
                    Drill::Session { key, label } => Some((key.clone(), label.clone())),
                    _ => None,
                });
                misses_card(ui, &r.misses, &r.rows, r.group_by, drilled);
            }
            ui.add_space(space::XS);
            let mut foot = String::from(super::usage::API_NOTE);
            if let Some(v) = &view.price_version {
                foot.push_str(&format!(" Prices checked {v}."));
            }
            if !view.history_scanned {
                foot.push_str(" Still listing the transcripts on this machine.");
            } else if view.history_pending > 0 {
                foot.push_str(&format!(" Reading the history: {} transcripts to go.", view.history_pending));
            }
            note(ui, &foot, t.muted);
            note(ui, DEFINITIONS, t.faint);
            ui.add_space(space::XL);
        });
    });
}

const DEFINITIONS: &str = "Cache hit rate: cache reads ÷ (input + cache reads + cache writes). Cache-miss cost: what the input and cache writes cost above the cache-read price. Cache misses: a request that re-wrote more than 20k tokens after its context was not read from the cache (usually the cache expired while idle); impact = the re-write above reading. Saved by caching: reads at the input price minus the cache writes' premium. As ContextCost computes them.";

fn note(ui: &mut Ui, s: &str, color: egui::Color32) {
    if !s.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new(s).font(theme::regular(text::CAPTION)).color(color)).wrap());
    }
}

fn header(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let (ir, _) = ui.allocate_exact_size(egui::Vec2::splat(icon::XXL), Sense::hover());
        icons::paint(ui, ir, "gauge", icon::XXL, t.blue);
        ui.label(egui::RichText::new("Claude usage").font(semibold(text::TITLE)).color(t.fg));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::button(ui, ButtonKind::Ghost, Some("x"), "Close usage", Some("Esc")).clicked() {
                actions.push(Action::CloseUsage);
            }
            if app.model.usage.live && widgets::button(ui, ButtonKind::Secondary, Some("restart"), "Refresh", None).clicked() {
                app.ui.usage_page.asked = None;
            }
        });
    });
}

fn toolbar(ui: &mut Ui, app: &mut CsmApp) {
    let t = Tokens::get(ui.ctx());
    let page = &mut app.ui.usage_page;
    ui.horizontal(|ui| {
        let labels: Vec<&str> = Range::ALL.iter().map(|r| r.label()).collect();
        let sel = Range::ALL.iter().position(|r| *r == page.range);
        if let Some(i) = widgets::segmented(ui, "Range", &labels, sel)
            && let Some(r) = Range::ALL.get(i)
        {
            page.range = *r;
        }
        if page.range == Range::Custom {
            ui.add_space(space::SM);
            let mut enter = false;
            for (id, buf, hint) in [("usage-from", &mut page.custom_from, "from YYYY-MM-DD"), ("usage-to", &mut page.custom_to, "to YYYY-MM-DD")] {
                let edit = egui::TextEdit::singleline(buf)
                    .id(Id::new(id))
                    .hint_text(egui::RichText::new(hint).color(t.faint))
                    .font(theme::mono(text::MONO))
                    .desired_width(size::DATE_FIELD_W);
                let r = ui.add(edit);
                enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }
            if widgets::button(ui, ButtonKind::Secondary, None, "Apply", None).clicked() || enter {
                page.apply_custom();
            }
        }
    });
    if page.range == Range::Custom {
        if let Some(e) = &page.custom_error {
            note(ui, e, t.red);
        } else if page.custom_applied.is_none() {
            note(ui, "Type the first and last day, then Apply. Until then: the last 7 days.", t.muted);
        }
    }
}

fn breadcrumb(ui: &mut Ui, app: &mut CsmApp) {
    let t = Tokens::get(ui.ctx());
    let page = &mut app.ui.usage_page;
    if page.drill.is_empty() {
        return;
    }
    let mut pop: Option<usize> = None;
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Ghost, None, "All usage", None).clicked() {
            pop = Some(0);
        }
        let n = page.drill.len();
        for (i, d) in page.drill.iter().enumerate() {
            ui.label(egui::RichText::new("›").font(theme::regular(text::BODY)).color(t.faint));
            let label = d.label();
            if i + 1 == n {
                ui.label(egui::RichText::new(label).font(semibold(text::BODY)).color(t.fg));
            } else if widgets::button(ui, ButtonKind::Ghost, None, &label, None).clicked() {
                pop = Some(i + 1);
            }
        }
    });
    if let Some(d) = pop {
        page.pop_to(d);
    }
}

/// The figures row: cost, tokens, cache hit rate, cache-miss cost, saved by caching.
fn figures(ui: &mut Ui, m: &Metrics, sessions: u64) {
    let t = Tokens::get(ui.ctx());
    let unpriced = if m.unpriced_requests > 0 { format!(" + {} unpriced", m.unpriced_requests) } else { String::new() };
    let k = &m.tokens;
    let plural = |n: u64, one: &str| match (n, one) {
        (1, _) => format!("1 {one}"),
        (_, "miss") => format!("{n} misses"),
        _ => format!("{n} {one}s"),
    };
    // (label, value, the line under it, the whole story on hover)
    let tiles: [(&str, String, String, String); 5] = [
        (
            "API-equivalent cost",
            format_usd(m.cost_usd),
            format!("{} · {}", plural(m.requests, "request"), plural(sessions, "session")),
            format!("{} requests in {} sessions{unpriced}. {}", m.requests, sessions, super::usage::API_NOTE),
        ),
        (
            "Tokens",
            format_tokens(k.total()),
            format!("read {} · write {}", format_tokens(k.cache_read), format_tokens(k.cache_write())),
            format!(
                "Input {} · output {} · cache writes {} (5m) + {} (1h) · cache reads {}",
                format_tokens(k.input),
                format_tokens(k.output),
                format_tokens(k.cache_write_5m),
                format_tokens(k.cache_write_1h),
                format_tokens(k.cache_read)
            ),
        ),
        (
            "Cache hit rate",
            pct(m.cache_hit_rate),
            format!("of {} prompt tokens", format_tokens(k.prompt())),
            format!("{} of {} prompt tokens (input + cache reads + cache writes) were read from the cache.", format_tokens(k.cache_read), format_tokens(k.prompt())),
        ),
        (
            "Cache-miss cost",
            format_usd(m.cache_miss_cost_usd),
            format!("{} · {}", plural(m.miss_events, "miss"), format_usd(m.miss_event_cost_usd)),
            format!(
                "Input and cache writes cost {} more than cache reads would have. {} cache misses (a re-write after the cache expired) cost {} of it.",
                format_usd(m.cache_miss_cost_usd),
                m.miss_events,
                format_usd(m.miss_event_cost_usd)
            ),
        ),
        (
            "Saved by caching",
            format_usd(m.net_cache_savings_usd().max(0.0)),
            format!("{} − {} premium", format_usd(m.cache_savings_usd), format_usd(m.cache_write_premium_usd)),
            format!(
                "Cache reads saved {} against the input price; cache writes cost {} more than input.",
                format_usd(m.cache_savings_usd),
                format_usd(m.cache_write_premium_usd)
            ),
        ),
    ];
    let w = ui.available_width();
    let per_row = ((w + space::SM) / (size::USAGE_KPI_MIN_W + space::SM)).floor().clamp(1.0, tiles.len() as f32) as usize;
    let tile_w = ((w - space::SM * (per_row as f32 - 1.0)) / per_row as f32).floor();
    for chunk in tiles.chunks(per_row) {
        let (row, _) = ui.allocate_exact_size(vec2(w, size::USAGE_KPI_H), Sense::hover());
        for (i, (label, value, detail, story)) in chunk.iter().enumerate() {
            let r = Rect::from_min_size(pos2(row.min.x + (tile_w + space::SM) * i as f32, row.min.y), vec2(tile_w, size::USAGE_KPI_H));
            ui.painter().rect(r, CornerRadius::same(radius::LG), t.mantle, Stroke::new(size::HAIRLINE, t.surface0), egui::StrokeKind::Inside);
            ui.interact(r, Id::new(("usage-figure", *label)), Sense::hover()).on_hover_text(story.as_str());
            let inner = r.shrink2(vec2(space::MD, space::SM));
            // A child that takes no room in the page (the row already did).
            let mut tile = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
            tile.set_clip_rect(inner);
            tile.spacing_mut().item_spacing.y = space::XXS;
            widgets::caps(&mut tile, label);
            tile.label(egui::RichText::new(value).font(semibold(text::DISPLAY)).color(t.fg));
            tile.add(egui::Label::new(egui::RichText::new(detail).font(theme::regular(text::CAPTION)).color(t.muted)).truncate());
        }
    }
}

fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let t = Tokens::get(ui.ctx());
    widgets::card_frame(&t).fill(t.mantle).show(ui, |ui| {
        let w = ui.available_width();
        ui.set_width(w);
        ui.set_max_width(w);
        add(ui);
    });
}

fn chart_card(ui: &mut Ui, app: &mut CsmApp, days: &[DayRow]) {
    let t = Tokens::get(ui.ctx());
    let metric = app.ui.usage_page.metric;
    card(ui, |ui| {
        ui.horizontal(|ui| {
            widgets::caps(ui, if metric == Metric::Cost { "Cost per day" } else { "Tokens per day" });
            ui.add_space(space::LG);
            // The legend.
            for (label, color, ..) in classes(&t, &Metrics::default()) {
                let (r, _) = ui.allocate_exact_size(vec2(size::SWATCH, size::SWATCH), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(radius::SM / 2), color);
                ui.label(egui::RichText::new(label).font(theme::regular(text::CAPTION)).color(t.fg2));
                ui.add_space(space::XS);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let sel = Some(usize::from(metric == Metric::Tokens));
                if let Some(i) = widgets::segmented(ui, "Chart", &["Cost", "Tokens"], sel) {
                    app.ui.usage_page.metric = if i == 0 { Metric::Cost } else { Metric::Tokens };
                }
            });
        });
        chart(ui, days, metric);
    });
}

/// A round number at or above `v` (1, 2, 2.5, 5 × 10ⁿ).
fn nice_ceil(v: f64) -> f64 {
    if !v.is_finite() || v <= 0.0 {
        return 1.0;
    }
    let p = 10f64.powf(v.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0].into_iter().map(|m| m * p).find(|x| *x >= v).unwrap_or(10.0 * p)
}

/// The daily stacked bars, with the value axis on the left, dates under it, and the day under
/// the pointer described in a tooltip.
fn chart(ui: &mut Ui, days: &[DayRow], metric: Metric) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::USAGE_CHART_H + size::USAGE_AXIS_H), Sense::hover());
    let plot = Rect::from_min_max(pos2(rect.min.x + size::USAGE_AXIS_W, rect.min.y + space::SM), pos2(rect.max.x, rect.min.y + size::USAGE_CHART_H));
    let p = ui.painter_at(rect);
    let value = |c: f64, k: u64| if metric == Metric::Cost { c } else { k as f64 };
    let totals: Vec<f64> = days.iter().map(|d| classes(&t, &d.metrics).iter().map(|(_, _, c, k)| value(*c, *k)).sum()).collect();
    let top = nice_ceil(totals.iter().copied().fold(0.0, f64::max));
    let fmt = |v: f64| if metric == Metric::Cost { format_usd(v) } else { format_tokens(v.max(0.0) as u64) };
    let y_of = |v: f64| plot.max.y - plot.height() * (v / top).clamp(0.0, 1.0) as f32;
    for i in 0..=4 {
        let v = top * f64::from(i) / 4.0;
        let y = y_of(v);
        p.hline(plot.x_range(), y, Stroke::new(size::HAIRLINE, if i == 0 { t.surface1 } else { t.surface0 }));
        p.text(pos2(plot.min.x - space::SM, y), Align2::RIGHT_CENTER, if i == 0 { "0".to_owned() } else { fmt(v) }, theme::mono(text::MONO_SM), t.muted);
    }
    let n = days.len().max(1);
    let slot = plot.width() / n as f32;
    let bar_w = (slot * size::USAGE_BAR_FILL).max(size::HAIRLINE);
    let hover = resp.hover_pos().filter(|h| plot.x_range().contains(h.x)).map(|h| (((h.x - plot.min.x) / slot).floor() as usize).min(n - 1));
    // Dates: as many as fit without touching, counted back from the last.
    let label_w = widgets::text_width(ui, "Nov 30", theme::mono(text::MONO_SM)) + space::MD;
    let every = ((label_w / slot).ceil() as usize).max(1);
    for (i, d) in days.iter().enumerate() {
        let x0 = plot.min.x + slot * i as f32;
        let cx = x0 + slot / 2.0;
        if hover == Some(i) {
            p.rect_filled(Rect::from_min_max(pos2(x0, plot.min.y), pos2(x0 + slot, plot.max.y)), CornerRadius::same(radius::SM), t.row_hover);
        }
        let mut acc = 0.0;
        for (_, color, c, k) in classes(&t, &d.metrics) {
            let v = value(c, k);
            if v <= 0.0 {
                continue;
            }
            let (y1, y0) = (y_of(acc), y_of(acc + v));
            acc += v;
            p.rect_filled(Rect::from_min_max(pos2(cx - bar_w / 2.0, y0), pos2(cx + bar_w / 2.0, y1)), CornerRadius::ZERO, color);
        }
        let last = i + 1 == days.len();
        if (days.len() - 1 - i).is_multiple_of(every) {
            // The last label keeps inside the plot.
            let (x, align) = if last && every > 1 { (plot.max.x, Align2::RIGHT_TOP) } else { (cx, Align2::CENTER_TOP) };
            p.text(pos2(x, plot.max.y + space::XS), align, short_day(&d.day), theme::mono(text::MONO_SM), if last { t.fg2 } else { t.muted });
        }
    }
    if days.is_empty() {
        p.text(plot.center(), Align2::CENTER_CENTER, "No days in this range.", theme::regular(text::BODY), t.muted);
    }
    let total: f64 = totals.iter().sum();
    let label = format!("{} per day, {} days, {} in all", if metric == Metric::Cost { "Cost" } else { "Tokens" }, days.len(), fmt(total));
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Image, true, label.clone()));
    if let Some(d) = hover.and_then(|i| days.get(i)) {
        resp.on_hover_ui_at_pointer(|ui| day_tooltip(ui, d));
    }
}

fn day_tooltip(ui: &mut Ui, d: &DayRow) {
    let t = Tokens::get(ui.ctx());
    let m = &d.metrics;
    ui.label(egui::RichText::new(format!("{} · {}", short_day(&d.day), format_usd(m.cost_usd))).font(semibold(text::BODY)).color(t.fg));
    egui::Grid::new("usage-day-tip").num_columns(3).spacing(vec2(space::MD, space::XXS)).show(ui, |ui| {
        for (label, color, c, k) in classes(&t, m).into_iter().rev() {
            ui.label(egui::RichText::new(label).font(theme::regular(text::CAPTION)).color(color));
            ui.label(egui::RichText::new(format_tokens(k)).font(theme::mono(text::MONO_SM)).color(t.fg2));
            ui.label(egui::RichText::new(format_usd(c)).font(theme::mono(text::MONO_SM)).color(t.fg2));
            ui.end_row();
        }
    });
    ui.label(
        egui::RichText::new(format!("{} requests · cache hits {} · {} misses", m.requests, pct(m.cache_hit_rate), m.miss_events))
            .font(theme::regular(text::CAPTION))
            .color(t.muted),
    );
}

/// The columns after the name that fit in `w`.
fn columns(w: f32) -> Vec<(SortCol, &'static str)> {
    let all = [(SortCol::Cost, "Cost"), (SortCol::Tokens, "Tokens"), (SortCol::HitRate, "Cache hits"), (SortCol::MissCost, "Miss cost"), (SortCol::Requests, "Requests")];
    let fit = ((w - size::USAGE_KPI_MIN_W * 1.5) / size::USAGE_NUM_W).floor().max(1.0) as usize;
    all.into_iter().take(fit).collect()
}

fn cell(m: &Metrics, col: SortCol) -> String {
    match col {
        SortCol::Name => String::new(),
        SortCol::Cost if m.unpriced_requests > 0 && m.cost_usd <= 0.0 => "unpriced".to_owned(),
        SortCol::Cost => format_usd(m.cost_usd),
        SortCol::Tokens => format_tokens(m.tokens.total()),
        SortCol::HitRate => pct(m.cache_hit_rate),
        SortCol::MissCost => format_usd(m.cache_miss_cost_usd),
        SortCol::Requests => m.requests.to_string(),
    }
}

fn group_label(g: GroupBy) -> &'static str {
    match g {
        GroupBy::Session => "Sessions",
        GroupBy::Project => "Projects",
        GroupBy::Model => "Models",
        GroupBy::Day => "Days",
    }
}

/// The second line of a row: where it ran and on what.
fn row_detail(by: GroupBy, r: &GroupRow) -> String {
    let models = || r.models.iter().take(3).map(|m| model_label(m)).collect::<Vec<_>>().join(", ");
    let sessions = if r.sessions == 1 { "1 session".to_owned() } else { format!("{} sessions", r.sessions) };
    match by {
        GroupBy::Session => {
            // A conversation CSM didn't run is already labelled with its project.
            let project = r.projects.first().filter(|p| !p.is_empty() && !r.label.starts_with(p.as_str())).cloned().unwrap_or_default();
            [project, models()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
        }
        GroupBy::Project | GroupBy::Day => format!("{sessions} · {}", models()),
        GroupBy::Model => format!("{sessions} · {} requests", r.metrics.requests),
    }
}

fn row_label(by: GroupBy, r: &GroupRow) -> String {
    match by {
        GroupBy::Day => short_day(&r.label),
        GroupBy::Model => model_label(&r.label),
        _ if r.label.is_empty() => "(unknown)".to_owned(),
        _ => r.label.clone(),
    }
}

fn table_card(ui: &mut Ui, app: &mut CsmApp, rows: &[GroupRow], truncated: bool) {
    let t = Tokens::get(ui.ctx());
    let page = app.ui.usage_page.clone();
    let by = page.group();
    card(ui, |ui| {
        ui.horizontal(|ui| {
            let groups = page.groups();
            let labels: Vec<&str> = groups.iter().map(|g| group_label(*g)).collect();
            let sel = groups.iter().position(|g| *g == by);
            if let Some(i) = widgets::segmented(ui, "Group by", &labels, sel) {
                app.ui.usage_page.group = groups.get(i).copied();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let hint = match by {
                    GroupBy::Session => "Click a session for its models and days",
                    GroupBy::Model => "Click a model to narrow to it",
                    GroupBy::Project => "Click a project for its sessions",
                    GroupBy::Day => "Click a day to see just that day",
                };
                ui.label(egui::RichText::new(hint).font(theme::regular(text::CAPTION)).color(t.faint));
            });
        });
        let w = ui.available_width();
        let cols = columns(w);
        let num_x = |i: usize| w - size::USAGE_NUM_W * (cols.len() - i) as f32;
        // The header: click to sort (again to flip).
        let (head, _) = ui.allocate_exact_size(vec2(w, size::CONTROL_SM), Sense::hover());
        let name_label = match by {
            GroupBy::Session => "Session",
            GroupBy::Project => "Project",
            GroupBy::Model => "Model",
            GroupBy::Day => "Day",
        };
        let mut heads: Vec<(SortCol, &str, Rect, Align2)> =
            vec![(SortCol::Name, name_label, Rect::from_min_max(head.min, pos2(head.min.x + num_x(0), head.max.y)), Align2::LEFT_CENTER)];
        for (i, (c, h)) in cols.iter().enumerate() {
            let r = Rect::from_min_max(pos2(head.min.x + num_x(i), head.min.y), pos2(head.min.x + num_x(i) + size::USAGE_NUM_W, head.max.y));
            heads.push((*c, h, r, Align2::RIGHT_CENTER));
        }
        for (c, h, r, align) in heads {
            let resp = ui.interact(r, Id::new(("usage-sort", h)), Sense::click());
            let on = page.sort.0 == c;
            let arrow = if on { if page.sort.1 { " ↑" } else { " ↓" } } else { "" };
            let pos = if align == Align2::RIGHT_CENTER { pos2(r.max.x - space::SM, r.center().y) } else { pos2(r.min.x + space::SM, r.center().y) };
            ui.painter().text(pos, align, format!("{}{arrow}", h.to_uppercase()), semibold(text::CAPS), if on || resp.hovered() { t.fg2 } else { t.muted });
            let label = format!("Sort by {h}");
            resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                let ascending = if on { !page.sort.1 } else { c == SortCol::Name };
                app.ui.usage_page.sort = (c, ascending);
            }
        }
        ui.painter().hline(head.x_range(), head.max.y, Stroke::new(size::HAIRLINE, t.surface0));
        if rows.is_empty() {
            ui.add_space(space::SM);
            ui.label(egui::RichText::new("No Claude requests in this range.").font(theme::regular(text::BODY)).color(t.muted));
            return;
        }
        let max_cost = rows.iter().map(|r| r.metrics.cost_usd).fold(0.0, f64::max);
        let can_drill = page.groups().len() > 1 || by == GroupBy::Day;
        let mut picked: Option<GroupRow> = None;
        for r in page.sorted(rows) {
            let (rr, resp) = ui.allocate_exact_size(vec2(w, size::USAGE_ROW_H), if can_drill { Sense::click() } else { Sense::hover() });
            if resp.hovered() {
                ui.painter().rect_filled(rr, CornerRadius::same(radius::MD), t.row_hover);
            }
            let name_w = num_x(0) - space::LG;
            let label = row_label(by, r);
            let top_y = rr.min.y + space::SM + text::BODY / 2.0;
            widgets::paint_text(ui, pos2(rr.min.x + space::SM, top_y), Align2::LEFT_CENTER, &label, theme::medium(text::BODY), t.fg, name_w);
            widgets::paint_text(
                ui,
                pos2(rr.min.x + space::SM, rr.max.y - space::SM - text::CAPTION / 2.0),
                Align2::LEFT_CENTER,
                &row_detail(by, r),
                theme::regular(text::CAPTION),
                t.muted,
                name_w,
            );
            // The share of the costliest row, under the cost.
            if max_cost > 0.0
                && let Some(ci) = cols.iter().position(|(c, _)| *c == SortCol::Cost)
            {
                let x1 = rr.min.x + num_x(ci) + size::USAGE_NUM_W - space::SM;
                let bw = (size::USAGE_NUM_W - space::LG) * (r.metrics.cost_usd / max_cost).clamp(0.0, 1.0) as f32;
                let bar = Rect::from_min_max(pos2(x1 - bw, rr.max.y - space::SM - size::STRIPE), pos2(x1, rr.max.y - space::SM));
                ui.painter().rect_filled(bar, CornerRadius::same(radius::SM / 2), Tokens::tint(t.blue, alpha::SHARE_BAR));
            }
            for (i, (c, _)) in cols.iter().enumerate() {
                let x = rr.min.x + num_x(i) + size::USAGE_NUM_W - space::SM;
                let strong = *c == page.sort.0 || *c == SortCol::Cost;
                ui.painter().text(pos2(x, top_y), Align2::RIGHT_CENTER, cell(&r.metrics, *c), theme::mono(text::MONO), if strong { t.fg } else { t.fg2 });
            }
            ui.painter().hline(rr.x_range(), rr.max.y, Stroke::new(size::HAIRLINE, t.surface0));
            let a11y = format!("Usage row: {label}");
            resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, a11y.clone()));
            if can_drill && resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                picked = Some(r.clone());
            }
        }
        if truncated {
            ui.add_space(space::XS);
            ui.label(egui::RichText::new("Only the costliest 500 are listed.").font(theme::regular(text::CAPTION)).color(t.muted));
        }
        if let Some(r) = picked {
            app.ui.usage_page.drill_into(by, &r);
        }
    });
}

fn misses_card(ui: &mut Ui, misses: &[MissEvent], rows: &[GroupRow], by: GroupBy, drilled: Option<(String, String)>) {
    let t = Tokens::get(ui.ctx());
    card(ui, |ui| {
        widgets::caps(ui, "Cache misses · largest re-writes");
        egui::Grid::new("usage-misses").num_columns(5).spacing(vec2(space::XL, space::XS)).show(ui, |ui| {
            for h in ["Session", "Model", "Re-written", "Idle before", "Impact"] {
                ui.label(egui::RichText::new(h.to_uppercase()).font(semibold(text::CAPS)).color(t.muted));
            }
            ui.end_row();
            for m in misses.iter().take(10) {
                let name = match by {
                    GroupBy::Session => rows.iter().find(|r| r.key == m.session_key).map(|r| r.label.clone()),
                    _ => drilled.as_ref().filter(|(k, _)| *k == m.session_key).map(|(_, l)| l.clone()),
                }
                .unwrap_or_else(|| {
                    let short: String = m.session_key.chars().take(8).collect();
                    if m.project.is_empty() { short } else { format!("{} · {short}", m.project) }
                });
                let name = if m.subagent { format!("{name} (subagent)") } else { name };
                ui.label(egui::RichText::new(name).font(theme::regular(text::SMALL)).color(t.fg));
                ui.label(egui::RichText::new(model_label(&m.model)).font(theme::mono(text::MONO_SM)).color(t.fg2));
                ui.label(egui::RichText::new(format_tokens(m.cache_write)).font(theme::mono(text::MONO_SM)).color(t.fg2));
                ui.label(egui::RichText::new(m.idle_gap_ms.map_or_else(|| "–".to_owned(), format_gap)).font(theme::mono(text::MONO_SM)).color(t.fg2));
                ui.label(egui::RichText::new(format_usd(m.impact_usd)).font(theme::mono(text::MONO_SM)).color(t.fg));
                ui.end_row();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, label: &str, cost: f64, requests: u64) -> GroupRow {
        GroupRow { key: key.into(), label: label.into(), metrics: Metrics { cost_usd: cost, requests, ..Metrics::default() }, ..GroupRow::default() }
    }

    #[test]
    fn params_follow_the_range_drill_and_group() {
        let mut p = PageState::default();
        assert_eq!(p.params(), json!({"from": "7d", "group_by": "session"}));
        p.range = Range::Month;
        p.drill_into(GroupBy::Session, &row("s1", "Shop", 1.0, 1));
        assert_eq!(p.params(), json!({"from": "30d", "group_by": "model", "session": "s1"}), "a session: its models");
        assert_eq!(p.groups(), [GroupBy::Model, GroupBy::Day]);
        p.drill_into(GroupBy::Model, &row("claude-opus-5-5", "claude-opus-5-5", 1.0, 1));
        assert_eq!(p.params()["group_by"], "day", "a session's model: its days");
        p.pop_to(0);
        p.drill_into(GroupBy::Model, &row("claude-opus-5-5", "claude-opus-5-5", 1.0, 1));
        assert_eq!(p.params(), json!({"from": "30d", "group_by": "session", "model": "claude-opus-5-5"}), "a model: its sessions");
        p.group = Some(GroupBy::Model);
        assert_eq!(p.group(), GroupBy::Session, "a grouping that is drilled already falls back");
        p.drill_into(GroupBy::Day, &row("2026-11-20", "2026-11-20", 1.0, 1));
        assert_eq!(p.params()["from"], "2026-11-20");
        assert_eq!(p.params()["to"], "2026-11-20");
        assert_eq!(p.range, Range::Custom);
    }

    #[test]
    fn the_custom_range_is_checked_before_it_is_sent() {
        let mut p = PageState { range: Range::Custom, ..PageState::default() };
        assert_eq!(p.params()["from"], "7d", "nothing applied yet");
        p.custom_from = "2026-11-1".into();
        p.apply_custom();
        assert!(p.custom_error.is_some());
        p.custom_from = "2026-11-10".into();
        p.custom_to = "2026-11-01".into();
        p.apply_custom();
        assert_eq!(p.custom_error.as_deref(), Some("The end is before the start."));
        p.custom_to = String::new();
        p.apply_custom();
        assert_eq!(p.custom_error, None);
        assert_eq!(p.params()["to"], "2026-11-10", "an empty end is the same day");
    }

    #[test]
    fn sorting_is_local_costliest_first_and_flips() {
        let rows = vec![row("a", "beta", 1.0, 9), row("b", "Alpha", 3.0, 1), row("c", "gamma", 2.0, 5)];
        let mut p = PageState::default();
        assert_eq!(p.sorted(&rows).iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["b", "c", "a"], "the default: costliest first");
        p.sort = (SortCol::Requests, false);
        assert_eq!(p.sorted(&rows)[0].key, "a");
        p.sort = (SortCol::Name, true);
        assert_eq!(p.sorted(&rows).iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["Alpha", "beta", "gamma"]);
    }

    #[test]
    fn labels_and_axis() {
        assert_eq!(short_day("2026-11-20"), "Nov 20");
        assert_eq!(short_day("2026-01-02"), "Jan 2");
        assert_eq!(short_day("garbage"), "garbage");
        assert_eq!(short_day("2026-13-01"), "2026-13-01");
        assert_eq!(model_label("claude-opus-5-5"), "opus-5-5");
        assert_eq!(model_label("qwen3.8:27b"), "qwen3.8:27b");
        assert_eq!(nice_ceil(0.0), 1.0);
        assert_eq!(nice_ceil(3.2), 5.0);
        assert_eq!(nice_ceil(18.0), 20.0);
        assert_eq!(nice_ceil(21.0), 25.0);
        assert_eq!(nice_ceil(f64::NAN), 1.0);
        assert!(is_date("2026-11-20") && !is_date("2026/11/20") && !is_date("20261120"));
        assert_eq!(pct(Some(0.934)), "93%");
        assert_eq!(pct(None), "–");
    }
}

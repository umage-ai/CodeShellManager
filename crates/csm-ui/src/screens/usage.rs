//! Claude usage on screen: the inspector's Usage tab, the sidebar rows' fill chips and Settings →
//! Usage's cost per project. Everything comes from the usage tools (`usage_session`,
//! `usage_context`, `usage_summary`) through [`crate::model::UiModel::usage`]; this module only
//! decides when to ask again (a poll, never a wait) and draws.
//!
//! Costs are always labelled **API-equivalent**: what the tokens would cost at API list prices,
//! not what a subscription bills.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::json;

use csm_core::claude_usage::{self as cu, FillLevel, format_gap, format_tokens, format_usd};

use crate::app::{Action, CsmApp};
use crate::model::SessionView;
use crate::theme::{self, Tokens, alpha, radius, semibold, size, space, text};
use crate::usage_model::{SummaryView, UsageChip};
use crate::widgets::{self, ButtonKind};

/// How often the sidebar's chips are refreshed (`usage_context`).
const CONTEXT_EVERY_S: f64 = 10.0;
/// How often the Usage tab refreshes the session it shows (`usage_session`).
const SESSION_EVERY_S: f64 = 5.0;

/// When each usage tool was last asked (UI time, seconds).
#[derive(Debug, Clone, Default)]
pub struct PollState {
    pub context_at: Option<f64>,
    /// The session the Usage tab last asked about, and when.
    pub session: Option<(String, f64)>,
    /// Settings → Usage asked for the summaries.
    pub summary_asked: bool,
    /// "Forget imported history" was clicked once and asks to be sure.
    pub forget_confirm: bool,
}

pub const API_NOTE: &str = "API-equivalent cost: what these tokens would cost at Anthropic API list prices, not what a subscription bills.";

/// Once a frame: asks `usage_context` for the sidebar chips every few seconds while a Claude
/// session is live (real engine only; the demo's numbers are a fixture).
pub fn poll(app: &mut CsmApp, ctx: &egui::Context) {
    if !app.model.usage.live || !app.model.sessions.iter().any(|s| s.is_claude() && s.status.is_live()) {
        return;
    }
    let now = ctx.input(|i| i.time);
    if app.ui.usage.context_at.is_none_or(|t| now - t >= CONTEXT_EVERY_S) {
        app.ui.usage.context_at = Some(now);
        app.dispatch.submit("usage_context", json!({}));
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(CONTEXT_EVERY_S));
}

/// The colour of a fill level.
fn level_color(t: &Tokens, l: FillLevel) -> egui::Color32 {
    match l {
        FillLevel::Ok => t.green,
        FillLevel::Warn => t.yellow,
        FillLevel::Critical => t.red,
    }
}

/// The chip text for a row `row_w` wide: `74%`, `74% · $18.42`, or nothing when the row is too
/// narrow (the name keeps its room). Pure.
pub fn chip_text(chip: &UsageChip, row_w: f32) -> Option<String> {
    if row_w < size::USAGE_CHIP_MIN_ROW_W {
        return None;
    }
    let pct = chip.context.as_ref().map(|c| format!("{:.0}%", (c.ratio * 100.0).clamp(0.0, 999.0)));
    let cost = (row_w >= size::USAGE_COST_MIN_ROW_W && chip.cost_usd > 0.0).then(|| format_usd(chip.cost_usd));
    match (pct, cost) {
        (Some(p), Some(c)) => Some(format!("{p} · {c}")),
        (Some(p), None) => Some(p),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

/// Paints a Claude row's chip right-aligned at `right` (centre line `cy`) and returns where the
/// next thing to its left may end. The chip is never cut: when its text does not fit between
/// `left` (where the name must still have room) and `right`, the fill alone is tried, then
/// nothing. Nothing either for rows without numbers or too narrow.
pub fn paint_row_chip(ui: &Ui, s: &SessionView, chip: Option<&UsageChip>, row: Rect, left: f32, right: f32, cy: f32) -> f32 {
    let (Some(chip), true) = (chip, s.is_claude()) else { return right };
    let font = theme::mono(text::MONO_SM);
    let room = right - left - size::PANE_NAME_MIN_W;
    let fits = |l: &String| widgets::text_width(ui, l, font.clone()) <= room;
    let label = chip_text(chip, row.width()).filter(fits).or_else(|| {
        // The fill alone (a narrower row's chip).
        let narrow = UsageChip { cost_usd: 0.0, ..chip.clone() };
        chip_text(&narrow, row.width()).filter(fits)
    });
    let Some(label) = label else { return right };
    let t = Tokens::get(ui.ctx());
    let color = chip.context.as_ref().map_or(t.muted, |c| if c.level == FillLevel::Ok { t.muted } else { level_color(&t, c.level) });
    let r = widgets::paint_text(ui, pos2(right, cy), Align2::RIGHT_CENTER, &label, font, color, room.max(0.0));
    r.min.x - space::SM
}

/// A line for a row's tooltip: `Context 74% of 200k · $18.42 API-equivalent`.
pub fn chip_tooltip(chip: &UsageChip) -> String {
    let ctx = chip.context.as_ref().map(|c| format!("Context {:.0}% of {} · ", c.ratio * 100.0, format_tokens(c.window_tokens))).unwrap_or_default();
    let unpriced = if chip.unpriced_requests > 0 { format!(" (+{} unpriced requests)", chip.unpriced_requests) } else { String::new() };
    if chip.cost_usd <= 0.0 && chip.unpriced_requests == 0 {
        // No cost (the Claude usage add-on is off, or nothing priced yet): the fill alone.
        return ctx.trim_end_matches(" · ").to_owned();
    }
    format!("{ctx}{} API-equivalent{unpriced}", format_usd(chip.cost_usd))
}

/// The context gauge: the fill, with the bands where auto-compact gets close.
fn gauge(ui: &mut Ui, ratio: f64, level: FillLevel) {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size::GAUGE_H), Sense::hover());
    let p = ui.painter();
    let rr = CornerRadius::same(radius::SM);
    p.rect_filled(rect, rr, t.surface0);
    let x_at = |r: f64| rect.min.x + rect.width() * (r.clamp(0.0, 1.0) as f32);
    let warn = Rect::from_min_max(pos2(x_at(cu::WARN_RATIO), rect.min.y), pos2(x_at(cu::CRITICAL_RATIO), rect.max.y));
    let crit = Rect::from_min_max(pos2(x_at(cu::CRITICAL_RATIO), rect.min.y), rect.max);
    p.rect_filled(warn, CornerRadius::ZERO, Tokens::tint(t.yellow, alpha::GAUGE_BAND));
    p.rect_filled(crit, CornerRadius { nw: 0, sw: 0, ne: radius::SM, se: radius::SM }, Tokens::tint(t.red, alpha::GAUGE_BAND));
    let fill = Rect::from_min_max(rect.min, pos2(x_at(ratio), rect.max.y));
    p.rect_filled(fill, rr, level_color(&t, level));
    p.vline(x_at(cu::CRITICAL_RATIO), rect.y_range(), Stroke::new(size::HAIRLINE, t.red));
    let label = format!("Context fill {:.0}%", ratio * 100.0);
    resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::ProgressIndicator, true, label.clone()));
}

fn kv(ui: &mut Ui, key: &str, value: &str, detail: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(key).font(theme::regular(text::SMALL)).color(t.muted));
    // The value, and its detail under it (a narrow inspector never cuts it off).
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::XXS;
        ui.label(egui::RichText::new(value).font(semibold(text::SMALL)).color(t.fg));
        if !detail.is_empty() {
            ui.label(egui::RichText::new(detail).font(theme::regular(text::CAPTION)).color(t.muted));
        }
    });
    ui.end_row();
}

fn note(ui: &mut Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(egui::RichText::new(s).font(theme::regular(text::CAPTION)).color(t.muted)).wrap());
}

fn cost_with_unpriced(cost: f64, unpriced: u64) -> String {
    if unpriced > 0 { format!("{} + {unpriced} unpriced", format_usd(cost)) } else { format_usd(cost) }
}

/// The inspector's Usage tab for session `s`.
pub fn tab(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    if !s.is_claude() {
        note(ui, "Usage is read from Claude Code's transcripts: this session doesn't run claude.");
        return;
    }
    let model = app.model.clone();
    let now = ui.input(|i| i.time);
    if model.usage.live {
        let due = match &app.ui.usage.session {
            Some((id, at)) => id != &s.id || now - at >= SESSION_EVERY_S,
            None => true,
        };
        if due {
            app.ui.usage.session = Some((s.id.clone(), now));
            actions.push(Action::Tool("usage_session", json!({"session_id": s.id})));
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(SESSION_EVERY_S));
    }
    if let Some(why) = model.usage.unavailable.get(&s.id) {
        note(ui, why);
        return;
    }
    let Some(v) = model.usage.sessions.get(&s.id) else {
        note(ui, "Reading Claude's transcript…");
        return;
    };
    let u = &v.usage;

    widgets::caps(ui, "Context");
    match &u.context {
        Some(c) => {
            gauge(ui, c.ratio, c.level);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} of {} · {:.0}%", format_tokens(c.fill_tokens), format_tokens(c.window_tokens), c.ratio * 100.0))
                        .font(semibold(text::SMALL))
                        .color(level_color(&t, c.level)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&c.model).font(theme::mono(text::MONO_SM)).color(t.muted));
                });
            });
            let hint = match c.level {
                FillLevel::Ok => "Last main-thread request. The bands mark where auto-compact gets close.".to_string(),
                FillLevel::Warn => "Getting full: Claude auto-compacts near the red band. /compact now keeps what you choose.".to_string(),
                FillLevel::Critical => "Auto-compact is near.".to_string(),
            };
            note(ui, &hint);
        }
        None => note(ui, if v.message.is_empty() { "No Claude requests recorded yet." } else { &v.message }),
    }

    widgets::caps(ui, "API-equivalent cost");
    egui::Grid::new(("usage-cost", s.id.as_str())).num_columns(2).spacing(vec2(space::MD, space::XS + space::XXS)).show(ui, |ui| {
        let tokens = |n: u64| format!("{} tokens", format_tokens(n));
        kv(
            ui,
            "Total",
            &cost_with_unpriced(u.total.cost_usd, u.total.unpriced_requests),
            &format!("{} requests · {}", u.total.requests, tokens(u.total.tokens.total())),
        );
        kv(ui, "Today", &cost_with_unpriced(u.today.cost_usd, u.today.unpriced_requests), &tokens(u.today.tokens.total()));
        kv(
            ui,
            "Subagents",
            &cost_with_unpriced(u.subagents.cost_usd, u.subagents.unpriced_requests),
            &format!("{} requests", u.subagents.requests),
        );
    });
    if !u.total.unpriced_models.is_empty() {
        note(ui, &format!("No price for {}: their requests are counted but not costed.", u.total.unpriced_models.join(", ")));
    }

    widgets::caps(ui, "Cache and compaction");
    egui::Grid::new(("usage-cache", s.id.as_str())).num_columns(2).spacing(vec2(space::MD, space::XS + space::XXS)).show(ui, |ui| {
        let hit = u.cache_hit_ratio.map_or_else(|| "–".to_string(), |r| format!("{:.0}%", r * 100.0));
        kv(ui, "Cache hits", &hit, "of input tokens read from cache");
        match &u.last_cache_miss {
            Some(m) => {
                let gap = m.idle_gap_ms.map(format_gap).map_or_else(String::new, |g| format!("after {g} idle"));
                kv(ui, "Last cache miss", &format!("{} written", format_tokens(m.cache_write)), &gap);
            }
            None => kv(ui, "Last cache miss", "none", ""),
        }
        let last = u.compactions.last.as_ref().map_or_else(String::new, |c| {
            let pre = c.pre_tokens.map_or_else(|| "?".into(), format_tokens);
            let post = c.post_tokens.map_or_else(|| "?".into(), format_tokens);
            format!("last {pre} → {post}{}", c.trigger.as_deref().map(|t| format!(" ({t})")).unwrap_or_default())
        });
        kv(ui, "Compactions", &u.compactions.count.to_string(), &last);
    });
    if v.bad_lines > 0 {
        note(ui, &format!("{} transcript lines could not be read and were skipped.", v.bad_lines));
    }
    note(ui, API_NOTE);
    if model.usage.live && widgets::button(ui, ButtonKind::Secondary, Some("restart"), "Refresh usage", None).clicked() {
        app.ui.usage.session = Some((s.id.clone(), now));
        actions.push(Action::Tool("usage_session", json!({"session_id": s.id})));
    }
    // The dashboard on this session: its cost per model and per day, over any range.
    if widgets::button(ui, ButtonKind::Secondary, Some("gauge"), "Usage by model and day", None).clicked() {
        actions.push(Action::OpenUsage(Some(super::usage_page::Drill::Session { key: s.id.clone(), label: s.name.clone() })));
    }
}

/// Settings → Usage: API-equivalent cost per project, today and this week (`usage_summary`).
pub fn settings(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(space::LG);
    super::settings::section(ui, "Claude usage");
    if !app.model.claude_usage_on() {
        super::settings::caption(
            ui,
            "Claude usage is off. It is a built-in add-on: enable it in Settings › Add-ons for Claude's tokens and API-equivalent cost (the Usage tab, the dashboard, cost per project). The sidebar's context fill works without it.",
        );
        if widgets::button(ui, ButtonKind::Secondary, Some("sliders"), "Go to Add-ons", None).clicked() {
            app.ui.settings.section = crate::app::SettingsSection::Addons;
        }
        return;
    }
    history_controls(ui, app, actions);
    let usage = app.model.usage.clone();
    if usage.live && !app.ui.usage.summary_asked {
        app.ui.usage.summary_asked = true;
        actions.push(Action::Tool("usage_summary", json!({"from": "today", "group_by": "project"})));
        actions.push(Action::Tool("usage_summary", json!({"from": "week", "group_by": "project"})));
    }
    ui.add_space(space::LG);
    super::settings::section(ui, "Claude cost per project");
    super::settings::caption(ui, API_NOTE);
    if let Some(e) = &usage.summary_error {
        super::settings::caption(ui, e);
    }
    let (today, week) = (usage.summary.get("today"), usage.summary.get("week"));
    match week {
        None if usage.summary_error.is_none() => {
            ui.label(egui::RichText::new("Loading…").font(theme::regular(text::BODY)).color(t.muted));
        }
        None => {}
        Some(w) if w.rows.is_empty() => {
            ui.label(egui::RichText::new("No Claude requests recorded this week.").font(theme::regular(text::BODY)).color(t.muted));
        }
        Some(w) => summary_table(ui, today, w),
    }
    if usage.live && widgets::button(ui, ButtonKind::Secondary, Some("restart"), "Refresh cost", None).clicked() {
        app.ui.usage.summary_asked = false;
    }
    if widgets::button(ui, ButtonKind::Secondary, Some("gauge"), "Open the usage dashboard", None).clicked() {
        actions.push(Action::OpenUsage(None));
    }
}

/// "Read past Claude history" (`ClaudeUsageHistory`) and, while it is off, forgetting what it
/// imported (`usage_forget_history`, asked twice: it deletes data).
pub fn history_controls(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    super::settings::bool_setting(
        ui,
        app,
        "ClaudeUsageHistory",
        "Read past Claude history",
        "Also import every past Claude Code transcript on this machine, so the dashboard has your history. Off: only the sessions CodeShellManager runs are counted. Never read in a --clean run.",
        actions,
    );
    if app.ui.settings.bool("ClaudeUsageHistory").unwrap_or(true) {
        app.ui.usage.forget_confirm = false;
        return;
    }
    if !app.ui.usage.forget_confirm {
        if widgets::button(ui, ButtonKind::Secondary, Some("trash"), "Forget imported history…", None).clicked() {
            app.ui.usage.forget_confirm = true;
        }
        return;
    }
    super::settings::caption(ui, "Delete the usage imported from past transcripts? What the sessions CodeShellManager ran recorded stays.");
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Danger, Some("trash"), "Forget imported history", None).clicked() {
            app.ui.usage.forget_confirm = false;
            app.ui.usage.summary_asked = false;
            actions.push(Action::Tool("usage_forget_history", json!({})));
        }
        if widgets::button(ui, ButtonKind::Ghost, None, "Cancel", None).clicked() {
            app.ui.usage.forget_confirm = false;
        }
    });
}

fn summary_table(ui: &mut Ui, today: Option<&SummaryView>, week: &SummaryView) {
    let t = Tokens::get(ui.ctx());
    widgets::card_frame(&t).inner_margin(egui::Margin::same(space::MD as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("usage-summary").num_columns(3).spacing(vec2(space::XL, space::SM)).show(ui, |ui| {
            for h in ["Project", "Today", "This week"] {
                widgets::caps(ui, h);
            }
            ui.end_row();
            for r in &week.rows {
                let day = today.and_then(|d| d.rows.iter().find(|x| x.label == r.label));
                ui.label(egui::RichText::new(&r.label).font(theme::mono(text::MONO)).color(t.fg));
                let d = day.map_or_else(|| "–".to_string(), |d| cost_with_unpriced(d.cost_usd, d.unpriced_requests));
                ui.label(egui::RichText::new(d).font(theme::regular(text::BODY)).color(t.fg));
                ui.label(egui::RichText::new(cost_with_unpriced(r.cost_usd, r.unpriced_requests)).font(theme::regular(text::BODY)).color(t.fg))
                    .on_hover_text(format!("{} requests · {} tokens", r.requests, format_tokens(r.tokens)));
                ui.end_row();
            }
            widgets::caps(ui, "Total");
            ui.label(egui::RichText::new(today.map_or_else(|| "–".to_string(), |d| format_usd(d.total_cost))).font(semibold(text::BODY)).color(t.fg));
            ui.label(egui::RichText::new(format_usd(week.total_cost)).font(semibold(text::BODY)).color(t.fg));
            ui.end_row();
        });
    });
    if !week.unpriced_models.is_empty() {
        super::settings::caption(ui, &format!("No price for {}: counted, not costed.", week.unpriced_models.join(", ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_core::claude_usage::ContextFill;

    #[test]
    fn the_chip_gives_way_on_narrow_rows() {
        let chip = UsageChip { context: Some(ContextFill::new(148_000, 200_000, "m".into(), None)), cost_usd: 18.42, unpriced_requests: 0 };
        assert_eq!(chip_text(&chip, size::USAGE_CHIP_MIN_ROW_W - 1.0), None);
        assert_eq!(chip_text(&chip, size::USAGE_CHIP_MIN_ROW_W).as_deref(), Some("74%"));
        assert_eq!(chip_text(&chip, size::USAGE_COST_MIN_ROW_W).as_deref(), Some("74% · $18.42"));
        let cost_only = UsageChip { context: None, cost_usd: 0.5, unpriced_requests: 0 };
        assert_eq!(chip_text(&cost_only, size::USAGE_COST_MIN_ROW_W).as_deref(), Some("$0.50"));
        assert_eq!(chip_text(&UsageChip::default(), 1000.0), None);
        assert_eq!(chip_tooltip(&chip), "Context 74% of 200k · $18.42 API-equivalent");
        let un = UsageChip { unpriced_requests: 2, ..cost_only };
        assert!(chip_tooltip(&un).contains("2 unpriced"));
    }

    #[test]
    fn unpriced_costs_say_so() {
        assert_eq!(cost_with_unpriced(1.5, 0), "$1.50");
        assert_eq!(cost_with_unpriced(1.5, 3), "$1.50 + 3 unpriced");
    }
}

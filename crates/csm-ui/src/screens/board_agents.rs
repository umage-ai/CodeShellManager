//! The subagent rows under a session's card on the Board: a compact list of what the session's
//! Claude has running (label, model, tokens and cost, last activity), workflow runs grouped with
//! their phase progress ("Screens core 2/4"), the running ones highlighted. Finished ones fade
//! for [`FADE_MS`], then collapse into a "+N done" row.
//!
//! [`rows`] is pure (tested); [`paint`] draws them. The data is `csm_core::subagents`, read by the
//! engine from Claude Code's own files; the same data is the `session_subagents` tool.

use csm_core::claude_usage::{format_tokens, format_usd};
use csm_core::subagents::{AgentState, Subagent, SubagentsReport, WorkflowRun};
use egui::{Align2, CornerRadius, Pos2, Rect, Ui};

use crate::theme::{self, Tokens, alpha, radius, size, space, text};
use crate::widgets;

/// A finished agent stays listed (faded) this long after its last activity.
pub const FADE_MS: u64 = 120_000;
/// Most rows under a card, including the closing "+N" row.
pub const MAX_ROWS: usize = 6;

/// One row under a card.
#[derive(Debug, Clone, PartialEq)]
pub enum Row<'a> {
    /// A workflow run: its active phase and the run's overall progress.
    Run { phase: String, done: usize, total: usize, run_done: usize, run_total: usize, running: usize },
    Agent { agent: &'a Subagent, nested: bool, fading: bool },
    /// Finished agents no longer listed (`+N done`).
    Done(usize),
    /// Rows past the cap (`+N more`).
    More(usize),
}

fn active_phase(run: &WorkflowRun) -> (String, usize, usize) {
    let p = run.phases.iter().find(|p| p.running > 0).or_else(|| run.phases.iter().find(|p| p.done < p.total)).or_else(|| run.phases.last());
    match p {
        Some(p) => (p.name.clone(), p.done, p.total),
        None => (String::new(), run.done, run.total),
    }
}

fn visible(a: &Subagent, now_ms: u64) -> bool {
    match a.state {
        AgentState::Running | AgentState::Stale => true,
        AgentState::Done => a.last_ms.and_then(|t| u64::try_from(t).ok()).is_some_and(|t| now_ms.saturating_sub(t) < FADE_MS),
    }
}

/// The rows to draw under a card.
pub fn rows(r: &SubagentsReport, now_ms: u64) -> Vec<Row<'_>> {
    let mut out: Vec<Row<'_>> = Vec::new();
    let mut hidden_done = r.omitted;
    for a in &r.agents {
        if !visible(a, now_ms) {
            hidden_done += 1;
        }
    }
    let fading = |a: &Subagent| a.state == AgentState::Done;
    for run in &r.runs {
        let mine: Vec<&Subagent> = r.agents.iter().filter(|a| a.run.as_deref() == Some(run.id.as_str()) && visible(a, now_ms)).collect();
        if run.running == 0 && mine.is_empty() {
            continue;
        }
        let (phase, done, total) = active_phase(run);
        out.push(Row::Run { phase, done, total, run_done: run.done, run_total: run.total, running: run.running });
        out.extend(mine.into_iter().map(|a| Row::Agent { agent: a, nested: true, fading: fading(a) }));
    }
    for a in r.agents.iter().filter(|a| visible(a, now_ms)) {
        if !a.run.as_deref().is_some_and(|id| r.runs.iter().any(|x| x.id == id)) {
            out.push(Row::Agent { agent: a, nested: false, fading: fading(a) });
        }
    }
    if out.len() >= MAX_ROWS {
        let extra = out.len() - (MAX_ROWS - 1);
        out.truncate(MAX_ROWS - 1);
        out.push(Row::More(extra + hidden_done));
    } else if hidden_done > 0 {
        out.push(Row::Done(hidden_done));
    }
    out
}

/// Height the rows take under a card (0 when none).
pub fn height(n: usize) -> f32 {
    if n == 0 { 0.0 } else { n as f32 * size::BOARD_LINE + space::XS }
}

/// "12s", "4m", "2h".
pub fn short_age(now_ms: u64, at_ms: Option<i64>) -> String {
    let Some(t) = at_ms.and_then(|t| u64::try_from(t).ok()) else { return String::new() };
    let s = now_ms.saturating_sub(t) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// The model without Claude's prefix, short.
pub fn short_model(m: &str) -> String {
    let m = m.strip_prefix("claude-").unwrap_or(m);
    m.chars().take(14).collect()
}

/// The right-hand text of an agent row, from most to least detailed.
fn right_text(a: &Subagent, now_ms: u64) -> [String; 3] {
    let mut nums = Vec::new();
    if a.tokens > 0 {
        nums.push(format_tokens(a.tokens));
    }
    if let Some(c) = a.cost_usd {
        nums.push(format_usd(c));
    }
    let age = short_age(now_ms, a.last_ms);
    let tail = |extra: &str| {
        let mut v: Vec<String> = Vec::new();
        if !extra.is_empty() {
            v.push(extra.to_owned());
        }
        v.extend(nums.iter().cloned());
        if !age.is_empty() {
            v.push(age.clone());
        }
        v.join(" · ")
    };
    let model = a.model.as_deref().map(short_model).unwrap_or_default();
    [tail(&model), tail(""), age.clone()]
}

/// Paints the rows from `top` between `x0` and `x1`.
pub fn paint(ui: &Ui, rows: &[Row<'_>], top: f32, x0: f32, x1: f32, now_ms: u64) {
    let t = Tokens::get(ui.ctx());
    for (i, row) in rows.iter().enumerate() {
        let y = top + size::BOARD_LINE * i as f32;
        let mid = y + size::BOARD_LINE / 2.0;
        match row {
            Row::Run { phase, done, total, run_done, run_total, running } => {
                let colour = if *running > 0 { t.mauve } else { t.muted };
                let right = format!("run {run_done}/{run_total}");
                let rw = widgets::text_width(ui, &right, theme::regular(text::CAPTION));
                ui.painter().text(Pos2::new(x1, mid), Align2::RIGHT_CENTER, &right, theme::regular(text::CAPTION), t.faint);
                let label = if phase.is_empty() { "Workflow".to_owned() } else { format!("{phase} {done}/{total}") };
                widgets::paint_text(ui, Pos2::new(x0, mid), Align2::LEFT_CENTER, &label, theme::medium(text::CAPTION), colour, x1 - x0 - rw - space::SM);
            }
            Row::Agent { agent, nested, fading } => {
                let indent = if *nested { space::MD } else { 0.0 };
                let running = agent.state == AgentState::Running;
                let band = Rect::from_min_max(Pos2::new(x0 + indent - space::XS, y + 1.0), Pos2::new(x1 + space::XS, y + size::BOARD_LINE - 1.0));
                if running {
                    ui.painter().rect_filled(band, CornerRadius::same(radius::sm()), Tokens::tint(t.st_working, alpha::SOFT));
                }
                let fade = |c| if *fading { Tokens::tint(c, alpha::ASLEEP) } else { c };
                let dot = match agent.state {
                    AgentState::Running => t.st_working,
                    AgentState::Stale => t.yellow,
                    AgentState::Done => t.faint,
                };
                ui.painter().circle_filled(Pos2::new(x0 + indent + space::XS, mid), space::XS - 1.0, fade(dot));
                let lx = x0 + indent + space::SM + space::XS;
                let candidates = right_text(agent, now_ms);
                let font = theme::regular(text::CAPTION);
                let want = |s: &str| widgets::text_width(ui, s, font.clone());
                let label_min = 64.0;
                let right = candidates.iter().find(|c| x1 - want(c) - space::SM - lx >= label_min).or(candidates.last()).cloned().unwrap_or_default();
                let rw = want(&right);
                let fg = match agent.state {
                    AgentState::Running => t.fg,
                    AgentState::Stale => t.fg2,
                    AgentState::Done => t.muted,
                };
                ui.painter().text(Pos2::new(x1, mid), Align2::RIGHT_CENTER, &right, font.clone(), fade(if running { t.fg2 } else { t.faint }));
                widgets::paint_text(ui, Pos2::new(lx, mid), Align2::LEFT_CENTER, &agent.label, font, fade(fg), x1 - rw - space::SM - lx);
            }
            Row::Done(n) => {
                ui.painter().text(Pos2::new(x0 + space::SM, mid), Align2::LEFT_CENTER, format!("+{n} done"), theme::regular(text::CAPTION), t.faint);
            }
            Row::More(n) => {
                ui.painter().text(Pos2::new(x0 + space::SM, mid), Align2::LEFT_CENTER, format!("+{n} more"), theme::regular(text::CAPTION), t.faint);
            }
        }
    }
}

/// A short badge for a session's tile or row: "3 agents" while any run, "2 done" after. Empty
/// without any.
pub fn badge(r: &SubagentsReport) -> String {
    match (r.running, r.total()) {
        (_, 0) => String::new(),
        (0, n) => format!("{n} done"),
        (1, _) => "1 agent".to_owned(),
        (n, _) => format!("{n} agents"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_core::subagents::PhaseProgress;

    fn ag(id: &str, run: Option<&str>, state: AgentState, last: i64) -> Subagent {
        Subagent {
            id: id.into(),
            label: id.into(),
            kind: "k".into(),
            model: Some("claude-opus-5-5".into()),
            run: run.map(Into::into),
            phase: None,
            state,
            started_ms: None,
            last_ms: Some(last),
            requests: 1,
            tokens: 1500,
            cost_usd: Some(0.5),
        }
    }

    fn rep(agents: Vec<Subagent>, runs: Vec<WorkflowRun>) -> SubagentsReport {
        let running = agents.iter().filter(|a| a.state == AgentState::Running).count();
        let done = agents.iter().filter(|a| a.state == AgentState::Done).count();
        SubagentsReport { stale: agents.len() - running - done, running, done, agents, runs, ..SubagentsReport::default() }
    }

    #[test]
    fn running_listed_finished_fade_then_collapse() {
        let now = 10_000_000_u64;
        let r = rep(
            vec![
                ag("r", None, AgentState::Running, now as i64 - 1000),
                ag("fresh", None, AgentState::Done, now as i64 - 10_000),
                ag("old1", None, AgentState::Done, now as i64 - 600_000),
                ag("old2", None, AgentState::Done, now as i64 - 700_000),
            ],
            vec![],
        );
        let rows = rows(&r, now);
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], Row::Agent { agent, fading: false, nested: false } if agent.id == "r"));
        assert!(matches!(&rows[1], Row::Agent { agent, fading: true, .. } if agent.id == "fresh"));
        assert_eq!(rows[2], Row::Done(2));
    }

    #[test]
    fn only_finished_old_agents_give_just_the_done_row() {
        let now = 10_000_000_u64;
        let r = rep(vec![ag("a", None, AgentState::Done, 1), ag("b", None, AgentState::Done, 2)], vec![]);
        assert_eq!(rows(&r, now), vec![Row::Done(2)]);
        assert!(rows(&SubagentsReport::default(), now).is_empty());
    }

    #[test]
    fn workflow_run_shows_active_phase_progress_and_nests_its_agents() {
        let now = 10_000_000_u64;
        let run = WorkflowRun {
            id: "wf".into(),
            phases: vec![
                PhaseProgress { name: "Plan".into(), done: 2, total: 2, running: 0 },
                PhaseProgress { name: "Screens core".into(), done: 2, total: 4, running: 2 },
            ],
            done: 4,
            total: 6,
            running: 2,
            last_ms: Some(now as i64),
        };
        let r = rep(vec![ag("w1", Some("wf"), AgentState::Running, now as i64), ag("solo", None, AgentState::Stale, now as i64 - 400_000)], vec![run]);
        let rows = rows(&r, now);
        assert_eq!(rows[0], Row::Run { phase: "Screens core".into(), done: 2, total: 4, run_done: 4, run_total: 6, running: 2 });
        assert!(matches!(&rows[1], Row::Agent { nested: true, .. }));
        assert!(matches!(&rows[2], Row::Agent { nested: false, agent, .. } if agent.id == "solo"));
    }

    #[test]
    fn rows_are_capped_with_a_more_row() {
        let now = 10_000_000_u64;
        let agents: Vec<Subagent> = (0..9).map(|i| ag(&format!("a{i}"), None, AgentState::Running, now as i64)).collect();
        let r = rep(agents, vec![]);
        let rows = rows(&r, now);
        assert_eq!(rows.len(), MAX_ROWS);
        assert_eq!(rows[MAX_ROWS - 1], Row::More(9 - (MAX_ROWS - 1)));
    }

    #[test]
    fn small_helpers() {
        assert_eq!(short_age(100_000, Some(88_000)), "12s");
        assert_eq!(short_age(1_000_000, Some(100_000)), "15m");
        assert_eq!(short_age(1, None), "");
        assert_eq!(short_model("claude-opus-5-5"), "opus-5-5");
        let r = rep(vec![ag("a", None, AgentState::Running, 1), ag("b", None, AgentState::Running, 1)], vec![]);
        assert_eq!(badge(&r), "2 agents");
        assert_eq!(badge(&SubagentsReport::default()), "");
    }
}

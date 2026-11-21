//! The Board with Claude subagents under a session's card: the card grows by the rows, the
//! running ones are listed, finished ones collapse into "+N done"; both themes render (PNGs in
//! `$CSM_SCREENS_OUT` or `<target>/screens/`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::*;
use csm_core::subagents::{AgentState, PhaseProgress, Subagent, SubagentsReport, WorkflowRun};
use csm_ui::demo::DemoBackend;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn agent(id: &str, label: &str, run: Option<&str>, state: AgentState, ago_s: i64, tokens: u64, cost: f64) -> Subagent {
    Subagent {
        id: id.into(),
        label: label.into(),
        kind: if run.is_some() { "workflow-subagent" } else { "Explore" }.into(),
        model: Some(if run.is_some() { "sonnet" } else { "claude-opus-5-5" }.into()),
        run: run.map(Into::into),
        phase: run.map(|_| "Screens core".into()),
        state,
        started_ms: Some(NOW_MS as i64 - ago_s * 1000 - 60_000),
        last_ms: Some(NOW_MS as i64 - ago_s * 1000),
        requests: 4,
        tokens,
        cost_usd: Some(cost),
    }
}

fn workflow_report() -> SubagentsReport {
    let agents = vec![
        agent("w1", "impl:core", Some("wf_a"), AgentState::Running, 3, 182_000, 0.84),
        agent("w2", "review:core:1", Some("wf_a"), AgentState::Running, 9, 41_000, 0.12),
        agent("w3", "review:core:2", Some("wf_a"), AgentState::Done, 40, 38_000, 0.1),
        agent("x1", "Inventory WPF features", None, AgentState::Stale, 400, 90_000, 0.4),
        agent("old", "older finished", None, AgentState::Done, 900, 5_000, 0.02),
    ];
    let runs = vec![WorkflowRun {
        id: "wf_a".into(),
        phases: vec![
            PhaseProgress { name: "Plan".into(), done: 2, total: 2, running: 0 },
            PhaseProgress { name: "Screens core".into(), done: 1, total: 4, running: 2 },
        ],
        done: 3,
        total: 6,
        running: 2,
        last_ms: Some(NOW_MS as i64 - 3000),
    }];
    SubagentsReport { agents, runs, omitted: 0, running: 2, stale: 1, done: 2, total_tokens: 356_000, total_cost_usd: 1.48 }
}

fn board(theme: ThemeKind, with: bool) -> DemoBackend {
    let mut b = backend(theme);
    if with {
        for s in &mut b.model_mut().sessions {
            if s.id == "web" {
                s.subagents = Some(Arc::new(workflow_report()));
            }
        }
    }
    b.publish();
    b
}

fn on_board(b: DemoBackend, size: egui::Vec2) -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness_wgpu_sized(app_with(b), 1.0, size);
    settle(&mut h);
    h.get_by_label("Layout: Board").click();
    settle(&mut h);
    h
}

#[test]
fn a_card_grows_by_its_subagent_rows_and_both_themes_render() {
    let size = egui::vec2(1600.0, 900.0);
    let plain = on_board(board(ThemeKind::Mocha, false), size);
    let base = plain.get_by_label("Agent: Odulodo.Web").rect().height();
    drop(plain);
    for (name, theme) in [("19-board-subagents", ThemeKind::Mocha), ("19-board-subagents-latte", ThemeKind::Latte)] {
        let mut h = on_board(board(theme, true), size);
        let with = h.get_by_label("Agent: Odulodo.Web").rect().height();
        // run row + w1 + w2 + w3 (fading) + the stale one + "+1 done" = 6 rows (the cap).
        let rows = (with - base - csm_ui::theme::space::XS) / csm_ui::theme::size::BOARD_LINE;
        assert!((rows - 6.0).abs() < 0.01, "{name}: card grew by {rows} rows");
        // Other cards are untouched.
        assert!((h.get_by_label("Agent: Odulodo").rect().height() - base).abs() < 0.01);
        let img = h.render().expect("render");
        let dir = std::env::var_os("CSM_SCREENS_OUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("screens"));
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
        let distinct: std::collections::HashSet<[u8; 4]> = img.pixels().map(|p| p.0).collect();
        assert!(distinct.len() > 100, "{name} looks blank");
    }
}

//! The subagents of a Claude session, as the Board and the `session_subagents` tool show them.
//!
//! Claude Code writes, next to each subagent's transcript `agent-<id>.jsonl`, an
//! `agent-<id>.meta.json` (what it is for, its type, model, whether it finished) and, in a
//! dynamic workflow's run folder, a `journal.jsonl` (which agents started in which phase, which
//! returned a result). **None of that is a documented API.** Everything here is parsed
//! leniently: a missing, odd or hostile file shows less, never fails; texts are cleaned and
//! capped; counts are capped. The reading of the disk is in `csm_services::claude_usage::subagents`;
//! this module is pure.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A subagent with no transcript growth for this long and no "finished" mark is stale (idle).
pub const STALE_MS: i64 = 5 * 60_000;
/// Agents listed in a report; the rest are only counted (`omitted`).
pub const MAX_AGENTS: usize = 48;
/// Phases listed per run, and runs per report.
pub const MAX_PHASES: usize = 24;
pub const MAX_RUNS: usize = 8;
/// Longest text kept from a file written by Claude Code, in characters.
pub const MAX_TEXT: usize = 120;
/// Journal lines read; the rest are ignored.
pub const MAX_JOURNAL_LINES: usize = 5000;

/// A text from a file we do not own: control characters dropped, whitespace folded, capped.
pub fn clean_text(s: &str) -> String {
    let mut out = String::new();
    let mut n = 0;
    let mut space = false;
    for c in s.chars() {
        if n >= MAX_TEXT {
            out.push('…');
            break;
        }
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if c.is_control() {
            continue;
        }
        if space {
            out.push(' ');
            n += 1;
            space = false;
        }
        out.push(c);
        n += 1;
    }
    out
}

fn text_field(v: &Value, key: &str) -> Option<String> {
    let s = clean_text(v.get(key)?.as_str()?);
    (!s.is_empty()).then_some(s)
}

/// What `agent-<id>.meta.json` says. Every field optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetaInfo {
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub phase: Option<String>,
    pub model: Option<String>,
    pub finished: Option<bool>,
    pub in_workflow: bool,
    pub spawn_depth: Option<u32>,
}

/// Parses a meta file. Anything that is not a JSON object gives the empty [`MetaInfo`].
pub fn parse_meta(text: &str) -> MetaInfo {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return MetaInfo::default() };
    if !v.is_object() {
        return MetaInfo::default();
    }
    MetaInfo {
        agent_type: text_field(&v, "agentType"),
        description: text_field(&v, "description"),
        phase: text_field(&v, "workflowPhase"),
        model: text_field(&v, "model"),
        finished: v.get("finished").and_then(Value::as_bool),
        in_workflow: v.get("inWorkflowRun").and_then(Value::as_bool).unwrap_or(false),
        spawn_depth: v.get("spawnDepth").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()),
    }
}

/// An agent a workflow journal says started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalStart {
    pub agent_id: String,
    pub label: Option<String>,
    pub phase: Option<String>,
}

/// What a workflow run's `journal.jsonl` says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Journal {
    pub launched: bool,
    pub started: Vec<JournalStart>,
    /// Agents that returned a result.
    pub results: Vec<String>,
    pub bad_lines: usize,
}

/// Parses a journal; bad lines are counted and skipped.
pub fn parse_journal(text: &str) -> Journal {
    let mut j = Journal::default();
    for line in text.lines().take(MAX_JOURNAL_LINES) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            j.bad_lines += 1;
            continue;
        };
        let agent = text_field(&v, "agentId");
        match v.get("type").and_then(Value::as_str) {
            Some("launched") => j.launched = true,
            Some("started") => {
                if let Some(agent_id) = agent
                    && !j.started.iter().any(|s| s.agent_id == agent_id)
                {
                    j.started.push(JournalStart { agent_id, label: text_field(&v, "label"), phase: text_field(&v, "phase") });
                }
            }
            Some("result") => {
                if let Some(a) = agent
                    && !j.results.contains(&a)
                {
                    j.results.push(a);
                }
            }
            _ => {}
        }
    }
    j
}

/// What the usage index knows of one subagent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentUsage {
    pub requests: u64,
    pub tokens: u64,
    /// `None` when no request of it could be priced.
    pub cost_usd: Option<f64>,
    pub unpriced: u64,
    pub model: Option<String>,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
}

/// One subagent as the disk and the index describe it, before [`derive`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentInput {
    pub id: String,
    /// The workflow run folder it sits in (`subagents/workflows/<run>/`).
    pub run: Option<String>,
    pub meta: MetaInfo,
    /// Transcript's last modification.
    pub mtime_ms: Option<i64>,
    pub usage: AgentUsage,
}

/// One workflow run folder and its journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunInput {
    pub id: String,
    pub journal: Journal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Running,
    /// No transcript growth for [`STALE_MS`] and not marked finished.
    Stale,
    Done,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Running => "running",
            AgentState::Stale => "idle",
            AgentState::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subagent {
    pub id: String,
    /// Its description (`review:core:2`, "Inventory WPF features").
    pub label: String,
    /// `workflow-subagent`, `Explore`, `general-purpose`, …
    pub kind: String,
    pub model: Option<String>,
    pub run: Option<String>,
    pub phase: Option<String>,
    pub state: AgentState,
    pub started_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub requests: u64,
    pub tokens: u64,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseProgress {
    pub name: String,
    pub done: usize,
    pub total: usize,
    pub running: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: String,
    pub phases: Vec<PhaseProgress>,
    pub done: usize,
    pub total: usize,
    pub running: usize,
    /// Latest activity of any of its agents.
    pub last_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubagentsReport {
    /// Running first, then idle, then done (newest first within each); at most [`MAX_AGENTS`].
    pub agents: Vec<Subagent>,
    /// Workflow runs, most recently active first.
    pub runs: Vec<WorkflowRun>,
    /// Agents not listed (past the cap).
    pub omitted: usize,
    pub running: usize,
    pub stale: usize,
    pub done: usize,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
}

impl SubagentsReport {
    pub fn total(&self) -> usize {
        self.running + self.stale + self.done
    }
}

/// Short form of an agent id for a label that has nothing else.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Builds the report. Pure; `now_ms` only decides stale.
pub fn derive(agents: Vec<AgentInput>, runs: Vec<RunInput>, now_ms: i64) -> SubagentsReport {
    // Results by agent id, from every run's journal (agent ids are unique per conversation).
    let resulted: HashSet<&str> = runs.iter().flat_map(|r| r.journal.results.iter().map(String::as_str)).collect();
    let journal_start: HashMap<&str, &JournalStart> = runs.iter().flat_map(|r| r.journal.started.iter().map(|s| (s.agent_id.as_str(), s))).collect();

    let mut out: Vec<Subagent> = Vec::new();
    for a in &agents {
        let js = journal_start.get(a.id.as_str());
        let last = [a.mtime_ms, a.usage.last_ms].into_iter().flatten().max();
        let done = a.meta.finished == Some(true) || resulted.contains(a.id.as_str());
        let state = if done {
            AgentState::Done
        } else if last.is_some_and(|l| now_ms.saturating_sub(l) > STALE_MS) {
            AgentState::Stale
        } else {
            AgentState::Running
        };
        out.push(Subagent {
            id: clean_text(&a.id),
            label: a.meta.description.clone().or_else(|| js.and_then(|s| s.label.clone())).unwrap_or_else(|| short_id(&a.id)),
            kind: a.meta.agent_type.clone().unwrap_or_else(|| "agent".into()),
            model: a.meta.model.clone().or_else(|| a.usage.model.clone()),
            run: a.run.clone(),
            phase: a.meta.phase.clone().or_else(|| js.and_then(|s| s.phase.clone())),
            state,
            started_ms: a.usage.first_ms,
            last_ms: last,
            requests: a.usage.requests,
            tokens: a.usage.tokens,
            cost_usd: a.usage.cost_usd,
        });
    }

    // Runs and phases: journal order first, then agents by start time.
    let mut run_out: Vec<WorkflowRun> = Vec::new();
    let mut run_ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
    for a in &out {
        if let Some(r) = a.run.as_deref()
            && !run_ids.contains(&r)
        {
            run_ids.push(r);
        }
    }
    for rid in run_ids {
        let journal = runs.iter().find(|r| r.id == rid).map(|r| &r.journal);
        let mine: Vec<&Subagent> = out.iter().filter(|a| a.run.as_deref() == Some(rid)).collect();
        // Every agent of the run, from the disk or only the journal: (id, phase, state).
        let mut members: Vec<(String, Option<String>, AgentState)> = Vec::new();
        if let Some(j) = journal {
            for s in &j.started {
                if !mine.iter().any(|a| a.id == s.agent_id) {
                    let st = if j.results.contains(&s.agent_id) { AgentState::Done } else { AgentState::Running };
                    members.push((s.agent_id.clone(), s.phase.clone(), st));
                }
            }
        }
        let mut disk: Vec<&&Subagent> = mine.iter().collect();
        disk.sort_by_key(|a| (a.started_ms.unwrap_or(i64::MAX), a.id.clone()));
        for a in disk {
            members.push((a.id.clone(), a.phase.clone(), a.state));
        }
        let mut order: Vec<Option<String>> = Vec::new();
        if let Some(j) = journal {
            for s in &j.started {
                if !order.contains(&s.phase) {
                    order.push(s.phase.clone());
                }
            }
        }
        for (_, p, _) in &members {
            if !order.contains(p) {
                order.push(p.clone());
            }
        }
        let phases: Vec<PhaseProgress> = order
            .iter()
            .take(MAX_PHASES)
            .map(|p| {
                let ms: Vec<&(String, Option<String>, AgentState)> = members.iter().filter(|m| &m.1 == p).collect();
                PhaseProgress {
                    name: p.clone().unwrap_or_else(|| "(no phase)".into()),
                    done: ms.iter().filter(|m| m.2 == AgentState::Done).count(),
                    total: ms.len(),
                    running: ms.iter().filter(|m| m.2 == AgentState::Running).count(),
                }
            })
            .collect();
        if members.is_empty() {
            continue;
        }
        run_out.push(WorkflowRun {
            id: clean_text(rid),
            phases,
            done: members.iter().filter(|m| m.2 == AgentState::Done).count(),
            total: members.len(),
            running: members.iter().filter(|m| m.2 == AgentState::Running).count(),
            last_ms: mine.iter().filter_map(|a| a.last_ms).max(),
        });
    }
    run_out.sort_by_key(|r| std::cmp::Reverse(r.last_ms.unwrap_or(i64::MIN)));
    run_out.truncate(MAX_RUNS);

    let mut rep = SubagentsReport { runs: run_out, ..SubagentsReport::default() };
    for a in &out {
        match a.state {
            AgentState::Running => rep.running += 1,
            AgentState::Stale => rep.stale += 1,
            AgentState::Done => rep.done += 1,
        }
        rep.total_tokens = rep.total_tokens.saturating_add(a.tokens);
        rep.total_cost_usd += a.cost_usd.unwrap_or(0.0);
    }
    let rank = |s: AgentState| match s {
        AgentState::Running => 0,
        AgentState::Stale => 1,
        AgentState::Done => 2,
    };
    out.sort_by(|a, b| rank(a.state).cmp(&rank(b.state)).then(b.last_ms.cmp(&a.last_ms)).then(a.id.cmp(&b.id)));
    rep.omitted = out.len().saturating_sub(MAX_AGENTS);
    out.truncate(MAX_AGENTS);
    rep.agents = out;
    rep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_of_a_workflow_agent() {
        let m = parse_meta(r#"{"agentType":"workflow-subagent","description":"review:core:2","workflowPhase":"Screens core","spawnDepth":1,"model":"haiku","inWorkflowRun":true}"#);
        assert_eq!(m.agent_type.as_deref(), Some("workflow-subagent"));
        assert_eq!(m.description.as_deref(), Some("review:core:2"));
        assert_eq!(m.phase.as_deref(), Some("Screens core"));
        assert_eq!(m.model.as_deref(), Some("haiku"));
        assert!(m.in_workflow);
        assert_eq!(m.spawn_depth, Some(1));
        assert_eq!(m.finished, None);
    }

    #[test]
    fn meta_of_an_explore_agent_that_finished() {
        let m = parse_meta(r#"{"agentType":"Explore","description":"Inventory","model":"claude-opus-5-5","finished":true}"#);
        assert_eq!(m.finished, Some(true));
        assert!(!m.in_workflow);
    }

    #[test]
    fn hostile_meta_shows_less_and_never_fails() {
        for bad in ["", "null", "[]", "42", "\"x\"", "{", "{\"description\": 5, \"finished\": \"yes\", \"spawnDepth\": -3, \"model\": {}}", "\u{0}\u{1}"] {
            let m = parse_meta(bad);
            assert_eq!(m.finished, None, "{bad}");
            assert_eq!(m.description, None, "{bad}");
            assert_eq!(m.spawn_depth, None, "{bad}");
        }
        let long = format!("{{\"description\":\"{}\\u0007\\n  a\"}}", "x".repeat(10_000));
        let m = parse_meta(&long);
        assert!(m.description.unwrap().chars().count() <= MAX_TEXT + 1);
    }

    #[test]
    fn clean_text_folds_and_strips() {
        assert_eq!(clean_text("  a \n\t b\u{7}c  "), "a bc");
        assert_eq!(clean_text("\u{1b}[31mred"), "[31mred");
        assert_eq!(clean_text(""), "");
    }

    #[test]
    fn journal_is_read_leniently() {
        let j = parse_journal(
            "{\"type\":\"launched\"}\nnot json\n{\"type\":\"started\",\"agentId\":\"a1\",\"label\":\"impl:core\",\"phase\":\"P1\"}\n\
             {\"type\":\"started\",\"agentId\":\"a1\",\"label\":\"dup\"}\n{\"type\":\"started\"}\n{\"type\":\"started\",\"agentId\":5}\n\
             {\"type\":\"result\",\"agentId\":\"a1\",\"result\":{\"summary\":\"big\"}}\n{\"type\":\"weird\"}\n[1,2]\n\n",
        );
        assert!(j.launched);
        assert_eq!(j.bad_lines, 1);
        assert_eq!(j.started, vec![JournalStart { agent_id: "a1".into(), label: Some("impl:core".into()), phase: Some("P1".into()) }]);
        assert_eq!(j.results, vec!["a1".to_string()]);
    }

    #[test]
    fn journal_line_count_is_capped() {
        let text: String = (0..MAX_JOURNAL_LINES + 500).map(|i| format!("{{\"type\":\"started\",\"agentId\":\"a{i}\"}}\n")).collect();
        assert_eq!(parse_journal(&text).started.len(), MAX_JOURNAL_LINES);
    }

    fn agent(id: &str, run: Option<&str>, desc: &str, phase: Option<&str>, last: i64) -> AgentInput {
        AgentInput {
            id: id.into(),
            run: run.map(Into::into),
            meta: MetaInfo { description: Some(desc.into()), phase: phase.map(Into::into), ..MetaInfo::default() },
            mtime_ms: Some(last),
            usage: AgentUsage { requests: 2, tokens: 1000, cost_usd: Some(0.5), first_ms: Some(last - 1000), last_ms: Some(last), ..AgentUsage::default() },
        }
    }

    #[test]
    fn state_comes_from_finished_result_or_silence() {
        let now = 10_000_000;
        let mut fin = agent("a", None, "fin", None, now);
        fin.meta.finished = Some(true);
        let res = agent("b", Some("wf1"), "res", Some("P"), now);
        let old = agent("c", None, "old", None, now - STALE_MS - 1);
        let live = agent("d", None, "live", None, now - 1000);
        let runs = vec![RunInput { id: "wf1".into(), journal: Journal { results: vec!["b".into()], ..Journal::default() } }];
        let r = derive(vec![fin, res, old, live], runs, now);
        let by = |id: &str| r.agents.iter().find(|a| a.id == id).unwrap().state;
        assert_eq!(by("a"), AgentState::Done);
        assert_eq!(by("b"), AgentState::Done);
        assert_eq!(by("c"), AgentState::Stale);
        assert_eq!(by("d"), AgentState::Running);
        assert_eq!((r.running, r.stale, r.done), (1, 1, 2));
        // Running first, done last.
        assert_eq!(r.agents[0].id, "d");
        assert_eq!(r.agents[1].id, "c");
        assert_eq!(r.total_tokens, 4000);
        assert!((r.total_cost_usd - 2.0).abs() < 1e-9);
    }

    #[test]
    fn workflow_runs_group_by_phase_with_progress() {
        let now = 5_000_000;
        let j = parse_journal(
            "{\"type\":\"launched\"}\n{\"type\":\"started\",\"agentId\":\"i1\",\"label\":\"impl:core\",\"phase\":\"Screens core\"}\n\
             {\"type\":\"started\",\"agentId\":\"r1\",\"label\":\"review:core:1\",\"phase\":\"Screens core\"}\n\
             {\"type\":\"started\",\"agentId\":\"x1\",\"label\":\"impl:ui\",\"phase\":\"Screens ui\"}\n\
             {\"type\":\"result\",\"agentId\":\"i1\"}\n",
        );
        let agents = vec![
            agent("i1", Some("wf_a"), "impl:core", Some("Screens core"), now - 5000),
            agent("r1", Some("wf_a"), "review:core:1", Some("Screens core"), now - 100),
            agent("lone", None, "Explore thing", None, now - 50),
        ];
        let r = derive(agents, vec![RunInput { id: "wf_a".into(), journal: j }], now);
        assert_eq!(r.runs.len(), 1);
        let run = &r.runs[0];
        assert_eq!((run.total, run.done, run.running), (3, 1, 2));
        assert_eq!(run.phases[0], PhaseProgress { name: "Screens core".into(), done: 1, total: 2, running: 1 });
        // x1 only in the journal: counted in its phase, not listed as an agent.
        assert_eq!(run.phases[1], PhaseProgress { name: "Screens ui".into(), done: 0, total: 1, running: 1 });
        assert_eq!(r.agents.len(), 3);
        assert_eq!(r.agents.iter().filter(|a| a.run.is_some()).count(), 2);
    }

    #[test]
    fn missing_everything_still_gives_a_row() {
        let r = derive(vec![AgentInput { id: "abcdef123456".into(), ..AgentInput::default() }], vec![], 1);
        assert_eq!(r.agents[0].label, "abcdef12");
        assert_eq!(r.agents[0].kind, "agent");
        assert_eq!(r.agents[0].state, AgentState::Running);
        assert!(r.runs.is_empty());
    }

    #[test]
    fn the_list_is_capped_but_counts_are_not() {
        let now = 1_000_000;
        let agents: Vec<AgentInput> = (0..MAX_AGENTS + 10).map(|i| agent(&format!("a{i:03}"), None, "x", None, now - i as i64)).collect();
        let r = derive(agents, vec![], now);
        assert_eq!(r.agents.len(), MAX_AGENTS);
        assert_eq!(r.omitted, 10);
        assert_eq!(r.total(), MAX_AGENTS + 10);
    }
}

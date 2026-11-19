//! What the Git tab shows beyond the snapshot's per-session fields: the changed files, the
//! branch list, the worktrees and the last failure. All of it comes from git tool **replies**
//! (`git_status`, `git_branches`, `worktree_list`, and the operations' results), which the
//! backend keeps here and puts into [`crate::model::UiModel::git`]; the screens only read it and
//! submit tools.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::backend::{Call, Notice};
use crate::model::{SessionId, SessionView};

/// The git tool ids whose replies land here.
pub const READ_TOOLS: [&str; 3] = ["git_status", "git_branches", "worktree_list"];
pub const OP_TOOLS: [&str; 6] = ["git_fetch", "git_pull", "git_switch_branch", "worktree_new", "worktree_remove", "worktree_prune"];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitChange {
    /// The two-letter XY code (`.M`, `??`, `UU`, ...).
    pub code: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorktreeSession {
    pub id: SessionId,
    pub name: String,
    pub asleep: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorktreeRow {
    pub path: String,
    pub name: String,
    /// `wt-feature  ⎇ feature ● ↑1 ↓2`, `wt-old — folder missing`.
    pub label: String,
    pub branch: Option<String>,
    pub main: bool,
    /// The session's own worktree.
    pub current: bool,
    pub dirty: bool,
    pub prunable: bool,
    pub locked: bool,
    pub sessions: Vec<WorktreeSession>,
    /// `session_new` params that open a session there (from the engine).
    pub open: Value,
}

impl WorktreeRow {
    /// What clicking the row does: focus its live session, wake its sleeping one, or open one.
    #[must_use]
    pub fn target(&self) -> (&'static str, Value) {
        let pick = self.sessions.iter().find(|s| !s.asleep).or(self.sessions.first());
        match pick {
            Some(s) => ("session_focus", serde_json::json!({"session_id": s.id})),
            None => ("session_new", self.open.clone()),
        }
    }
}

/// A branch checked out in another worktree.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Elsewhere {
    pub name: String,
    pub path: String,
    pub session_id: Option<SessionId>,
    pub open: Value,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BranchRow {
    pub name: String,
    pub current: bool,
    pub elsewhere: Option<Elsewhere>,
}

/// The last git operation that failed, for the output panel: what failed, git's own words, and
/// the recognised hint (all in `text`, as the tool's error says it).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GitFailure {
    /// Increases with every failure, so a dismissed panel comes back for the next one.
    pub seq: u64,
    pub tool: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GitDetail {
    /// The menu header line as `git_status` last read it.
    pub summary: Option<String>,
    pub changes: Vec<GitChange>,
    /// `git_status` said the folder is not in a repository.
    pub not_repo: bool,
    pub worktrees: Option<Vec<WorktreeRow>>,
    pub prunable: usize,
    pub branches: Option<Vec<BranchRow>>,
    pub failure: Option<GitFailure>,
    /// Bumped when an operation finished: the tab reads status and worktrees again.
    pub generation: u64,
}

/// The fields whose change means the tab's lists are stale (the poll moved them).
#[must_use]
pub fn signature(s: &SessionView) -> String {
    format!("{:?}|{}|{}|{}|{}|{:?}", s.branch, s.dirty, s.ahead, s.behind, s.changes, s.git_summary)
}

/// The details per session, kept by the backend from tool replies.
#[derive(Debug, Clone, Default)]
pub struct GitDetails {
    pub map: BTreeMap<SessionId, GitDetail>,
    seq: u64,
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn opt_s(v: &Value) -> Option<String> {
    v.as_str().filter(|x| !x.is_empty()).map(str::to_string)
}

impl GitDetails {
    /// A git tool's reply: stores what it says. `Some(notices)` when the call was a git tool
    /// (they replace the backend's default toast: reads are quiet, operations say their one
    /// sentence, failures say their first line and open the output panel).
    pub fn on_reply(&mut self, call: &Call, reply: &csm_engine::Reply) -> Option<Vec<Notice>> {
        let id = call.id.as_str();
        if !READ_TOOLS.contains(&id) && !OP_TOOLS.contains(&id) {
            return None;
        }
        let sid = call.params.get("session_id").and_then(Value::as_str).unwrap_or_default().to_string();
        let d = self.map.entry(sid).or_default();
        match (id, reply) {
            ("git_status", Ok(out)) => {
                let v = &out.data;
                d.not_repo = false;
                d.summary = opt_s(&v["summary"]);
                d.changes = v["changes"]
                    .as_array()
                    .map(|a| a.iter().map(|c| GitChange { code: s(&c["code"]), path: s(&c["path"]) }).collect())
                    .unwrap_or_default();
                Some(Vec::new())
            }
            ("git_status", Err(csm_engine::ToolError::Refused(_))) => {
                d.not_repo = true;
                d.summary = None;
                d.changes.clear();
                Some(Vec::new())
            }
            ("git_branches", Ok(out)) => {
                d.branches = Some(
                    out.data["branches"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|b| BranchRow {
                                    name: s(&b["name"]),
                                    current: b["current"].as_bool().unwrap_or(false),
                                    elsewhere: b["worktree"].is_object().then(|| Elsewhere {
                                        name: s(&b["worktree"]["name"]),
                                        path: s(&b["worktree"]["path"]),
                                        session_id: opt_s(&b["worktree"]["session_id"]),
                                        open: b["worktree"]["open"].clone(),
                                    }),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                );
                Some(Vec::new())
            }
            ("worktree_list", Ok(out)) => {
                d.prunable = out.data["prunable"].as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
                d.worktrees = Some(
                    out.data["worktrees"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .map(|w| WorktreeRow {
                                    path: s(&w["path"]),
                                    name: s(&w["name"]),
                                    label: s(&w["label"]),
                                    branch: opt_s(&w["branch"]),
                                    main: w["main"].as_bool().unwrap_or(false),
                                    current: w["current"].as_bool().unwrap_or(false),
                                    dirty: w["dirty"].as_bool().unwrap_or(false),
                                    prunable: w["prunable"].as_bool().unwrap_or(false),
                                    locked: w["locked"].as_bool().unwrap_or(false),
                                    sessions: w["sessions"]
                                        .as_array()
                                        .map(|ss| {
                                            ss.iter()
                                                .map(|x| WorktreeSession { id: s(&x["id"]), name: s(&x["name"]), asleep: x["asleep"].as_bool().unwrap_or(false) })
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                    open: w["open"].clone(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                );
                Some(Vec::new())
            }
            (_, Err(e)) if READ_TOOLS.contains(&id) => Some(vec![Notice::Toast(first_line(&e.to_string()))]),
            (_, Ok(out)) => {
                d.failure = None;
                d.branches = None;
                d.generation = d.generation.wrapping_add(1);
                Some(if out.message.is_empty() { Vec::new() } else { vec![Notice::Toast(out.message.clone())] })
            }
            (_, Err(e)) => {
                self.seq = self.seq.wrapping_add(1);
                let text = e.to_string();
                let toast = first_line(&text);
                let d = self.map.entry(call.params.get("session_id").and_then(Value::as_str).unwrap_or_default().to_string()).or_default();
                d.failure = Some(GitFailure { seq: self.seq, tool: id.to_string(), text });
                d.generation = d.generation.wrapping_add(1);
                Some(vec![Notice::Toast(toast)])
            }
        }
    }
}

fn first_line(t: &str) -> String {
    t.lines().next().unwrap_or_default().to_string()
}

/// The demo's fixture: what the Git tab shows for the Odulodo.Web session in `--demo` (and the
/// UI tests), without a repository.
#[must_use]
pub fn demo_details() -> BTreeMap<SessionId, GitDetail> {
    let mut m = BTreeMap::new();
    let wt = |path: &str, name: &str, label: &str, branch: &str, main: bool, current: bool, sessions: Vec<WorktreeSession>| WorktreeRow {
        path: path.into(),
        name: name.into(),
        label: label.into(),
        branch: Some(branch.into()),
        main,
        current,
        sessions,
        open: serde_json::json!({"folder": path, "command": "claude"}),
        ..WorktreeRow::default()
    };
    m.insert(
        "web".to_string(),
        GitDetail {
            summary: Some("feat/brand-facet → origin/feat/brand-facet · ↑2 · 3 changed".into()),
            changes: vec![
                GitChange { code: ".M".into(), path: "src/search/FacetPanel.tsx".into() },
                GitChange { code: ".M".into(), path: "src/search/facets.ts".into() },
                GitChange { code: "A.".into(), path: "src/search/api/brand.ts".into() },
            ],
            worktrees: Some(vec![
                wt(
                    "C:\\GitHub\\Odulodo.Web",
                    "Odulodo.Web",
                    "Odulodo.Web  ⎇ feat/brand-facet ● ↑2",
                    "feat/brand-facet",
                    true,
                    true,
                    vec![WorktreeSession { id: "web".into(), name: "Odulodo.Web".into(), asleep: false }],
                ),
                wt(
                    "C:\\GitHub\\Odulodo.Web\\.worktrees\\search-sort",
                    "search-sort",
                    "search-sort  ⎇ feat/search-sort",
                    "feat/search-sort",
                    false,
                    false,
                    vec![WorktreeSession { id: "wt".into(), name: "search-sort".into(), asleep: false }],
                ),
            ]),
            ..GitDetail::default()
        },
    );
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_engine::{ToolError, ToolOutput};
    use serde_json::json;

    fn call(id: &str) -> Call {
        Call { id: id.into(), params: json!({"session_id": "s1"}) }
    }

    #[test]
    fn replies_fill_the_details_and_failures_open_the_panel() {
        let mut g = GitDetails::default();
        let st = ToolOutput::new("main · clean", json!({"summary": "main · clean", "changes": [{"code": "??", "path": "x"}]}));
        assert_eq!(g.on_reply(&call("git_status"), &Ok(st)), Some(vec![]), "reads are quiet");
        assert_eq!(g.map["s1"].changes, vec![GitChange { code: "??".into(), path: "x".into() }]);

        let e = Err(ToolError::Failed("Pull (fast-forward only) failed.\nfatal: Not possible to fast-forward\n\nhint".into()));
        let n = g.on_reply(&call("git_pull"), &e).unwrap();
        assert_eq!(n, vec![Notice::Toast("Pull (fast-forward only) failed.".into())]);
        let f = g.map["s1"].failure.clone().unwrap();
        assert!(f.text.contains("Not possible to fast-forward"));

        let ok = Ok(ToolOutput::new("Fetched.", json!({})));
        assert_eq!(g.on_reply(&call("git_fetch"), &ok), Some(vec![Notice::Toast("Fetched.".into())]));
        assert!(g.map["s1"].failure.is_none(), "a success clears the panel");
        assert!(g.on_reply(&call("session_new"), &ok).is_none(), "other tools are not ours");
    }
}

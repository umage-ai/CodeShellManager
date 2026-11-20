//! The Board's tasks: the model, `tasks.json`, the state machine and who may do what.
//!
//! A task belongs to a **project**: the git repository a session works in (every worktree of a
//! repository shares it), or the session's group outside a repository ([`project_key`]).
//!
//! ```text
//!   new ──► Backlog ──claim/assign──► Assigned ──done──► Review ──accept──► Done
//!             ▲                          │                  │
//!             └────────handoff───────────┘                  └──reopen──► Assigned (or Backlog)
//!   cancel: any open state ──► Cancelled; reopen brings Done/Cancelled back too.
//! ```
//!
//! Pure: no I/O, no clock (every `now` is a parameter). Everything read from `tasks.json` or
//! sent by an agent is hostile input: parsing never fails the whole file for one bad task, and
//! text is cleaned ([`clean_title`], [`clean_body`], [`clean_note`]) before it is stored, since a
//! task's text is later pasted into a session's prompt.

use serde::{Deserialize, Serialize};

/// Longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 200;
/// Longest body, in bytes (UTF-8).
pub const MAX_BODY_BYTES: usize = 16 * 1024;
/// Longest progress note, in characters.
pub const MAX_NOTE_CHARS: usize = 500;
/// Open tasks (not done, not cancelled) one agent session may have created.
pub const MAX_OPEN_PER_SESSION: usize = 50;
/// Tasks kept in the file at most (a bound on what agents can make `tasks.json` grow to).
pub const MAX_TASKS: usize = 5000;
/// The `tasks.json` format version.
pub const TASKS_FILE_VERSION: u32 = 1;
/// `created_by` of tasks the person made (in the app or with the CLI).
pub const CREATED_BY_PERSON: &str = "person";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Waiting for someone to take it.
    Backlog,
    /// An agent session works on it.
    Assigned,
    /// The agent says it is done; the person checks it.
    Review,
    /// Accepted.
    Done,
    Cancelled,
}

impl TaskState {
    pub fn id(self) -> &'static str {
        match self {
            TaskState::Backlog => "backlog",
            TaskState::Assigned => "assigned",
            TaskState::Review => "review",
            TaskState::Done => "done",
            TaskState::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskState::Backlog => "Backlog",
            TaskState::Assigned => "Assigned",
            TaskState::Review => "Review",
            TaskState::Done => "Done",
            TaskState::Cancelled => "Cancelled",
        }
    }

    /// Not finished: counts against [`MAX_OPEN_PER_SESSION`].
    pub fn is_open(self) -> bool {
        !matches!(self, TaskState::Done | TaskState::Cancelled)
    }
}

/// One task. Ids are `t<n>`, unique in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// [`project_key`] of the project it belongs to.
    pub project: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub state: TaskState,
    /// The session working on it (Assigned) or that did it (Review, Done).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    /// [`CREATED_BY_PERSON`], `session:<id>` (an agent in one of our sessions) or `mcp:<client>`.
    pub created_by: String,
    /// How it was made: `ui`, `cli` or `mcp`.
    #[serde(default)]
    pub source: String,
    /// The assignee's last progress note (`task_update`, `task_done`, `task_handoff`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// Position within its column (ascending).
    #[serde(default)]
    pub order: i64,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl Task {
    /// The session that created it, when an agent did.
    pub fn creator_session(&self) -> Option<&str> {
        self.created_by.strip_prefix("session:")
    }
}

/// The whole of `tasks.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFile {
    pub version: u32,
    /// The number the next task's id gets.
    pub next_id: u64,
    pub tasks: Vec<Task>,
}

impl Default for TaskFile {
    fn default() -> Self {
        TaskFile { version: TASKS_FILE_VERSION, next_id: 1, tasks: Vec::new() }
    }
}

impl TaskFile {
    /// Reads `tasks.json`. `Err` only when the text isn't a task file at all (the caller then
    /// tries the `.bak`); a task that doesn't parse, has an empty or duplicate id, or text over
    /// the caps is dropped (text is cleaned again: the file is untrusted), never the whole file.
    pub fn parse(text: &str) -> Result<TaskFile, String> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            version: u32,
            #[serde(default)]
            next_id: u64,
            #[serde(default)]
            tasks: Vec<serde_json::Value>,
        }
        let raw: Raw = serde_json::from_str(text).map_err(|e| format!("not a task file: {e}"))?;
        let mut tasks: Vec<Task> = Vec::new();
        for v in raw.tasks {
            let Ok(mut t) = serde_json::from_value::<Task>(v) else { continue };
            if t.id.trim().is_empty() || tasks.iter().any(|x| x.id == t.id) {
                continue;
            }
            let (Ok(title), Ok(body)) = (clean_title(&t.title), clean_body(&t.body)) else { continue };
            t.title = title;
            t.body = body;
            t.progress = t.progress.as_deref().and_then(|p| clean_note(p).ok()).filter(|p| !p.is_empty());
            t.project = strip_controls(&t.project, false);
            tasks.push(t);
            if tasks.len() >= MAX_TASKS {
                break;
            }
        }
        // Never hand out an id that is already taken.
        let highest = tasks.iter().filter_map(|t| t.id.strip_prefix('t').and_then(|n| n.parse::<u64>().ok())).max().unwrap_or(0);
        Ok(TaskFile { version: raw.version.max(TASKS_FILE_VERSION), next_id: raw.next_id.max(highest.saturating_add(1)).max(1), tasks })
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// Open tasks an agent session created (the per-session cap).
    pub fn open_created_by(&self, session_id: &str) -> usize {
        self.tasks.iter().filter(|t| t.state.is_open() && t.creator_session() == Some(session_id)).count()
    }

    /// Adds a Backlog task at the end of its project's backlog; returns its id. The caller has
    /// cleaned the text and checked the permissions and caps.
    pub fn add(&mut self, project: &str, title: String, body: String, created_by: String, source: &str, now_ms: i64) -> Result<String, TaskError> {
        if self.tasks.len() >= MAX_TASKS {
            return Err(TaskError(format!("there are already {MAX_TASKS} tasks; delete finished ones first")));
        }
        let id = format!("t{}", self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let order = self.next_order(project, TaskState::Backlog);
        self.tasks.push(Task {
            id: id.clone(),
            project: project.to_owned(),
            title,
            body,
            state: TaskState::Backlog,
            assignee: None,
            created_by,
            source: source.to_owned(),
            progress: None,
            order,
            created_ms: now_ms,
            updated_ms: now_ms,
        });
        Ok(id)
    }

    /// One past the highest order in a project's column.
    pub fn next_order(&self, project: &str, state: TaskState) -> i64 {
        self.tasks.iter().filter(|t| t.project == project && t.state == state).map(|t| t.order).max().map_or(0, |o| o.saturating_add(1))
    }

    /// A session is gone: its Assigned tasks go back to the backlog (Review and Done keep who
    /// did them). Returns the ids that moved.
    pub fn release_session(&mut self, session_id: &str, now_ms: i64) -> Vec<String> {
        let mut moved = Vec::new();
        for i in 0..self.tasks.len() {
            let hit = self.tasks.get(i).is_some_and(|t| t.state == TaskState::Assigned && t.assignee.as_deref() == Some(session_id));
            if !hit {
                continue;
            }
            let (project, id) = match self.tasks.get(i) {
                Some(t) => (t.project.clone(), t.id.clone()),
                None => continue,
            };
            let order = self.next_order(&project, TaskState::Backlog);
            if let Some(t) = self.tasks.get_mut(i) {
                t.state = TaskState::Backlog;
                t.assignee = None;
                t.order = order;
                t.updated_ms = now_ms;
                moved.push(id);
            }
        }
        moved
    }
}

/// Why a transition or an action was refused (plain English, says what to do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskError(pub String);

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn refuse<T>(msg: impl Into<String>) -> Result<T, TaskError> {
    Err(TaskError(msg.into()))
}

// ── The state machine ───────────────────────────────────────────────────────────────────

/// `task_claim`: an unassigned Backlog task becomes `session_id`'s.
pub fn claim(t: &mut Task, session_id: &str, now_ms: i64) -> Result<(), TaskError> {
    if t.state != TaskState::Backlog {
        return refuse(format!("task {} is {}, not in the backlog; only backlog tasks can be claimed", t.id, t.state.id()));
    }
    if t.assignee.as_deref().is_some_and(|a| a != session_id) {
        return refuse(format!("task {} is already someone else's", t.id));
    }
    t.state = TaskState::Assigned;
    t.assignee = Some(session_id.to_owned());
    t.updated_ms = now_ms;
    Ok(())
}

/// `task_assign`: Backlog, Assigned or Review → Assigned to `session_id`. Taking it from another
/// session that still has it needs `force`.
pub fn assign(t: &mut Task, session_id: &str, force: bool, now_ms: i64) -> Result<(), TaskError> {
    if !t.state.is_open() {
        return refuse(format!("task {} is {}; reopen it first", t.id, t.state.id()));
    }
    if t.state == TaskState::Assigned && t.assignee.as_deref().is_some_and(|a| a != session_id) && !force {
        return refuse(format!("task {} is assigned to another session; pass force: true to move it", t.id));
    }
    t.state = TaskState::Assigned;
    t.assignee = Some(session_id.to_owned());
    t.updated_ms = now_ms;
    Ok(())
}

/// `task_done`: Assigned → Review (the person accepts or reopens it).
pub fn done(t: &mut Task, note: Option<String>, now_ms: i64) -> Result<(), TaskError> {
    if t.state != TaskState::Assigned {
        return refuse(format!("task {} is {}; only an assigned task can be marked done", t.id, t.state.id()));
    }
    t.state = TaskState::Review;
    if note.is_some() {
        t.progress = note;
    }
    t.updated_ms = now_ms;
    Ok(())
}

/// `task_handoff`: Assigned → Backlog, unassigned, so someone else can take it.
pub fn handoff(t: &mut Task, note: Option<String>, order: i64, now_ms: i64) -> Result<(), TaskError> {
    if t.state != TaskState::Assigned {
        return refuse(format!("task {} is {}; only an assigned task can be handed off", t.id, t.state.id()));
    }
    t.state = TaskState::Backlog;
    t.assignee = None;
    t.order = order;
    if note.is_some() {
        t.progress = note;
    }
    t.updated_ms = now_ms;
    Ok(())
}

/// `task_accept`: Review → Done.
pub fn accept(t: &mut Task, now_ms: i64) -> Result<(), TaskError> {
    if t.state != TaskState::Review {
        return refuse(format!("task {} is {}; only a task in review can be accepted", t.id, t.state.id()));
    }
    t.state = TaskState::Done;
    t.updated_ms = now_ms;
    Ok(())
}

/// `task_reopen`: Review, Done or Cancelled → Assigned to whoever had it, or Backlog.
pub fn reopen(t: &mut Task, note: Option<String>, now_ms: i64) -> Result<(), TaskError> {
    if !matches!(t.state, TaskState::Review | TaskState::Done | TaskState::Cancelled) {
        return refuse(format!("task {} is {}; only a task in review, done or cancelled can be reopened", t.id, t.state.id()));
    }
    t.state = if t.assignee.is_some() { TaskState::Assigned } else { TaskState::Backlog };
    if note.is_some() {
        t.progress = note;
    }
    t.updated_ms = now_ms;
    Ok(())
}

/// `task_update { cancel: true }`: any open state → Cancelled.
pub fn cancel(t: &mut Task, now_ms: i64) -> Result<(), TaskError> {
    if !t.state.is_open() {
        return refuse(format!("task {} is already {}", t.id, t.state.id()));
    }
    t.state = TaskState::Cancelled;
    t.updated_ms = now_ms;
    Ok(())
}

// ── Who may do what ─────────────────────────────────────────────────────────────────────

/// Who asks, as the permission rules see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// The person: the app's UI or the CLI. May do everything.
    Person,
    /// An MCP client running in one of our sessions, working in `project`.
    Agent { session_id: String, project: String },
    /// An MCP client outside our sessions: it belongs to no project.
    Outsider,
}

/// What an actor wants to do (the `task_*` tools).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Read,
    New,
    Claim,
    Update,
    Cancel,
    Done,
    Handoff,
    Assign,
    Accept,
    Reopen,
    Delete,
}

/// The rules for agents (decided 2026-11-20: task bookkeeping without a consent prompt, within
/// limits). Checked before any consent card, so a call that would be refused never asks.
///
/// - The person may do everything.
/// - An agent works only in its own project; it may claim unassigned backlog tasks and update,
///   finish or hand off only the tasks assigned to itself. It may create tasks in its project
///   (the cap is [`MAX_OPEN_PER_SESSION`], checked with `open_created`). It can't cancel, accept
///   or reopen. Assign and delete are allowed in its project; their effects make it ask.
/// - An outsider may read, and assign or delete (asking); task bookkeeping needs a session.
///
/// `task` is the task acted on (`None` for `New`); `project` is the project of a `New` task or
/// the target session's project for `Assign`.
pub fn permit(actor: &Actor, op: Op, task: Option<&Task>, project: Option<&str>, open_created: usize) -> Result<(), TaskError> {
    let (own, own_project) = match actor {
        Actor::Person => return Ok(()),
        Actor::Outsider => {
            return match op {
                Op::Read | Op::Assign | Op::Delete => Ok(()),
                Op::Accept | Op::Reopen => refuse("accepting and reopening tasks is the person's call, in the app or the CLI"),
                _ => refuse("only an agent running in a CodeShellManager session can change tasks over MCP; ask the person"),
            };
        }
        Actor::Agent { session_id, project } => (session_id.as_str(), project.as_str()),
    };
    if let Some(t) = task
        && t.project != own_project
    {
        return refuse(format!("task {} belongs to another project; an agent works only on its own project's tasks", t.id));
    }
    if let Some(p) = project
        && p != own_project
    {
        return refuse("that is another project; an agent works only in its own project");
    }
    let mine = task.is_some_and(|t| t.assignee.as_deref() == Some(own));
    match op {
        Op::Read | Op::Assign | Op::Delete => Ok(()),
        Op::New if open_created >= MAX_OPEN_PER_SESSION => {
            refuse(format!("this session already created {MAX_OPEN_PER_SESSION} open tasks; finish or hand some back first"))
        }
        Op::New => Ok(()),
        Op::Claim => match task {
            Some(t) if t.assignee.is_none() || mine => Ok(()),
            _ => refuse("that task is assigned to another session; an agent may claim only unassigned tasks"),
        },
        Op::Update | Op::Done | Op::Handoff if mine => Ok(()),
        Op::Update | Op::Done | Op::Handoff => refuse("an agent may change only the tasks assigned to itself; claim it first (task_claim)"),
        Op::Cancel => refuse("an agent can't cancel a task; hand it back (task_handoff) and say why"),
        Op::Accept | Op::Reopen => refuse("accepting and reopening tasks is the person's call, in the app or the CLI"),
    }
}

// ── Projects ────────────────────────────────────────────────────────────────────────────

/// A session's project: `repo:<main checkout root>` inside a git repository (worktrees of one
/// repository share it), else `group:<group id>` (`group:` for ungrouped sessions).
pub fn project_key(repo_root: Option<&str>, group_id: &str) -> String {
    match repo_root.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("repo:{r}"),
        None => format!("group:{group_id}"),
    }
}

/// A project's name for people: the repository's folder name, or the group's name
/// (`group_name` looks it up; "Ungrouped" without one).
pub fn project_label(key: &str, group_name: impl Fn(&str) -> Option<String>) -> String {
    if let Some(root) = key.strip_prefix("repo:") {
        let leaf = root.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(root);
        return if leaf.is_empty() { root.to_owned() } else { leaf.to_owned() };
    }
    match key.strip_prefix("group:") {
        Some("") => "Ungrouped".to_owned(),
        Some(g) => group_name(g).unwrap_or_else(|| "Ungrouped".to_owned()),
        None => key.to_owned(),
    }
}

// ── Text ────────────────────────────────────────────────────────────────────────────────

/// Bidirectional overrides and isolates: invisible, and able to make text read differently
/// from what it is ("Trojan Source").
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}')
}

/// Removes control characters (ESC included, so no escape sequence survives) and bidi controls;
/// `keep_lines` keeps newline and tab. CR LF and lone CR become LF first.
fn strip_controls(s: &str, keep_lines: bool) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    s.chars().filter(|&c| !is_bidi_control(c) && (!c.is_control() || (keep_lines && (c == '\n' || c == '\t')))).collect()
}

/// A title: one line, controls removed, trimmed, 1 to [`MAX_TITLE_CHARS`] characters.
pub fn clean_title(s: &str) -> Result<String, String> {
    let t = strip_controls(&s.replace(['\r', '\n', '\t'], " "), false).trim().to_owned();
    if t.is_empty() {
        return Err("the title is empty".into());
    }
    if t.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("the title is longer than {MAX_TITLE_CHARS} characters"));
    }
    Ok(t)
}

/// A body: lines kept, other controls removed, trailing space trimmed, at most
/// [`MAX_BODY_BYTES`].
pub fn clean_body(s: &str) -> Result<String, String> {
    let b = strip_controls(s, true).trim_end().to_owned();
    if b.len() > MAX_BODY_BYTES {
        return Err(format!("the body is longer than {} KiB", MAX_BODY_BYTES / 1024));
    }
    Ok(b)
}

/// A progress note: like a title, up to [`MAX_NOTE_CHARS`]; empty is allowed (clears it).
pub fn clean_note(s: &str) -> Result<String, String> {
    let t = strip_controls(&s.replace(['\r', '\n', '\t'], " "), false).trim().to_owned();
    if t.chars().count() > MAX_NOTE_CHARS {
        return Err(format!("the note is longer than {MAX_NOTE_CHARS} characters"));
    }
    Ok(t)
}

/// What `task_assign` pastes into the session: `Task <id> from <creator>: <title>`, then the body.
pub fn assignment_prompt(t: &Task, creator: &str) -> String {
    let head = format!("Task {} from {creator}: {}", t.id, t.title);
    if t.body.is_empty() { head } else { format!("{head}\n{}", t.body) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn task(state: TaskState, assignee: Option<&str>) -> Task {
        Task {
            id: "t1".into(),
            project: "repo:/src/app".into(),
            title: "Fix it".into(),
            body: String::new(),
            state,
            assignee: assignee.map(str::to_owned),
            created_by: CREATED_BY_PERSON.into(),
            source: "ui".into(),
            progress: None,
            order: 0,
            created_ms: 0,
            updated_ms: 0,
        }
    }

    fn agent(id: &str) -> Actor {
        Actor::Agent { session_id: id.into(), project: "repo:/src/app".into() }
    }

    #[test]
    fn the_happy_path_through_every_state() {
        let mut t = task(TaskState::Backlog, None);
        claim(&mut t, "a", NOW).unwrap();
        assert_eq!((t.state, t.assignee.as_deref(), t.updated_ms), (TaskState::Assigned, Some("a"), NOW));
        done(&mut t, Some("tests pass".into()), NOW + 1).unwrap();
        assert_eq!((t.state, t.progress.as_deref()), (TaskState::Review, Some("tests pass")));
        reopen(&mut t, Some("one more case".into()), NOW + 2).unwrap();
        assert_eq!((t.state, t.assignee.as_deref()), (TaskState::Assigned, Some("a")), "reopened back to the same agent");
        done(&mut t, None, NOW + 3).unwrap();
        assert_eq!(t.progress.as_deref(), Some("one more case"), "no note keeps the last one");
        accept(&mut t, NOW + 4).unwrap();
        assert_eq!(t.state, TaskState::Done);
        assert!(!t.state.is_open());
    }

    #[test]
    fn transitions_from_the_wrong_state_are_refused() {
        let mut t = task(TaskState::Backlog, None);
        assert!(done(&mut t, None, NOW).is_err());
        assert!(handoff(&mut t, None, 0, NOW).is_err());
        assert!(accept(&mut t, NOW).is_err());
        assert!(reopen(&mut t, None, NOW).is_err());
        let mut d = task(TaskState::Done, Some("a"));
        assert!(claim(&mut d, "a", NOW).is_err());
        assert!(assign(&mut d, "b", true, NOW).is_err(), "a done task is reopened, not reassigned");
        assert!(cancel(&mut d, NOW).is_err());
        let mut c = task(TaskState::Cancelled, None);
        reopen(&mut c, None, NOW).unwrap();
        assert_eq!(c.state, TaskState::Backlog, "nobody had it: back to the backlog");
        assert_eq!(d, task(TaskState::Done, Some("a")), "a refused transition changes nothing");
    }

    #[test]
    fn handoff_returns_it_unassigned_and_assign_needs_force_to_take_it_away() {
        let mut t = task(TaskState::Assigned, Some("a"));
        assert!(claim(&mut t, "b", NOW).is_err());
        assert!(assign(&mut t, "b", false, NOW).is_err());
        assign(&mut t, "a", false, NOW).unwrap();
        assign(&mut t, "b", true, NOW).unwrap();
        assert_eq!(t.assignee.as_deref(), Some("b"));
        handoff(&mut t, Some("blocked on the API".into()), 7, NOW).unwrap();
        assert_eq!((t.state, t.assignee.clone(), t.order), (TaskState::Backlog, None, 7));
        // A task in review can be given to someone else straight away.
        let mut r = task(TaskState::Review, Some("a"));
        assign(&mut r, "b", false, NOW).unwrap();
        assert_eq!((r.state, r.assignee.as_deref()), (TaskState::Assigned, Some("b")));
        cancel(&mut r, NOW).unwrap();
        assert_eq!(r.state, TaskState::Cancelled);
    }

    #[test]
    fn the_person_may_do_everything() {
        let t = task(TaskState::Assigned, Some("other"));
        for op in [Op::Read, Op::New, Op::Claim, Op::Update, Op::Cancel, Op::Done, Op::Handoff, Op::Assign, Op::Accept, Op::Reopen, Op::Delete] {
            assert!(permit(&Actor::Person, op, Some(&t), Some("group:x"), 999).is_ok(), "{op:?}");
        }
    }

    #[test]
    fn an_agent_keeps_to_its_own_project_and_its_own_tasks() {
        let me = agent("me");
        let free = task(TaskState::Backlog, None);
        let mine = task(TaskState::Assigned, Some("me"));
        let theirs = task(TaskState::Assigned, Some("other"));
        assert!(permit(&me, Op::Read, Some(&theirs), None, 0).is_ok());
        assert!(permit(&me, Op::Claim, Some(&free), None, 0).is_ok());
        assert!(permit(&me, Op::Claim, Some(&theirs), None, 0).is_err());
        for op in [Op::Update, Op::Done, Op::Handoff] {
            assert!(permit(&me, op, Some(&mine), None, 0).is_ok(), "{op:?} on its own");
            assert!(permit(&me, op, Some(&theirs), None, 0).is_err(), "{op:?} on another's");
            assert!(permit(&me, op, Some(&free), None, 0).is_err(), "{op:?} on an unclaimed one");
        }
        for op in [Op::Cancel, Op::Accept, Op::Reopen] {
            assert!(permit(&me, op, Some(&mine), None, 0).is_err(), "{op:?} is the person's");
        }
        // Assign and delete are allowed in its project (their effects make it ask).
        assert!(permit(&me, Op::Assign, Some(&free), Some("repo:/src/app"), 0).is_ok());
        assert!(permit(&me, Op::Delete, Some(&theirs), None, 0).is_ok());
        // Another project: nothing, not even reading.
        let mut elsewhere = free.clone();
        elsewhere.project = "repo:/src/other".into();
        for op in [Op::Read, Op::Claim, Op::Assign, Op::Delete] {
            assert!(permit(&me, op, Some(&elsewhere), None, 0).is_err(), "{op:?} in another project");
        }
        assert!(permit(&me, Op::New, None, Some("group:g"), 0).is_err());
        assert!(permit(&me, Op::Assign, Some(&free), Some("repo:/src/other"), 0).is_err(), "to a session elsewhere");
    }

    #[test]
    fn an_agent_may_create_at_most_fifty_open_tasks() {
        let me = agent("me");
        assert!(permit(&me, Op::New, None, Some("repo:/src/app"), MAX_OPEN_PER_SESSION - 1).is_ok());
        assert!(permit(&me, Op::New, None, Some("repo:/src/app"), MAX_OPEN_PER_SESSION).is_err());
        let mut f = TaskFile::default();
        for i in 0..3 {
            f.add("repo:/src/app", format!("x{i}"), String::new(), "session:me".into(), "mcp", NOW).unwrap();
        }
        f.add("repo:/src/app", "y".into(), String::new(), CREATED_BY_PERSON.into(), "ui", NOW).unwrap();
        assert_eq!(f.open_created_by("me"), 3);
        f.tasks[0].state = TaskState::Done;
        assert_eq!(f.open_created_by("me"), 2, "finished tasks don't count");
    }

    #[test]
    fn an_outsider_reads_and_asks_but_does_no_bookkeeping() {
        let t = task(TaskState::Backlog, None);
        assert!(permit(&Actor::Outsider, Op::Read, Some(&t), None, 0).is_ok());
        assert!(permit(&Actor::Outsider, Op::Assign, Some(&t), None, 0).is_ok());
        assert!(permit(&Actor::Outsider, Op::Delete, Some(&t), None, 0).is_ok());
        for op in [Op::New, Op::Claim, Op::Update, Op::Done, Op::Handoff, Op::Accept, Op::Reopen, Op::Cancel] {
            assert!(permit(&Actor::Outsider, op, Some(&t), None, 0).is_err(), "{op:?}");
        }
    }

    #[test]
    fn untrusted_text_is_cleaned_and_capped() {
        assert_eq!(clean_title("  Fix\tthe\x1b[31m bug\r\n now \u{202E}gnp.exe ").unwrap(), "Fix the[31m bug   now gnp.exe");
        assert!(clean_title(" \x07 ").is_err());
        assert!(clean_title(&"x".repeat(MAX_TITLE_CHARS + 1)).is_err());
        assert_eq!(clean_title(&"é".repeat(MAX_TITLE_CHARS)).unwrap().chars().count(), MAX_TITLE_CHARS);
        // The body keeps its lines but no escape: a pasted ESC[201~ can't end the bracketed paste.
        assert_eq!(clean_body("a\r\nb\x1b[201~c\x00\n\td  \n").unwrap(), "a\nb[201~c\n\td");
        assert!(clean_body(&"x".repeat(MAX_BODY_BYTES + 1)).is_err());
        assert_eq!(clean_body(&"x".repeat(MAX_BODY_BYTES)).unwrap().len(), MAX_BODY_BYTES);
        assert_eq!(clean_note("").unwrap(), "");
        assert!(clean_note(&"x".repeat(MAX_NOTE_CHARS + 1)).is_err());
    }

    #[test]
    fn the_prompt_names_the_task_and_its_creator() {
        let mut t = task(TaskState::Backlog, None);
        assert_eq!(assignment_prompt(&t, "the person"), "Task t1 from the person: Fix it");
        t.body = "Line one\nLine two".into();
        assert_eq!(assignment_prompt(&t, "Odulodo"), "Task t1 from Odulodo: Fix it\nLine one\nLine two");
    }

    #[test]
    fn projects_are_repositories_or_groups() {
        assert_eq!(project_key(Some("/src/app"), "g1"), "repo:/src/app");
        assert_eq!(project_key(Some("  "), "g1"), "group:g1");
        assert_eq!(project_key(None, ""), "group:");
        let names = |g: &str| (g == "g1").then(|| "Client X".to_owned());
        assert_eq!(project_label("repo:C:\\GitHub\\Odulodo", names), "Odulodo");
        assert_eq!(project_label("repo:/src/app/", names), "app");
        assert_eq!(project_label("group:g1", names), "Client X");
        assert_eq!(project_label("group:", names), "Ungrouped");
        assert_eq!(project_label("group:gone", names), "Ungrouped");
    }

    #[test]
    fn the_file_round_trips_and_survives_bad_entries() {
        let mut f = TaskFile::default();
        let a = f.add("group:", "One".into(), "body".into(), CREATED_BY_PERSON.into(), "ui", NOW).unwrap();
        let b = f.add("group:", "Two".into(), String::new(), "session:s1".into(), "mcp", NOW).unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("t1", "t2"));
        assert_eq!(f.get("t2").unwrap().order, 1);
        let back = TaskFile::parse(&f.to_json().unwrap()).unwrap();
        assert_eq!(back, f);
        // Hostile: a duplicate id, a task without a title, an escape in a title, a next_id that
        // would hand out a taken id, and junk.
        let text = r#"{"version":1,"next_id":1,"tasks":[
            {"id":"t5","project":"group:","title":"ok \u001b]0;x\u0007","state":"backlog","created_by":"person","created_ms":0,"updated_ms":0},
            {"id":"t5","project":"group:","title":"dup","state":"backlog","created_by":"person","created_ms":0,"updated_ms":0},
            {"id":"t6","project":"group:","title":"","state":"backlog","created_by":"person","created_ms":0,"updated_ms":0},
            {"id":"t7","project":"group:","title":"bad state","state":"limbo","created_by":"person","created_ms":0,"updated_ms":0},
            42]}"#;
        let f = TaskFile::parse(text).unwrap();
        assert_eq!(f.tasks.len(), 1);
        assert_eq!(f.tasks[0].title, "ok ]0;x");
        assert_eq!(f.next_id, 6);
        assert!(TaskFile::parse("not json").is_err());
        assert!(TaskFile::parse("42").is_err());
        assert_eq!(TaskFile::parse("{}").unwrap(), TaskFile::default());
    }

    #[test]
    fn a_closed_session_gives_its_assigned_tasks_back() {
        let mut f = TaskFile::default();
        for title in ["a", "b", "c"] {
            f.add("group:", title.into(), String::new(), CREATED_BY_PERSON.into(), "ui", NOW).unwrap();
        }
        claim(f.get_mut("t1").unwrap(), "s", NOW).unwrap();
        claim(f.get_mut("t2").unwrap(), "s", NOW).unwrap();
        done(f.get_mut("t2").unwrap(), None, NOW).unwrap();
        assert_eq!(f.release_session("s", NOW + 1), vec!["t1".to_string()]);
        let t1 = f.get("t1").unwrap();
        assert_eq!((t1.state, t1.assignee.clone(), t1.order), (TaskState::Backlog, None, 3));
        assert_eq!(f.get("t2").unwrap().assignee.as_deref(), Some("s"), "review keeps who did it");
    }
}

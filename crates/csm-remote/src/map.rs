//! The pure part: the host's wire read model, its events and replies, as the UI and the engine's
//! types. No I/O, no threads; everything here is tested without a connection.
//!
//! Nothing here indexes or unwraps: wire values are the host's, and a remote host is not trusted to
//! send well-formed data.

use std::collections::BTreeMap;

use csm_core::models::InspectorTab as CoreTab;
use csm_engine::{Effects, Event, GroupView as EngineView, Reply, ToolError, ToolOutput, Toolset};
use csm_peer_proto::Event as WireEvent;
use csm_peer_proto::msg::{ErrorKind, Outcome};
use csm_peer_proto::{GroupView as WireView, RunPhase, RunView as WireRun, SessionView as WireSession, Snapshot as WireSnapshot, Status as WireStatus};
use csm_ui::backend::ToolInfo;
use csm_ui::engine_backend::{accent_from_hex, layout_from_core, scope_key_from_engine};
use csm_ui::model::{
    ClosedSessionView, FolderSuggestion, GroupMode, GroupView, InboxPanel, InspectorTab, Kind, Layout, OpenWithEntryView, OpenWithItem, RunState, RunView,
    Scope, SessionView, Status, UiModel,
};
use csm_ui::theme::ThemeKind;
use csm_ui::view_overlay::{SessionFact, ThemeFact, ViewFacts, ViewOverlay};
use serde_json::Value;

/// The UI's status for a host status. An exited process shows as idle, as the office does.
pub fn status(s: WireStatus) -> Status {
    match s {
        WireStatus::Starting => Status::Starting,
        WireStatus::Working => Status::Working,
        WireStatus::NeedsYouApproval => Status::Approval,
        WireStatus::NeedsYouQuestion => Status::Question,
        WireStatus::DoneUnseen => Status::Done,
        WireStatus::Error => Status::Error,
        WireStatus::Idle | WireStatus::Exited => Status::Idle,
        WireStatus::Asleep => Status::Asleep,
    }
}

fn kind(k: &str) -> Kind {
    match k {
        "ssh" => Kind::Ssh,
        "wsl" => Kind::Wsl,
        _ => Kind::Local,
    }
}

fn millis(t: Option<i64>) -> Option<u64> {
    t.and_then(|t| u64::try_from(t).ok())
}

/// How long, in words ("12 s", "4 min", "2 h").
fn took(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h", s / 3600),
    }
}

fn run(r: &WireRun, now_ms: i64) -> RunView {
    let dur = match (r.started_ms, r.ended_ms) {
        (Some(a), Some(b)) => Some(took(b - a)),
        (Some(a), None) => Some(took(now_ms - a)),
        _ => None,
    };
    let with = |s: &str| dur.as_ref().map_or_else(|| s.to_string(), |d| format!("{s} · {d}"));
    let (state, detail) = match r.phase {
        RunPhase::Idle => (RunState::NotRun, "not run yet".to_string()),
        RunPhase::Starting => (RunState::Running, "starting".to_string()),
        RunPhase::Running => (RunState::Running, with("running")),
        RunPhase::Ok => (RunState::Passed, with("passed · exit 0")),
        RunPhase::Failed => match r.exit_code {
            Some(-1) => (RunState::Failed, "could not start".to_string()),
            Some(c) => (RunState::Failed, with(&format!("failed · exit {c}"))),
            None => (RunState::Failed, with("failed")),
        },
        RunPhase::Stopped => (RunState::NotRun, with("stopped")),
    };
    RunView {
        id: r.id.clone(),
        label: r.label.clone(),
        command_line: r.command_line.clone(),
        is_default: r.is_default,
        state,
        detail,
        output: String::new(),
        mode: r.mode.clone(),
        post_run_url: r.post_run_url.clone(),
        exit_code: r.exit_code,
    }
}

fn started(s: &WireSession) -> String {
    match (s.asleep, s.backend.as_deref(), s.status == WireStatus::Exited) {
        (true, _, _) => "Asleep: the process is stopped; it starts again when woken".to_string(),
        (_, _, true) => "The process ended; restart or close the session".to_string(),
        (_, Some(b), _) if s.is_claude => format!("Running on the {b} · resumes its conversation on restart"),
        (_, Some(b), _) => format!("Running on the {b}"),
        (_, None, _) => "Starting".to_string(),
    }
}

fn session(s: &WireSession, now_ms: i64) -> SessionView {
    let kind = kind(&s.kind);
    let folder = match kind {
        Kind::Local if !s.folder.is_empty() => s.folder.clone(),
        _ => s.folder_short.clone(),
    };
    SessionView {
        id: s.id.clone(),
        name: s.name.clone(),
        group: (!s.group_id.is_empty()).then(|| s.group_id.clone()),
        kind,
        command: s.command.clone(),
        folder,
        branch: s.branch.clone(),
        dirty: s.git_dirty,
        ahead: s.git_ahead.and_then(|a| u32::try_from(a).ok()).unwrap_or(0),
        behind: s.git_behind.and_then(|b| u32::try_from(b).ok()).unwrap_or(0),
        upstream: s.git_upstream.clone(),
        changes: u32::try_from(s.git_changes).unwrap_or(u32::MAX),
        git_summary: s.git_summary.clone(),
        worktree_of: s.worktree_of.clone(),
        repo_root: s.repo_root.clone(),
        git_busy: s.git_busy,
        status: status(s.status),
        progress: s.progress,
        detail: s.status_message.clone().unwrap_or_default(),
        status_since_ms: millis(s.status_since_ms),
        inbox_since_ms: millis(s.inbox_since_ms),
        project: s.project.clone(),
        project_name: s.project_name.clone(),
        accent: accent_from_hex(&s.accent),
        accent_overridden: s.accent_override,
        parent: None,
        runs: s.runs.iter().map(|r| run(r, now_ms)).collect(),
        open_with: s
            .open_with
            .iter()
            .map(|o| OpenWithItem { entry_id: o.entry_id.clone(), label: o.label.clone(), program: o.program.clone(), command: o.command.clone(), is_default: o.is_default })
            .collect(),
        started: started(s),
        exited: s.status == WireStatus::Exited,
        exit_code: s.exit_code,
        look: None,
        last_used_ms: s.last_activity_ms,
        remote_control: s.remote_control,
        remote_control_url: s.remote_control_url.clone(),
        ..SessionView::default()
    }
}

/// A linked worktree's session nests under the main checkout's session with the same repo root.
fn worktree_parents(sessions: &mut [SessionView]) {
    let mains: Vec<(String, String)> = sessions
        .iter()
        .filter(|s| s.worktree_of.is_none())
        .filter_map(|s| s.repo_root.as_deref().map(|r| (r.to_lowercase(), s.id.clone())))
        .collect();
    let roots: Vec<Option<String>> = sessions.iter().map(|s| s.repo_root.as_deref().map(str::to_lowercase)).collect();
    for (s, root) in sessions.iter_mut().zip(roots) {
        if s.worktree_of.is_some()
            && let Some(r) = root
        {
            s.parent = mains.iter().find(|(mr, id)| *mr == r && *id != s.id).map(|(_, id)| id.clone());
        }
    }
}

/// The quick-open's folders, from the host's session folders.
fn folders(sessions: &[SessionView]) -> Vec<FolderSuggestion> {
    let mut out: Vec<FolderSuggestion> = Vec::new();
    for s in sessions.iter().filter(|s| s.kind == Kind::Local && !s.folder.is_empty()) {
        let live = u32::from(s.status.is_live());
        match out.iter_mut().find(|f| f.path.eq_ignore_ascii_case(&s.folder)) {
            Some(f) => f.open += live,
            None => {
                let name = s.folder.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or_default().to_string();
                out.push(FolderSuggestion { name, path: s.folder.clone(), branch: s.branch.clone(), tag: "In the sidebar".into(), open: live, recent: true })
            }
        }
    }
    out
}

fn wire_view(v: WireView) -> EngineView {
    match v {
        WireView::Rail => EngineView::Rail,
        WireView::Headers => EngineView::Headers,
        WireView::Flat => EngineView::Flat,
    }
}

fn inspector(t: CoreTab) -> InspectorTab {
    match t {
        CoreTab::Notes => InspectorTab::Notes,
        CoreTab::Runs => InspectorTab::Runs,
        CoreTab::Git => InspectorTab::Git,
        CoreTab::Session => InspectorTab::Session,
        CoreTab::Usage => InspectorTab::Usage,
        CoreTab::Docs => InspectorTab::Docs,
    }
}

/// The whole model for one host snapshot and this client's own view state. The group rows, the
/// layouts, the rail, the focus and the inspector come from `overlay`; the sessions, their status,
/// git and runs come from the host.
pub fn model(snap: &WireSnapshot, overlay: &ViewOverlay, theme: ThemeKind, now_ms: i64) -> UiModel {
    let mut sessions: Vec<SessionView> = snap.sessions.iter().map(|s| session(s, now_ms)).collect();
    worktree_parents(&mut sessions);
    let groups = snap
        .groups
        .iter()
        .map(|g| GroupView { id: g.id.clone(), name: g.name.clone(), torn_off: overlay.torn_off.contains(&g.id), collapsed: overlay.collapsed.contains(&g.id) })
        .collect();
    let layouts: BTreeMap<String, Layout> = overlay.layouts.iter().map(|(k, l)| (scope_key_from_engine(k), layout_from_core(l))).collect();
    UiModel {
        group_mode: match overlay.view {
            EngineView::Rail => GroupMode::Rail,
            EngineView::Headers => GroupMode::Headers,
            EngineView::Flat => GroupMode::Flat,
        },
        rail: overlay.rail_tab.clone().map_or(Scope::All, Scope::Group),
        layouts,
        active: overlay.focused.clone(),
        inspector: overlay.inspector.map(inspector),
        inbox_panel: InboxPanel {
            open: overlay.inbox_open,
            height: overlay.inbox_height.filter(|h| h.is_finite()).unwrap_or(csm_ui::theme::size::INBOX_DEFAULT_H),
        },
        inspector_width: snap.inspector_width.filter(|w| w.is_finite()),
        folders: folders(&sessions),
        windows: overlay.windows.clone(),
        open_with: snap
            .open_with
            .iter()
            .map(|e| OpenWithEntryView {
                id: e.id.clone(),
                label: e.label.clone(),
                executable: e.executable.clone(),
                program: e.program.clone(),
                local_args: e.local_args.clone(),
                wsl_args: e.wsl_args.clone(),
                ssh_args: e.ssh_args.clone(),
                is_default: e.is_default,
            })
            .collect(),
        recently_closed: snap.recently_closed.iter().map(|c| ClosedSessionView { name: c.name.clone(), subtitle: c.subtitle.clone(), command: c.command.clone() }).collect(),
        theme,
        now_ms: u64::try_from(now_ms).unwrap_or(0),
        sessions,
        groups,
        ..UiModel::default()
    }
}

/// The facts the overlay checks a view call against: the host's groups and sessions, and this
/// client's own theme list.
pub fn facts(snap: Option<&WireSnapshot>, themes: &[ThemeFact]) -> ViewFacts {
    let Some(snap) = snap else { return ViewFacts::default().with_themes(themes.to_vec()) };
    ViewFacts {
        groups: snap.groups.iter().map(|g| g.id.clone()).collect(),
        sessions: snap.sessions.iter().map(|s| SessionFact { id: s.id.clone(), group: s.group_id.clone(), asleep: s.asleep }).collect(),
        themes: themes.to_vec(),
    }
}

/// Seeds a client that has no saved view state with the host's rail, view, focus and collapse
/// state. After that the client keeps its own.
pub fn seed_overlay(snap: &WireSnapshot, overlay: &mut ViewOverlay) {
    overlay.view = wire_view(snap.view);
    overlay.rail_tab = snap.rail_tab.clone();
    overlay.focused = snap.focused.clone();
    overlay.inbox_open = snap.inbox_open;
    overlay.inbox_height = snap.inbox_height;
    overlay.collapsed = snap.groups.iter().filter(|g| !g.expanded).map(|g| g.id.clone()).collect();
    overlay.torn_off = snap.groups.iter().filter(|g| g.torn_off).map(|g| g.id.clone()).collect();
}

/// A host event as the engine's event type (the UI's `on_event` takes that). `None` for an event
/// this client does not show.
pub fn event(e: &WireEvent) -> Option<Event> {
    Some(match e {
        WireEvent::SessionChanged { session_id } => Event::SessionChanged { session_id: session_id.clone() },
        WireEvent::SessionRemoved { session_id } => Event::SessionRemoved { session_id: session_id.clone() },
        WireEvent::LayoutChanged => Event::LayoutChanged,
        WireEvent::InboxChanged => Event::InboxChanged,
        WireEvent::FocusRequested { session_id } => Event::FocusRequested { session_id: session_id.clone() },
        WireEvent::Toast { message } => Event::Toast { message: message.clone() },
        WireEvent::ConsentRequested { request } => Event::ConsentRequested { request: request.clone() },
        WireEvent::ConsentResolved { request_id } => Event::ConsentResolved { request_id: *request_id },
        WireEvent::Error { message } => Event::Error { message: message.clone() },
        WireEvent::QuitProgress { session_id, phase, elapsed_ms } => Event::QuitProgress { session_id: session_id.clone(), phase: phase.clone(), elapsed_ms: *elapsed_ms },
        WireEvent::QuitReady => Event::QuitReady,
        WireEvent::WindowShowRequested => Event::WindowShowRequested,
        WireEvent::NotificationTestRequested => Event::NotificationTestRequested,
        WireEvent::ClipboardOffered { session_id } => Event::ClipboardOffered { session_id: session_id.clone() },
        WireEvent::ScreenChanged { instance } => Event::ScreenChanged { instance: instance.clone() },
        WireEvent::ScreenFocusRequested { instance } => Event::ScreenFocusRequested { instance: instance.clone() },
        WireEvent::SettingChanged { key, value } => Event::SettingChanged { key: key.clone(), value: value.clone() },
        _ => return None,
    })
}

/// A host's answer as the engine's reply type.
pub fn reply(o: Outcome) -> Reply {
    match o {
        Outcome::Ok { message, data } => Ok(ToolOutput::new(message, data)),
        Outcome::Err { kind, message } => Err(match kind {
            ErrorKind::UnknownTool => ToolError::UnknownTool(message),
            ErrorKind::InvalidParams => ToolError::InvalidParams(message),
            ErrorKind::NotFound => ToolError::NotFound(message),
            ErrorKind::Refused | ErrorKind::Forbidden => ToolError::Refused(message),
            ErrorKind::Declined => ToolError::Declined(message),
            ErrorKind::Failed => ToolError::Failed(message),
            ErrorKind::TimedOut => ToolError::TimedOut(message),
        }),
    }
}

/// The reply for a call whose answer will never come (the connection ended, or it is not up).
pub fn lost(why: &str) -> Reply {
    Err(ToolError::Failed(why.to_string()))
}

/// The host's `tool_list` rows (`id`, `title`, `summary`, `toolset`, `effects`) as tool infos. The
/// schema is not in `tool_list`, so every tool has `Value::Null` here. Rows that do not parse are
/// skipped.
pub fn tools(data: &Value) -> Vec<ToolInfo> {
    let Some(rows) = data.as_array() else { return Vec::new() };
    rows.iter()
        .filter_map(|r| {
            let toolset: Toolset = serde_json::from_value(r.get("toolset")?.clone()).ok()?;
            let effects = effects(r.get("effects")?)?;
            Some(ToolInfo {
                id: r.get("id")?.as_str()?.to_owned(),
                title: r.get("title")?.as_str()?.to_owned(),
                summary: r.get("summary")?.as_str()?.to_owned(),
                toolset,
                effects,
                input_schema: Value::Null,
            })
        })
        .collect()
}

/// The effects a host row declares. The host writes them as flag names joined with `|`; an
/// unknown name, or a bit this client does not know, means "every effect": the tool is then
/// forwarded to the host rather than taken as UI-only.
fn effects(v: &Value) -> Option<Effects> {
    match v {
        Value::Number(n) => Some(Effects::from_bits_retain(u16::try_from(n.as_u64()?).ok()?)),
        Value::String(s) => {
            let mut out = Effects::empty();
            for part in s.split('|').map(str::trim).filter(|p| !p.is_empty()) {
                match Effects::from_name(part) {
                    Some(flag) => out |= flag,
                    None => return Some(Effects::all()),
                }
            }
            Some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_host_status_has_a_ui_status_and_an_exited_process_is_idle() {
        assert_eq!(status(WireStatus::NeedsYouApproval), Status::Approval);
        assert_eq!(status(WireStatus::NeedsYouQuestion), Status::Question);
        assert_eq!(status(WireStatus::DoneUnseen), Status::Done);
        assert_eq!(status(WireStatus::Asleep), Status::Asleep);
        assert_eq!(status(WireStatus::Exited), Status::Idle);
    }

    #[test]
    fn a_host_error_keeps_its_kind() {
        let kinds = [
            (ErrorKind::UnknownTool, "unknown"),
            (ErrorKind::InvalidParams, "bad"),
            (ErrorKind::NotFound, "missing"),
            (ErrorKind::Refused, "refused"),
            (ErrorKind::Declined, "declined"),
            (ErrorKind::Failed, "failed"),
            (ErrorKind::TimedOut, "slow"),
            (ErrorKind::Forbidden, "forbidden"),
        ];
        for (kind, message) in kinds {
            let err = reply(Outcome::Err { kind, message: message.into() }).unwrap_err();
            assert!(err.to_string().contains(message), "{kind:?} keeps its message");
        }
        assert!(matches!(reply(Outcome::Err { kind: ErrorKind::Forbidden, message: String::new() }), Err(ToolError::Refused(_))));
        let ok = reply(Outcome::Ok { message: "done".into(), data: json!({"a": 1}) }).unwrap();
        assert_eq!(ok.message, "done");
        assert_eq!(ok.data, json!({"a": 1}));
        assert!(lost("gone").is_err());
    }

    #[test]
    fn effects_are_read_from_flag_names_and_an_unknown_flag_means_every_effect() {
        assert_eq!(effects(&json!("IDEMPOTENT")), Some(Effects::IDEMPOTENT));
        assert_eq!(effects(&json!("CHANGES_UI_STATE | IDEMPOTENT")), Some(Effects::CHANGES_UI_STATE | Effects::IDEMPOTENT));
        assert_eq!(effects(&json!("")), Some(Effects::empty()));
        assert_eq!(effects(&json!("SOMETHING_NEW")), Some(Effects::all()), "forwarded, not taken as UI-only");
        assert_eq!(effects(&json!(2)), Some(Effects::CHANGES_UI_STATE));
        assert_eq!(effects(&json!(null)), None);
    }

    #[test]
    fn the_tool_list_rows_become_tool_infos_and_bad_rows_are_skipped() {
        let data = json!([
            {"id": "view_set", "title": "View", "summary": "s", "toolset": "core", "effects": "CHANGES_UI_STATE | IDEMPOTENT"},
            {"id": "broken", "title": "x", "summary": "s", "toolset": "no_such_set", "effects": "IDEMPOTENT"},
            {"title": "no id"},
        ]);
        let list = tools(&data);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "view_set");
        assert_eq!(list[0].toolset, Toolset::Core);
        assert_eq!(list[0].effects, Effects::CHANGES_UI_STATE | Effects::IDEMPOTENT);
        assert!(tools(&json!({"not": "a list"})).is_empty());
    }

    #[test]
    fn host_events_map_to_the_ui_events_and_unknown_ones_are_dropped() {
        let e = event(&WireEvent::Toast { message: "hi".into() });
        assert_eq!(e, Some(Event::Toast { message: "hi".into() }));
        assert_eq!(event(&WireEvent::FocusRequested { session_id: "s".into() }), Some(Event::FocusRequested { session_id: "s".into() }));
        assert_eq!(event(&WireEvent::ConsentResolved { request_id: 7 }), Some(Event::ConsentResolved { request_id: 7 }));
    }
}

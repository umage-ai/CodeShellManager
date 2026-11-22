//! Conversions from the engine's read model and events to the owned wire types of
//! `csm-peer-proto`. Pure mapping: no I/O, no locks, no clock.
//!
//! This is the one place the two shapes meet. `csm-peer-proto` sits below the engine and cannot
//! name these types, so the `From` impls live here (the engine owns the source types). Every
//! field is copied by name; a test below serialises each engine value and checks the wire value
//! produces the same JSON, so a drift in either shape fails the build.

use csm_peer_proto as wire;

use crate::{ClosedView, Event, GroupInfo, GroupView, OpenWithTarget, OpenWithView, RunPhase, RunView, SessionView, Snapshot, Status};

impl From<Status> for wire::Status {
    fn from(s: Status) -> Self {
        match s {
            Status::Starting => wire::Status::Starting,
            Status::Working => wire::Status::Working,
            Status::NeedsYouApproval => wire::Status::NeedsYouApproval,
            Status::NeedsYouQuestion => wire::Status::NeedsYouQuestion,
            Status::DoneUnseen => wire::Status::DoneUnseen,
            Status::Error => wire::Status::Error,
            Status::Idle => wire::Status::Idle,
            Status::Asleep => wire::Status::Asleep,
            Status::Exited => wire::Status::Exited,
        }
    }
}

impl From<GroupView> for wire::GroupView {
    fn from(v: GroupView) -> Self {
        match v {
            GroupView::Rail => wire::GroupView::Rail,
            GroupView::Headers => wire::GroupView::Headers,
            GroupView::Flat => wire::GroupView::Flat,
        }
    }
}

impl From<RunPhase> for wire::RunPhase {
    fn from(p: RunPhase) -> Self {
        match p {
            RunPhase::Idle => wire::RunPhase::Idle,
            RunPhase::Starting => wire::RunPhase::Starting,
            RunPhase::Running => wire::RunPhase::Running,
            RunPhase::Ok => wire::RunPhase::Ok,
            RunPhase::Failed => wire::RunPhase::Failed,
            RunPhase::Stopped => wire::RunPhase::Stopped,
        }
    }
}

impl From<&RunView> for wire::RunView {
    fn from(r: &RunView) -> Self {
        wire::RunView {
            id: r.id.clone(),
            label: r.label.clone(),
            command_line: r.command_line.clone(),
            is_default: r.is_default,
            mode: r.mode.to_owned(),
            post_run_url: r.post_run_url.clone(),
            phase: r.phase.into(),
            exit_code: r.exit_code,
            started_ms: r.started_ms,
            ended_ms: r.ended_ms,
        }
    }
}

impl From<&OpenWithTarget> for wire::OpenWithTarget {
    fn from(t: &OpenWithTarget) -> Self {
        wire::OpenWithTarget {
            entry_id: t.entry_id.clone(),
            label: t.label.clone(),
            program: t.program.clone(),
            is_default: t.is_default,
            command: t.command.clone(),
        }
    }
}

impl From<&OpenWithView> for wire::OpenWithEntry {
    fn from(e: &OpenWithView) -> Self {
        wire::OpenWithEntry {
            id: e.id.clone(),
            label: e.label.clone(),
            executable: e.executable.clone(),
            program: e.program.clone(),
            local_args: e.local_args.clone(),
            wsl_args: e.wsl_args.clone(),
            ssh_args: e.ssh_args.clone(),
            is_default: e.is_default,
        }
    }
}

impl From<&SessionView> for wire::SessionView {
    fn from(s: &SessionView) -> Self {
        wire::SessionView {
            id: s.id.clone(),
            name: s.name.clone(),
            kind: s.kind.to_owned(),
            folder: s.folder.clone(),
            folder_short: s.folder_short.clone(),
            annotation: s.annotation.clone(),
            owner_addon: s.owner_addon.clone(),
            command: s.command.clone(),
            group_id: s.group_id.clone(),
            accent: s.accent.clone(),
            accent_override: s.accent_override,
            status: s.status.into(),
            status_label: s.status_label.to_owned(),
            progress: s.progress,
            status_message: s.status_message.clone(),
            live: s.live,
            asleep: s.asleep,
            exit_code: s.exit_code,
            branch: s.branch.clone(),
            git_dirty: s.git_dirty,
            git_upstream: s.git_upstream.clone(),
            git_ahead: s.git_ahead,
            git_behind: s.git_behind,
            git_changes: s.git_changes,
            git_summary: s.git_summary.clone(),
            worktree_of: s.worktree_of.clone(),
            repo_root: s.repo_root.clone(),
            git_busy: s.git_busy,
            title: s.title.clone(),
            is_claude: s.is_claude,
            claude_session_id: s.claude_session_id.clone(),
            reports_status: s.reports_status,
            remote_control: s.remote_control,
            remote_control_url: s.remote_control_url.clone(),
            last_activity_ms: s.last_activity_ms,
            inbox_since_ms: s.inbox_since_ms,
            status_since_ms: s.status_since_ms,
            project: s.project.clone(),
            project_name: s.project_name.clone(),
            backend: s.backend.map(str::to_owned),
            runs: s.runs.iter().map(wire::RunView::from).collect(),
            open_with: s.open_with.iter().map(wire::OpenWithTarget::from).collect(),
            subagents_running: s.subagents_running,
            subagents_total: s.subagents_total,
        }
    }
}

impl From<&GroupInfo> for wire::GroupInfo {
    fn from(g: &GroupInfo) -> Self {
        wire::GroupInfo {
            id: g.id.clone(),
            name: g.name.clone(),
            expanded: g.expanded,
            torn_off: g.torn_off,
            session_count: g.session_count,
            needs_you: g.needs_you,
            unseen: g.unseen,
        }
    }
}

impl From<&ClosedView> for wire::ClosedView {
    fn from(c: &ClosedView) -> Self {
        wire::ClosedView { name: c.name.clone(), subtitle: c.subtitle.clone(), command: c.command.clone() }
    }
}

impl From<&Snapshot> for wire::Snapshot {
    fn from(s: &Snapshot) -> Self {
        wire::Snapshot {
            version: s.version,
            sessions: s.sessions.iter().map(wire::SessionView::from).collect(),
            groups: s.groups.iter().map(wire::GroupInfo::from).collect(),
            view: s.view.into(),
            rail_tab: s.rail_tab.clone(),
            layout_key: s.layout_key.clone(),
            inbox: s.inbox.clone(),
            focused: s.focused.clone(),
            inbox_open: s.inbox_open,
            inbox_height: s.inbox_height,
            recently_closed: s.recently_closed.iter().map(wire::ClosedView::from).collect(),
            open_with: s.open_with.iter().map(wire::OpenWithEntry::from).collect(),
        }
    }
}

impl From<&Event> for wire::Event {
    fn from(e: &Event) -> Self {
        match e {
            Event::SessionChanged { session_id } => wire::Event::SessionChanged { session_id: session_id.clone() },
            Event::SessionRemoved { session_id } => wire::Event::SessionRemoved { session_id: session_id.clone() },
            Event::LayoutChanged => wire::Event::LayoutChanged,
            Event::InboxChanged => wire::Event::InboxChanged,
            Event::FocusRequested { session_id } => wire::Event::FocusRequested { session_id: session_id.clone() },
            Event::Toast { message } => wire::Event::Toast { message: message.clone() },
            Event::ConsentRequested { request } => wire::Event::ConsentRequested { request: request.clone() },
            Event::ConsentResolved { request_id } => wire::Event::ConsentResolved { request_id: *request_id },
            Event::Error { message } => wire::Event::Error { message: message.clone() },
            Event::QuitProgress { session_id, phase, elapsed_ms } => {
                wire::Event::QuitProgress { session_id: session_id.clone(), phase: phase.clone(), elapsed_ms: *elapsed_ms }
            }
            Event::QuitReady => wire::Event::QuitReady,
            Event::WindowShowRequested => wire::Event::WindowShowRequested,
            Event::NotificationTestRequested => wire::Event::NotificationTestRequested,
            Event::ClipboardOffered { session_id } => wire::Event::ClipboardOffered { session_id: session_id.clone() },
            Event::ScreenChanged { instance } => wire::Event::ScreenChanged { instance: instance.clone() },
            Event::ScreenFocusRequested { instance } => wire::Event::ScreenFocusRequested { instance: instance.clone() },
            Event::SettingChanged { key, value } => wire::Event::SettingChanged { key: key.clone(), value: value.clone() },
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// Every engine status, with its wire twin: the serde names must agree.
    #[test]
    fn every_status_serialises_like_the_engine() {
        let all = [
            Status::Starting,
            Status::Working,
            Status::NeedsYouApproval,
            Status::NeedsYouQuestion,
            Status::DoneUnseen,
            Status::Error,
            Status::Idle,
            Status::Asleep,
            Status::Exited,
        ];
        for s in all {
            let engine = serde_json::to_value(s).unwrap();
            let wire = serde_json::to_value(wire::Status::from(s)).unwrap();
            assert_eq!(engine, wire, "{s:?}");
            assert_eq!(engine, json!(s.id()), "{s:?} is its own id");
        }
    }

    #[test]
    fn every_run_phase_serialises_like_the_engine() {
        for p in [RunPhase::Idle, RunPhase::Starting, RunPhase::Running, RunPhase::Ok, RunPhase::Failed, RunPhase::Stopped] {
            assert_eq!(serde_json::to_value(p).unwrap(), serde_json::to_value(wire::RunPhase::from(p)).unwrap(), "{p:?}");
        }
    }

    #[test]
    fn every_event_serialises_like_the_engine_and_reads_back() {
        let events = vec![
            Event::SessionChanged { session_id: "s1".into() },
            Event::SessionRemoved { session_id: "s1".into() },
            Event::LayoutChanged,
            Event::InboxChanged,
            Event::FocusRequested { session_id: "s1".into() },
            Event::Toast { message: "Started claude.".into() },
            Event::ConsentRequested { request: json!({"tool": "session_send_text", "id": 4}) },
            Event::ConsentResolved { request_id: 4 },
            Event::Error { message: "boom".into() },
            Event::QuitProgress { session_id: "s1".into(), phase: "forced".into(), elapsed_ms: 1500 },
            Event::QuitReady,
            Event::WindowShowRequested,
            Event::NotificationTestRequested,
            Event::ClipboardOffered { session_id: "s1".into() },
            Event::ScreenChanged { instance: "screen-1".into() },
            Event::ScreenFocusRequested { instance: "screen-1".into() },
            Event::SettingChanged { key: "ui_zoom".into(), value: json!(1.25) },
        ];
        for e in &events {
            let engine: Value = serde_json::to_value(e).unwrap();
            let converted = wire::Event::from(e);
            assert_eq!(engine, serde_json::to_value(&converted).unwrap(), "{e:?}");
            let back: wire::Event = serde_json::from_value(engine).unwrap();
            assert_eq!(back, converted, "{e:?} reads back");
        }
    }
}

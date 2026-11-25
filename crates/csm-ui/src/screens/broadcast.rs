//! "Send to all sessions…" on a group (#59): a one-line prompt for the text, then a confirm that
//! says how many sessions will get it, then `session_send_text_many { group_id, text, submit }`.
//! The engine pastes (bracketed, never typed) into each live session and skips the ones that
//! are asleep or working mid-turn; the reply's sentence is the toast.

use serde_json::json;

use crate::app::{Action, ConfirmState};
use crate::model::{Scope, Status, UiModel};

/// How many of the group's sessions would take the text now, and how many are busy mid-turn
/// (skipped). Asleep ones are neither.
pub fn counts(model: &UiModel, group: &str) -> (usize, usize) {
    let scope = Scope::Group(group.to_string());
    let live: Vec<Status> = model.scope_sessions(&scope).iter().map(|s| s.status).filter(|s| s.is_live() && *s != Status::Starting).collect();
    let busy = live.iter().filter(|s| **s == Status::Working).count();
    (live.len() - busy, busy)
}

pub fn title(model: &UiModel, group: &str) -> String {
    match model.group(group) {
        Some(g) => format!("Send to all sessions in {}", g.name),
        None => "Send to all sessions".to_string(),
    }
}

/// The confirm shown after the text is typed.
pub fn confirm(model: &UiModel, group: &str, text: &str) -> Action {
    let (ready, busy) = counts(model, group);
    let name = model.group(group).map_or_else(|| "the group".to_string(), |g| g.name.clone());
    if ready == 0 {
        // "Send to 0 sessions?" would be a question with one honest answer.
        let why = if busy > 0 { format!("the {busy} running are working mid-turn") } else { "none is running".to_string() };
        return Action::Toast(format!("No session in {name} can take the text now: {why}."));
    }
    let shown: String = text.chars().take(80).collect();
    let more = if text.chars().count() > 80 { "…" } else { "" };
    let mut body = format!("“{shown}{more}” is pasted into each live session in {name}, and Enter is pressed.");
    if busy > 0 {
        body.push_str(&format!(" {busy} working mid-turn will be skipped."));
    }
    body.push_str(" Sleeping sessions are skipped.");
    let plural = if ready == 1 { "" } else { "s" };
    Action::Confirm(ConfirmState {
        safe_default: false,
        also: None,
        title: format!("Send to {ready} session{plural}?"),
        body,
        yes_label: format!("Send to {ready}"),
        no_label: "Cancel".into(),
        yes: Box::new(Action::Tool("session_send_text_many", json!({"group_id": group, "text": text, "submit": true}))),
    })
}

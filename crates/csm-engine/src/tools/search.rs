//! `notes` and `search` tools: the per-folder notes, full-text search of terminal output, and
//! reopening a session that search found but that is gone.
//!
//! The index lives on its own thread (`csm-index`), so none of these block the engine: each
//! one parks its reply, hands the query to the index thread and finishes from the callback.
//! Everything that needs session state (names, which ids are in a group, who shares a folder)
//! is read here on the engine thread *before* the query goes out and moved into the callback.
//!
//! User query text is untrusted: it is quoted as one FTS5 phrase unless `advanced` is set, and
//! a syntax error in advanced mode comes back as a plain message.

use std::collections::HashMap;

use csm_core::models::{RecentlyClosedEntry, SessionKind, ShellSession};
use csm_index::{Index, IndexError, fts_phrase, normalize_folder};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{find, find_group};
use crate::engine::{Ctx, ParkedReply, Reply};
use crate::state::State;
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

/// Longest note accepted (256 KiB): a note is a scratchpad, not a document store.
const MAX_NOTE_BYTES: usize = 256 * 1024;
const MAX_QUERY_CHARS: usize = 500;
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;

pub(super) fn specs() -> Vec<ToolSpec> {
    let content = Effects::IDEMPOTENT.union(Effects::READS_SESSION_CONTENT);
    vec![
        ToolSpec {
            id: "note_get",
            title: "Notes",
            summary: "The note kept for a folder (give folder, or session_id to use that session's folder). Sessions in one folder share one note; SSH sessions have none.",
            toolset: Toolset::Notes,
            effects: content,
            input_schema: schema_of::<NoteGetParams>,
            run: note_get,
        },
        ToolSpec {
            id: "note_set",
            title: "Save note",
            summary: "Replace the note kept for a folder (folder or session_id). An empty text deletes the note. Refused for SSH sessions and sessions without a folder.",
            toolset: Toolset::Notes,
            effects: Effects::MUTATES_STATE,
            input_schema: schema_of::<NoteSetParams>,
            run: note_set,
        },
        ToolSpec {
            id: "notes_import_wpf",
            title: "Import notes from CodeShellManager (WPF)…",
            summary: "Import the per-folder notes from the WPF app's output.db (default: its usual location). The file is opened read-only; a note already here is kept unless the imported one is newer.",
            toolset: Toolset::Notes,
            effects: Effects::MUTATES_STATE.union(Effects::READS_SESSION_CONTENT),
            input_schema: schema_of::<ImportParams>,
            run: notes_import_wpf,
        },
        ToolSpec {
            id: "search_output",
            title: "Search all output",
            summary: "Full-text search of everything every session printed (ANSI stripped). The query is taken literally as a phrase unless advanced is true (FTS5 syntax: AND, OR, NOT, prefix*). Newest matches first.",
            toolset: Toolset::Search,
            effects: content,
            input_schema: schema_of::<SearchOutputParams>,
            run: search_output,
        },
        ToolSpec {
            id: "search_notes",
            title: "Search notes",
            summary: "Search every folder's notes for a piece of text (case-insensitive, literal).",
            toolset: Toolset::Search,
            effects: content,
            input_schema: schema_of::<SearchNotesParams>,
            run: search_notes,
        },
        ToolSpec {
            id: "search_reopen_session",
            title: "Reopen a session from search",
            summary: "Start a new session from the configuration a closed session had (session_id from a search result). Refused when that session is still in the sidebar.",
            toolset: Toolset::Search,
            effects: Effects::MUTATES_STATE.union(Effects::LAUNCHES_PROCESS),
            input_schema: schema_of::<ReopenParams>,
            run: search_reopen_session,
        },
    ]
}

// ── Helpers ──────────────────────────────────────────────────────────────────────────────

fn index_of(ctx: &Ctx<'_>) -> Result<Index, ToolError> {
    ctx.state.rt.index.clone().ok_or_else(|| {
        ToolError::Refused("search and notes are not available in this run (there is no search index).".into())
    })
}

fn park(ctx: &mut Ctx<'_>) -> Result<ParkedReply, ToolError> {
    ctx.park().ok_or_else(|| ToolError::Failed("this tool cannot run outside a call".into()))
}

fn tool_err(e: IndexError) -> ToolError {
    match e {
        IndexError::Message(m) => ToolError::InvalidParams(m),
        IndexError::Db(m) | IndexError::Unavailable(m) => ToolError::Failed(m),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The folder a session keeps its note under, or why it has none.
fn session_note_folder(s: &ShellSession) -> Result<String, ToolError> {
    if s.kind == SessionKind::Ssh {
        return Err(ToolError::Refused("SSH sessions don't keep notes: there is no local folder to key them by.".into()));
    }
    if s.working_folder.trim().is_empty() {
        return Err(ToolError::Refused("this session has no folder, so there is no note to keep.".into()));
    }
    Ok(s.working_folder.clone())
}

fn resolve_folder(state: &State, folder: Option<String>, session_id: Option<String>) -> Result<String, ToolError> {
    match (folder, session_id) {
        (Some(_), Some(_)) => Err(ToolError::InvalidParams("give either folder or session_id, not both".into())),
        (None, None) => Err(ToolError::InvalidParams("give a folder or a session_id".into())),
        (Some(f), None) => {
            normalize_folder(&f).ok_or_else(|| ToolError::InvalidParams("the folder is empty".into()))?;
            Ok(f)
        }
        (None, Some(id)) => {
            let i = find(state, &id)?;
            let s = state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound(format!("no session `{id}`")))?;
            session_note_folder(s)
        }
    }
}

/// Live and sleeping sessions that keep their note under this normalised folder.
fn sessions_in(state: &State, key: &str) -> Vec<String> {
    state
        .app
        .sessions
        .iter()
        .filter(|s| s.kind != SessionKind::Ssh && normalize_folder(&s.working_folder).as_deref() == Some(key))
        .map(|s| s.id.clone())
        .collect()
}

fn clamp_limit(limit: Option<u32>) -> usize {
    usize::try_from(limit.unwrap_or(0)).ok().filter(|n| *n > 0).map_or(DEFAULT_LIMIT, |n| n.min(MAX_LIMIT))
}

fn check_query(q: &str) -> Result<&str, ToolError> {
    let t = q.trim();
    if t.is_empty() {
        return Err(ToolError::InvalidParams("type something to search for".into()));
    }
    if t.chars().count() > MAX_QUERY_CHARS {
        return Err(ToolError::InvalidParams(format!("the query is too long (over {MAX_QUERY_CHARS} characters)")));
    }
    Ok(t)
}

// ── Notes ────────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct NoteGetParams {
    /// The folder the note is kept for. Give this or `session_id`.
    folder: Option<String>,
    /// Use this session's folder (refused for SSH sessions and sessions without a folder).
    session_id: Option<String>,
}

fn note_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NoteGetParams = parse_params(params)?;
    let folder = resolve_folder(ctx.state, p.folder, p.session_id)?;
    let key = normalize_folder(&folder).unwrap_or_default();
    let shared = sessions_in(ctx.state, &key).len();
    let ix = index_of(ctx)?;
    let reply = park(ctx)?;
    ix.call(
        move |db| db.note_get(&folder),
        move |r| {
            reply.send(match r {
                Ok(row) => {
                    let (text, at) = row.map_or((String::new(), 0), |n| (n.text, n.updated_at));
                    let msg = if text.is_empty() { "No note for this folder yet.".to_owned() } else { format!("Note for {key}.") };
                    Ok(ToolOutput::new(msg, json!({"folder": key, "text": text, "updated_at": at, "shared_by": shared})))
                }
                Err(IndexError::Message(m)) => Err(ToolError::Refused(m)),
                Err(e) => Err(tool_err(e)),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteSetParams {
    /// The folder to keep the note for. Give this or `session_id`.
    folder: Option<String>,
    /// Use this session's folder.
    session_id: Option<String>,
    /// The whole note. Empty deletes it. At most 256 KiB.
    text: String,
}

fn note_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NoteSetParams = parse_params(params)?;
    if p.text.len() > MAX_NOTE_BYTES {
        return Err(ToolError::InvalidParams(format!("the note is too long ({} KiB is the most)", MAX_NOTE_BYTES / 1024)));
    }
    let folder = resolve_folder(ctx.state, p.folder, p.session_id)?;
    let key = normalize_folder(&folder).unwrap_or_default();
    let shared = sessions_in(ctx.state, &key).len();
    // Notes are plain text: keep newlines and tabs, drop other control characters.
    let text: String = p.text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t' | '\r')).collect();
    let ix = index_of(ctx)?;
    let reply = park(ctx)?;
    let now = crate::session::now_ms();
    let deleting = text.trim().is_empty();
    ix.call(
        move |db| db.note_set(&folder, &text, now),
        move |r| {
            reply.send(match r {
                Ok(()) => {
                    let msg = if deleting { "Deleted the note." } else { "Saved the note." };
                    Ok(ToolOutput::new(msg, json!({"folder": key, "shared_by": shared, "updated_at": now})))
                }
                Err(IndexError::Message(m)) => Err(ToolError::Refused(m)),
                Err(e) => Err(tool_err(e)),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ImportParams {
    /// The WPF app's `output.db`. Default: `%AppData%\CodeShellManager\output.db`.
    path: Option<String>,
}

fn notes_import_wpf(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ImportParams = parse_params(params)?;
    let path = match p.path.filter(|s| !s.trim().is_empty()) {
        Some(s) => std::path::PathBuf::from(s.trim()),
        None => crate::index::wpf_output_db().ok_or_else(|| ToolError::NotFound("could not find the WPF app's data folder; pass path".into()))?,
    };
    let ix = index_of(ctx)?;
    let reply = park(ctx)?;
    let shown = path.display().to_string();
    ix.call(
        move |db| db.import_wpf_notes(&path),
        move |r| {
            reply.send(match r {
                Ok(n) => Ok(ToolOutput::new(format!("Imported {} from {shown}.", plural(n, "note", "notes")), json!({"imported": n}))),
                Err(IndexError::Message(m)) => Err(ToolError::NotFound(m)),
                Err(e) => Err(tool_err(e)),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

// ── Search ───────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchOutputParams {
    /// What to look for. Taken literally as a phrase unless `advanced` is true.
    query: String,
    /// At most this many results (default 50, at most 500).
    limit: Option<u32>,
    /// Only this session's output.
    session_id: Option<String>,
    /// Only the output of sessions in this group.
    group_id: Option<String>,
    /// Pass the query to FTS5 as written (AND, OR, NOT, NEAR, prefix*, "phrases").
    #[serde(default)]
    advanced: bool,
}

fn search_output(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SearchOutputParams = parse_params(params)?;
    let q = check_query(&p.query)?.to_owned();
    let limit = clamp_limit(p.limit);
    let mut filter: Option<Vec<String>> = None;
    if let Some(g) = &p.group_id {
        let gi = find_group(ctx.state, g)?;
        let gid = ctx.state.app.groups.get(gi).map(|x| x.id.clone()).unwrap_or_default();
        filter = Some(ctx.state.app.sessions.iter().filter(|s| s.group_id == gid).map(|s| s.id.clone()).collect());
    }
    if let Some(sid) = &p.session_id {
        filter = Some(match filter {
            Some(ids) => ids.into_iter().filter(|i| i == sid).collect(),
            None => vec![sid.clone()],
        });
    }
    let names: HashMap<String, (String, bool)> = ctx
        .state
        .app
        .sessions
        .iter()
        .map(|s| (s.id.clone(), (if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() }, s.is_dormant)))
        .collect();
    let expr = if p.advanced { q.clone() } else { fts_phrase(&q) };
    let ix = index_of(ctx)?;
    let reply = park(ctx)?;
    ix.call(
        move |db| db.search_output(&expr, limit, filter.as_deref()),
        move |r| {
            reply.send(match r {
                Ok(hits) => {
                    let mut sessions = std::collections::BTreeSet::new();
                    let results: Vec<Value> = hits
                        .iter()
                        .map(|h| {
                            sessions.insert(h.session_id.clone());
                            let live = names.get(&h.session_id);
                            json!({
                                "type": "output",
                                "session_id": h.session_id,
                                "session_name": live.map_or(h.session_name.as_str(), |(n, _)| n.as_str()),
                                "folder": h.folder,
                                "ts": h.ts,
                                "excerpt": h.excerpt,
                                "matches": h.matches,
                                "exists": live.is_some(),
                                "asleep": live.is_some_and(|(_, a)| *a),
                                "reopenable": h.reopenable,
                            })
                        })
                        .collect();
                    let msg = if results.is_empty() {
                        format!("No output matches “{q}”.")
                    } else {
                        format!("{} for “{q}” in {}.", plural(results.len(), "match", "matches"), plural(sessions.len(), "session", "sessions"))
                    };
                    Ok(ToolOutput::new(msg, json!({"query": q, "truncated": results.len() >= limit, "results": results})))
                }
                Err(e) => Err(tool_err(e)),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchNotesParams {
    /// The text to look for in notes (and in their folder names).
    query: String,
    /// At most this many results (default 50, at most 500).
    limit: Option<u32>,
}

fn search_notes(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SearchNotesParams = parse_params(params)?;
    let q = check_query(&p.query)?.to_owned();
    let limit = clamp_limit(p.limit);
    let mut by_folder: HashMap<String, Vec<String>> = HashMap::new();
    for s in &ctx.state.app.sessions {
        if s.kind != SessionKind::Ssh
            && let Some(k) = normalize_folder(&s.working_folder)
        {
            by_folder.entry(k).or_default().push(s.id.clone());
        }
    }
    let ix = index_of(ctx)?;
    let reply = park(ctx)?;
    let needle = q.clone();
    ix.call(
        move |db| db.search_notes(&needle, limit),
        move |r| {
            reply.send(match r {
                Ok(hits) => {
                    let results: Vec<Value> = hits
                        .iter()
                        .map(|h| {
                            json!({
                                "type": "note",
                                "folder": h.folder,
                                "updated_at": h.updated_at,
                                "excerpt": h.excerpt,
                                "matches": h.matches,
                                "session_ids": by_folder.get(&h.folder).cloned().unwrap_or_default(),
                            })
                        })
                        .collect();
                    let msg = if results.is_empty() {
                        format!("No note mentions “{q}”.")
                    } else {
                        format!("{} mention “{q}”.", plural(results.len(), "note", "notes"))
                    };
                    Ok(ToolOutput::new(msg, json!({"query": q, "truncated": results.len() >= limit, "results": results})))
                }
                Err(e) => Err(tool_err(e)),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReopenParams {
    /// The `session_id` of a search result whose session is gone.
    session_id: String,
}

fn search_reopen_session(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ReopenParams = parse_params(params)?;
    if ctx.state.rt.shutting_down {
        return Err(ToolError::Refused("the app is closing".into()));
    }
    if ctx.state.app.sessions.iter().any(|s| s.id == p.session_id) {
        return Err(ToolError::Refused("that session is still in the sidebar; focus it instead".into()));
    }
    let ix = index_of(ctx)?;
    let reply = park(ctx)?;
    let engine = ctx.engine.clone();
    let id = p.session_id.clone();
    ix.call(
        move |db| db.history(&id),
        move |r| match r {
            Err(e) => reply.send(Err(tool_err(e))),
            Ok(None) => reply.send(Err(ToolError::NotFound("no configuration was kept for that session, so it cannot be reopened".into()))),
            Ok(Some(json)) => {
                engine.post(Box::new(move |ctx| {
                    let Ok(entry) = serde_json::from_str::<RecentlyClosedEntry>(&json) else {
                        reply.send(Err(ToolError::Failed("the kept configuration could not be read".into())));
                        return;
                    };
                    // Reuse the reopen path: put the snapshot at the front of Recently closed and take it out again.
                    ctx.state.app.recently_closed.insert(0, entry);
                    let r = super::sessions::session_reopen_closed(ctx, json!({"index": 0}));
                    reply.send(r);
                }));
            }
        },
    );
    Ok(ToolOutput::message(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_defaults_and_clamps() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(7)), 7);
        assert_eq!(clamp_limit(Some(10_000)), MAX_LIMIT);
    }

    #[test]
    fn queries_are_checked() {
        assert!(check_query("  ").is_err());
        assert!(check_query(&"x".repeat(MAX_QUERY_CHARS + 1)).is_err());
        assert_eq!(check_query(" hi ").unwrap(), "hi");
    }
}

//! Claude's replies, from the session's transcript: `claude_reply_list`, `claude_reply_get` and
//! `claude_code_block_get` (the pane menu's "Copy last reply" / "Copy reply…", Ctrl+Shift+C).
//!
//! The text comes from the transcript the usage reader follows for every live local Claude
//! session (`usage_tail`, also with the usage add-on off), so it is what Claude wrote, not what the
//! terminal shows after wrapping. WSL and SSH sessions have no transcript CSM can read.

use std::path::PathBuf;

use csm_core::claude_reply::ClaudeReply;
use csm_core::models::SessionKind;
use serde::Deserialize;
use serde_json::{Value, json};

use super::find;
use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

/// The most replies listed or looked through.
pub(crate) const MAX_REPLIES: usize = 50;
/// Replies listed by default.
const DEFAULT_LIST: usize = 10;

pub(super) fn specs() -> Vec<ToolSpec> {
    let read = Effects::READ_ONLY.union(Effects::READS_SESSION_CONTENT);
    vec![
        ToolSpec {
            id: "claude_reply_list",
            title: "List Claude's recent replies",
            summary: "The last replies (default 10, at most 50) Claude wrote in a local Claude session, newest first (index 0 is the latest): each with its id, a one-line preview, its line count and its code blocks (language and preview). Read from the conversation's transcript; WSL and SSH sessions are not available.",
            toolset: Toolset::Core,
            effects: read,
            input_schema: schema_of::<ListParams>,
            run: claude_reply_list,
        },
        ToolSpec {
            id: "claude_reply_get",
            title: "Get Claude's reply",
            summary: "One reply Claude wrote in a local Claude session, exactly as written (Markdown; the text of the turn's assistant messages, without tool calls). index 0 (the default) is the latest; reply_id picks one from claude_reply_list instead. Read from the conversation's transcript; WSL and SSH sessions are not available.",
            toolset: Toolset::Core,
            effects: read,
            input_schema: schema_of::<GetParams>,
            run: claude_reply_get,
        },
        ToolSpec {
            id: "claude_code_block_get",
            title: "Get a code block from Claude's reply",
            summary: "One code block of a reply Claude wrote, exactly as written, without the fences: block_index (default 0, the first) of the reply at reply_index (default 0, the latest) or reply_id. Read from the conversation's transcript; WSL and SSH sessions are not available.",
            toolset: Toolset::Core,
            effects: read,
            input_schema: schema_of::<BlockParams>,
            run: claude_code_block_get,
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListParams {
    session_id: String,
    /// How many replies, newest first (default 10, at most 50).
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetParams {
    session_id: String,
    /// 0 is the latest reply, 1 the one before, … (at most 49). Default 0.
    #[serde(default)]
    index: Option<usize>,
    /// A reply's `id` from `claude_reply_list` (stays right while Claude writes more); wins over
    /// `index`.
    #[serde(default)]
    reply_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct BlockParams {
    session_id: String,
    /// The reply: 0 is the latest (the default).
    #[serde(default)]
    reply_index: Option<usize>,
    /// A reply's `id` from `claude_reply_list`; wins over `reply_index`.
    #[serde(default)]
    reply_id: Option<String>,
    /// The code block in that reply: 0 is the first (the default).
    #[serde(default)]
    block_index: Option<usize>,
}

/// The transcript of a session whose replies can be read, or why not.
fn transcript_of(ctx: &Ctx<'_>, id: &str) -> Result<PathBuf, ToolError> {
    let i = find(ctx.state, id)?;
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound(format!("no session `{id}`")))?;
    if s.kind != SessionKind::Local {
        return Err(ToolError::Refused(
            "not available for this session: Claude's replies are read from its transcript, which CSM can read only for local sessions (not WSL or SSH)".into(),
        ));
    }
    if !crate::launch::is_claude_command(&s.command) {
        return Err(ToolError::Refused("not available for this session: it does not run Claude Code".into()));
    }
    if !ctx.state.rt.sessions.contains_key(id) {
        return Err(ToolError::Refused("not available while the session is asleep: wake it first".into()));
    }
    ctx.state.rt.claude_transcripts.get(id).cloned().ok_or_else(|| {
        ToolError::NotFound("no transcript for this session yet: Claude has not written one, or CSM is still looking for it; try again in a moment".into())
    })
}

/// Reads the newest `want` replies on a worker and answers with `f` of them.
fn with_replies(
    ctx: &mut Ctx<'_>,
    session_id: &str,
    want: usize,
    f: impl FnOnce(&str, Vec<ClaudeReply>) -> Result<ToolOutput, ToolError> + Send + 'static,
) -> Reply {
    let path = transcript_of(ctx, session_id)?;
    let sid = session_id.to_owned();
    let read = move || -> Result<ToolOutput, ToolError> {
        let replies = csm_services::claude_usage::replies::read_newest(&path, want)
            .map_err(|e| ToolError::Failed(format!("could not read Claude's transcript: {e}")))?;
        f(&sid, replies)
    };
    let Some(reply) = ctx.park() else {
        return read();
    };
    let spawned = std::thread::Builder::new().name("csm-replies".into()).spawn(move || reply.send(read()));
    match spawned {
        Ok(_) => Ok(ToolOutput::message("reading")),
        Err(e) => Err(ToolError::Failed(format!("could not read Claude's transcript: {e}"))),
    }
}

fn blocks_json(r: &ClaudeReply) -> Value {
    Value::Array(
        r.code_blocks()
            .iter()
            .enumerate()
            .map(|(i, b)| json!({"index": i, "lang": b.lang, "lines": b.code.lines().count(), "preview": b.preview()}))
            .collect(),
    )
}

fn claude_reply_list(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ListParams = parse_params(params)?;
    let n = p.limit.unwrap_or(DEFAULT_LIST);
    if n == 0 || n > MAX_REPLIES {
        return Err(ToolError::InvalidParams(format!("limit is 1 to {MAX_REPLIES}")));
    }
    with_replies(ctx, &p.session_id, n, |sid, replies| {
        let rows: Vec<Value> = replies
            .iter()
            .enumerate()
            .map(|(i, r)| {
                json!({"index": i, "id": r.id, "timestamp": r.timestamp, "preview": r.preview(), "lines": r.line_count(), "code_blocks": blocks_json(r)})
            })
            .collect();
        let msg = match rows.len() {
            0 => "Claude has not replied yet".to_owned(),
            1 => "1 reply".to_owned(),
            k => format!("{k} replies"),
        };
        Ok(ToolOutput::new(msg, json!({"session_id": sid, "replies": rows})))
    })
}

/// The reply picked by `reply_id`, else by `index` (0 is the latest), with its index.
fn pick(replies: Vec<ClaudeReply>, index: usize, reply_id: Option<&str>) -> Result<(usize, ClaudeReply), ToolError> {
    if replies.is_empty() {
        return Err(ToolError::NotFound("Claude has not replied yet in this session".into()));
    }
    let found = match reply_id {
        Some(id) => replies.into_iter().enumerate().find(|(_, r)| r.id == id),
        None => replies.into_iter().enumerate().nth(index),
    };
    found.ok_or_else(|| match reply_id {
        Some(id) => ToolError::NotFound(format!("no reply `{id}` among the last {MAX_REPLIES}; claude_reply_list shows the ids")),
        None => ToolError::NotFound(format!("there is no reply {index}; claude_reply_list shows what there is")),
    })
}

/// How many replies to read to find the one asked for.
fn want_for(index: usize, reply_id: Option<&str>) -> Result<usize, ToolError> {
    if reply_id.is_some() {
        return Ok(MAX_REPLIES);
    }
    if index >= MAX_REPLIES {
        return Err(ToolError::InvalidParams(format!("index is 0 to {}", MAX_REPLIES - 1)));
    }
    Ok(index + 1)
}

fn lines_word(n: usize) -> String {
    if n == 1 { "1 line".into() } else { format!("{n} lines") }
}

fn claude_reply_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: GetParams = parse_params(params)?;
    let index = p.index.unwrap_or(0);
    let want = want_for(index, p.reply_id.as_deref())?;
    let reply_id = p.reply_id.clone();
    with_replies(ctx, &p.session_id, want, move |sid, replies| {
        let (i, r) = pick(replies, index, reply_id.as_deref())?;
        let lines = r.line_count();
        let which = if i == 0 { "Claude's last reply".to_owned() } else { format!("Claude's reply {i} before the last") };
        Ok(ToolOutput::new(
            format!("{which} ({})", lines_word(lines)),
            json!({"session_id": sid, "index": i, "id": r.id, "timestamp": r.timestamp, "lines": lines, "text": r.text, "code_blocks": blocks_json(&r)}),
        ))
    })
}

fn claude_code_block_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: BlockParams = parse_params(params)?;
    let index = p.reply_index.unwrap_or(0);
    let block = p.block_index.unwrap_or(0);
    let want = want_for(index, p.reply_id.as_deref())?;
    let reply_id = p.reply_id.clone();
    with_replies(ctx, &p.session_id, want, move |sid, replies| {
        let (i, r) = pick(replies, index, reply_id.as_deref())?;
        let blocks = r.code_blocks();
        let count = blocks.len();
        let b = blocks.into_iter().nth(block).ok_or_else(|| {
            ToolError::NotFound(match count {
                0 => "that reply of Claude's has no code block".to_owned(),
                n => format!("that reply has {n} code block{}; block_index is 0 to {}", if n == 1 { "" } else { "s" }, n - 1),
            })
        })?;
        let lines = b.code.lines().count();
        let lang = b.lang.clone().map(|l| format!("{l} ")).unwrap_or_default();
        Ok(ToolOutput::new(
            format!("Code block {} ({lang}{})", block + 1, lines_word(lines)),
            json!({"session_id": sid, "reply_index": i, "reply_id": r.id, "block_index": block, "lang": b.lang, "lines": lines, "code": b.code}),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: &str, text: &str) -> ClaudeReply {
        ClaudeReply { id: id.into(), timestamp: None, text: text.into() }
    }

    #[test]
    fn pick_by_index_or_id() {
        let v = vec![r("a", "new"), r("b", "old")];
        assert_eq!(pick(v.clone(), 1, None).unwrap().1.id, "b");
        assert_eq!(pick(v.clone(), 0, Some("b")).unwrap().0, 1);
        assert!(matches!(pick(v.clone(), 2, None), Err(ToolError::NotFound(_))));
        assert!(matches!(pick(v, 0, Some("zz")), Err(ToolError::NotFound(_))));
        assert!(matches!(pick(Vec::new(), 0, None), Err(ToolError::NotFound(m)) if m.contains("not replied")));
    }

    #[test]
    fn index_is_bounded() {
        assert_eq!(want_for(0, None).unwrap(), 1);
        assert_eq!(want_for(3, Some("x")).unwrap(), MAX_REPLIES);
        assert!(want_for(MAX_REPLIES, None).is_err());
    }
}

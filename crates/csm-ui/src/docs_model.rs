//! What the inspector's Docs tab shows: the project docs of a session's folder, from the
//! `project_docs_list` and `project_doc_get` tool **replies**. The backend keeps them here and puts
//! them into [`crate::model::UiModel::docs`]; the tab reads it and submits the tools (polled,
//! never waited for). A doc's Markdown is parsed once, when its reply lands, never per frame.
//! The demo fills it from a fixture.

use std::collections::BTreeMap;
use std::sync::Arc;

use csm_core::addons::screen::markdown::{Block, ParseOptions, parse_with};
use serde_json::Value;

use crate::backend::Call;
use crate::model::SessionId;

/// The tool ids whose replies land here.
pub const TOOLS: [&str; 2] = ["project_docs_list", "project_doc_get"];

/// The names looked for, in order (`csm_services::project_docs::NAMES`).
pub const LOOKED_FOR: [&str; 6] = csm_services::project_docs::NAMES;

/// The most text shown (`csm_services::project_docs::MAX_BYTES`): a bigger file is not read.
pub const MAX_BYTES: u64 = csm_services::project_docs::MAX_BYTES;

/// One doc found on disk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocEntry {
    /// `CLAUDE.md`, `.claude/CLAUDE.md`, ...
    pub name: String,
    /// Relative to the session's folder, with `/` (`README.md`, `../../CLAUDE.md`).
    pub rel: String,
    pub path: String,
    /// Found at the top of the git repository, not in the folder itself.
    pub repo_root: bool,
    pub size: u64,
    pub modified_ms: u64,
}

impl DocEntry {
    /// The picker's label: the name, or the relative path for a doc at the repository root.
    pub fn label(&self) -> &str {
        if self.repo_root { &self.rel } else { &self.name }
    }
}

/// A folder's docs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocList {
    pub folder: String,
    pub entries: Vec<DocEntry>,
    /// The doc to show first (its `rel`).
    pub default: Option<String>,
}

impl DocList {
    pub fn entry(&self, rel: &str) -> Option<&DocEntry> {
        self.entries.iter().find(|e| e.rel == rel)
    }
}

/// One doc's text, parsed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocView {
    pub rel: String,
    pub size: u64,
    pub modified_ms: u64,
    /// Over [`MAX_BYTES`]: not shown.
    pub truncated: bool,
    /// Not UTF-8: bad bytes were replaced.
    pub lossy: bool,
    pub blocks: Arc<Vec<Block>>,
}

/// Docs keyed by session (the tab follows the active session) and, for texts, by `rel`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DocsDetails {
    /// The real engine: the tab polls the tools. The demo's fixture is static.
    pub live: bool,
    pub lists: BTreeMap<SessionId, DocList>,
    /// Why a session has no docs list (an SSH session, a folder that is gone).
    pub unavailable: BTreeMap<SessionId, String>,
    pub texts: BTreeMap<(SessionId, String), Arc<DocView>>,
    /// Why a doc could not be read.
    pub text_errors: BTreeMap<(SessionId, String), String>,
}

fn entry_of(v: &Value) -> Option<DocEntry> {
    Some(DocEntry {
        name: v["name"].as_str()?.to_owned(),
        rel: v["rel"].as_str()?.to_owned(),
        path: v["path"].as_str().unwrap_or_default().to_owned(),
        repo_root: v["scope"].as_str() == Some("repo_root"),
        size: v["size"].as_u64().unwrap_or(0),
        modified_ms: v["modified_ms"].as_u64().unwrap_or(0),
    })
}

/// Parses a doc's Markdown (capped; a text the parser refuses shows as one paragraph).
pub fn parse_doc(text: &str) -> Vec<Block> {
    parse_with(text, ParseOptions::DOCS).unwrap_or_else(|e| vec![Block::Paragraph(vec![csm_core::addons::screen::markdown::Inline::Text(e.to_string())])])
}

impl DocsDetails {
    /// Follows a docs tool's reply. `None` when the call is not one, or was not about a session
    /// (a CLI or palette call with `path`). Errors are kept for the tab: a poll must not toast.
    pub fn on_reply(&mut self, call: &Call, reply: &csm_engine::Reply) -> Option<()> {
        if !TOOLS.contains(&call.id.as_str()) {
            return None;
        }
        let sid = call.params["session_id"].as_str()?.to_owned();
        if call.id == "project_docs_list" {
            match reply {
                Ok(o) => {
                    self.unavailable.remove(&sid);
                    let entries: Vec<DocEntry> = o.data["docs"].as_array().into_iter().flatten().filter_map(entry_of).collect();
                    let default = o.data["default"].as_str().map(str::to_owned);
                    let folder = o.data["folder"].as_str().unwrap_or_default().to_owned();
                    // Texts of docs that are gone are dropped.
                    self.texts.retain(|(s, rel), _| s != &sid || entries.iter().any(|e| &e.rel == rel));
                    self.lists.insert(sid, DocList { folder, entries, default });
                }
                Err(e) => {
                    self.lists.remove(&sid);
                    self.unavailable.insert(sid, e.to_string());
                }
            }
            return Some(());
        }
        let rel = call.params["file"].as_str().unwrap_or_default().to_owned();
        let key = (sid, rel.clone());
        match reply {
            Ok(o) => {
                self.text_errors.remove(&key);
                let d = &o.data;
                let truncated = d["truncated"].as_bool().unwrap_or(false);
                let blocks = if truncated { Vec::new() } else { parse_doc(d["text"].as_str().unwrap_or_default()) };
                let view = DocView {
                    rel,
                    size: d["size"].as_u64().unwrap_or(0),
                    modified_ms: d["modified_ms"].as_u64().unwrap_or(0),
                    truncated,
                    lossy: d["lossy"].as_bool().unwrap_or(false),
                    blocks: Arc::new(blocks),
                };
                self.texts.insert(key, Arc::new(view));
            }
            Err(e) => {
                self.text_errors.insert(key, e.to_string());
            }
        }
        Some(())
    }
}

/// `path` with `.` and `..` folded and `\` read as `/` (lexically; a leading `..` stays).
fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." if out.last().is_some_and(|l| *l != "..") => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// The listed doc a relative link in doc `from` (a `rel`) points at, if any. Names compare
/// ignoring ASCII case, as the docs are found.
pub fn resolve_link<'a>(list: &'a DocList, from: &str, target: &str) -> Option<&'a DocEntry> {
    let dir = from.rsplit_once('/').map_or("", |(d, _)| d);
    let want = normalize(&format!("{dir}/{target}"));
    list.entries.iter().find(|e| normalize(&e.rel).eq_ignore_ascii_case(&want))
}

/// The doc to show: the person's choice for this folder when it is still listed, else the
/// list's default, else the first.
pub fn pick<'a>(list: &'a DocList, chosen: Option<&str>) -> Option<&'a DocEntry> {
    chosen.and_then(|c| list.entry(c)).or_else(|| list.default.as_deref().and_then(|d| list.entry(d))).or_else(|| list.entries.first())
}

const DEMO_CLAUDE: &str = "# Odulodo.Web

Storefront for Odulodo (ASP.NET Core 9 + Optimizely Commerce). Read [AGENTS.md](AGENTS.md) for the shared agent rules and the [README](README.md) for setup.

## Build and test

```sh
dotnet build Odulodo.Web.sln
dotnet test tests/Odulodo.Web.Tests
npm run build --prefix src/Frontend
```

## Rules

- **Never** commit to `main`; work on a `feat/*` branch and open a PR.
- Facets live in `src/Search/Facets/`; every new facet needs a unit test.
- Brand colours come from `tokens.json`, not literals.
- Ask before touching `appsettings.Production.json`.

> The search index is rebuilt nightly. Locally, run `dotnet run --project tools/Reindex`.

See the [Optimizely docs](https://docs.developers.optimizely.com/commerce-connect) for the catalog API.
";

const DEMO_README: &str = "# Odulodo.Web

The Odulodo storefront.

## Getting started

1. Install the .NET 9 SDK and Node 22.
2. `dotnet restore`, then `npm ci --prefix src/Frontend`.
3. Copy `appsettings.Development.sample.json` to `appsettings.Development.json`.
4. `dotnet run --project src/Odulodo.Web`.

Agents: start with [CLAUDE.md](CLAUDE.md).

---

Licensed to Odulodo A/S. Questions: *#odulodo-web* on Slack.
";

const DEMO_AGENTS: &str = "# Agent rules

These hold for every coding agent in this repository (Claude Code, Codex, ...).

- Keep changes small and reviewable; one concern per PR.
- Run the tests before saying something works.
- Don't add packages without asking.
- Secrets never go in the repo; use `dotnet user-secrets`.

Project-specific notes are in [CLAUDE.md](./CLAUDE.md).
";

/// The demo's docs: Odulodo.Web (and its other sessions) has three, Odulodo a README, the rest
/// none.
pub fn demo_details() -> DocsDetails {
    let mut d = DocsDetails::default();
    let entry = |folder: &str, name: &str, text: &str, mins_ago: u64| DocEntry {
        name: name.into(),
        rel: name.into(),
        path: format!("{folder}\\{name}"),
        repo_root: false,
        size: text.len() as u64,
        modified_ms: 1_795_513_320_000 - mins_ago * 60_000,
    };
    let docs = [("README.md", DEMO_README, 60 * 24 * 9), ("CLAUDE.md", DEMO_CLAUDE, 42), ("AGENTS.md", DEMO_AGENTS, 60 * 26)];
    for (sid, folder) in [("web", "C:\\GitHub\\Odulodo.Web"), ("webps", "C:\\GitHub\\Odulodo.Web")] {
        let entries: Vec<DocEntry> = docs.iter().map(|(n, t, m)| entry(folder, n, t, *m)).collect();
        for ((name, text, _), e) in docs.iter().zip(&entries) {
            let view = DocView { rel: (*name).into(), size: e.size, modified_ms: e.modified_ms, truncated: false, lossy: false, blocks: Arc::new(parse_doc(text)) };
            d.texts.insert((sid.into(), (*name).into()), Arc::new(view));
        }
        d.lists.insert(sid.into(), DocList { folder: folder.into(), entries, default: Some("CLAUDE.md".into()) });
    }
    let folder = "C:\\GitHub\\Odulodo";
    let readme = "# Odulodo\n\nThe Odulodo back office: catalog import, pricing and the order pipeline.\n\n- `dotnet test tests/Odulodo.Tests`\n- Docs: <https://wiki.odulodo.example/backoffice>\n";
    let e = entry(folder, "README.md", readme, 60 * 3);
    let view = DocView { rel: "README.md".into(), size: e.size, modified_ms: e.modified_ms, truncated: false, lossy: false, blocks: Arc::new(parse_doc(readme)) };
    d.texts.insert(("odulodo".into(), "README.md".into()), Arc::new(view));
    d.lists.insert("odulodo".into(), DocList { folder: folder.into(), entries: vec![e], default: Some("README.md".into()) });
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_engine::{ToolError, ToolOutput};
    use serde_json::json;

    fn call(id: &str, params: Value) -> Call {
        Call { id: id.into(), params }
    }

    fn list() -> DocList {
        let e = |name: &str, rel: &str, root: bool| DocEntry { name: name.into(), rel: rel.into(), repo_root: root, ..DocEntry::default() };
        DocList {
            folder: "C:\\r\\sub".into(),
            entries: vec![e("README.md", "README.md", false), e(".claude/CLAUDE.md", ".claude/CLAUDE.md", false), e("CLAUDE.md", "../CLAUDE.md", true)],
            default: Some("../CLAUDE.md".into()),
        }
    }

    #[test]
    fn links_resolve_against_the_doc_they_are_in() {
        let l = list();
        assert_eq!(resolve_link(&l, "README.md", "../CLAUDE.md").map(|e| e.rel.as_str()), Some("../CLAUDE.md"));
        assert_eq!(resolve_link(&l, "README.md", "./readme.MD").map(|e| e.rel.as_str()), Some("README.md"), "case-insensitive");
        assert_eq!(resolve_link(&l, ".claude/CLAUDE.md", "../README.md").map(|e| e.rel.as_str()), Some("README.md"));
        assert_eq!(resolve_link(&l, "../CLAUDE.md", "sub/README.md").map(|e| e.rel.as_str()), None, "lexical: the folder's name is not known");
        assert_eq!(resolve_link(&l, "../CLAUDE.md", "CLAUDE.md").map(|e| e.rel.as_str()), Some("../CLAUDE.md"), "itself");
        assert!(resolve_link(&l, "README.md", "docs/plan.md").is_none(), "not listed: plain text");
        assert!(resolve_link(&l, "README.md", "..\\..\\..\\x.md").is_none());
    }

    #[test]
    fn the_pick_follows_the_choice_then_the_default() {
        let l = list();
        assert_eq!(pick(&l, None).map(|e| e.rel.as_str()), Some("../CLAUDE.md"));
        assert_eq!(pick(&l, Some("README.md")).map(|e| e.rel.as_str()), Some("README.md"));
        assert_eq!(pick(&l, Some("gone.md")).map(|e| e.rel.as_str()), Some("../CLAUDE.md"));
        assert_eq!(pick(&DocList::default(), Some("x")), None);
        assert_eq!(l.entries[2].label(), "../CLAUDE.md", "a root doc shows where it is");
        assert_eq!(l.entries[1].label(), ".claude/CLAUDE.md");
    }

    #[test]
    fn replies_fill_the_details_and_errors_are_kept() {
        let mut d = DocsDetails::default();
        assert_eq!(d.on_reply(&call("note_get", json!({"session_id": "s"})), &Ok(ToolOutput::message(""))), None);
        assert_eq!(d.on_reply(&call("project_docs_list", json!({"path": "C:\\x"})), &Ok(ToolOutput::message(""))), None, "not about a session");
        let out = ToolOutput::new(
            "",
            json!({"folder": "C:\\p", "default": "CLAUDE.md", "docs": [
                {"name": "README.md", "rel": "README.md", "path": "C:\\p\\README.md", "scope": "folder", "size": 10, "modified_ms": 5},
                {"name": "CLAUDE.md", "rel": "CLAUDE.md", "path": "C:\\p\\CLAUDE.md", "scope": "repo_root", "size": 3, "modified_ms": 6},
                {"bad": true}
            ]}),
        );
        d.on_reply(&call("project_docs_list", json!({"session_id": "s"})), &Ok(out));
        let l = &d.lists["s"];
        assert_eq!(l.entries.len(), 2);
        assert!(l.entries[1].repo_root);
        assert_eq!(l.default.as_deref(), Some("CLAUDE.md"));

        let text = ToolOutput::new("", json!({"rel": "CLAUDE.md", "size": 3, "modified_ms": 6, "text": "# Hi\n\n[r](README.md)", "truncated": false, "lossy": true}));
        d.on_reply(&call("project_doc_get", json!({"session_id": "s", "file": "CLAUDE.md"})), &Ok(text));
        let v = &d.texts[&("s".to_owned(), "CLAUDE.md".to_owned())];
        assert!(v.lossy && !v.truncated);
        assert_eq!(v.blocks.len(), 2);
        d.on_reply(&call("project_doc_get", json!({"session_id": "s", "file": "README.md"})), &Err(ToolError::Failed("denied".into())));
        assert_eq!(d.text_errors[&("s".to_owned(), "README.md".to_owned())], "denied");

        // A doc that is gone takes its text with it.
        d.on_reply(&call("project_docs_list", json!({"session_id": "s"})), &Ok(ToolOutput::new("", json!({"docs": []}))));
        assert!(d.texts.is_empty());
        d.on_reply(&call("project_docs_list", json!({"session_id": "s"})), &Err(ToolError::Refused("SSH".into())));
        assert_eq!(d.unavailable["s"], "SSH");
        assert!(!d.lists.contains_key("s"));
    }

    #[test]
    fn hostile_replies_do_not_panic() {
        let mut d = DocsDetails::default();
        for data in [json!(null), json!([]), json!({"docs": "x", "text": 5}), json!({"docs": [null, 1, {"name": 3}]})] {
            for id in TOOLS {
                d.on_reply(&call(id, json!({"session_id": "s", "file": "a"})), &Ok(ToolOutput::new("", data.clone())));
            }
        }
    }

    #[test]
    fn the_demo_has_docs_that_link_to_each_other() {
        let d = demo_details();
        assert!(!d.live);
        let l = &d.lists["web"];
        assert_eq!(pick(l, None).map(|e| e.name.as_str()), Some("CLAUDE.md"));
        assert!(resolve_link(l, "README.md", "CLAUDE.md").is_some());
        assert!(d.texts.contains_key(&("web".to_owned(), "AGENTS.md".to_owned())));
    }
}

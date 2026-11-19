# Tools: every action, one registry, three front ends

CodeShellManager follows the Craft apps' rule: **everything is a tool.** Every action the person
can take (open a session, focus one, sleep a group, switch the layout, answer the inbox) is a
`ToolSpec` registered with the engine (`crates/csm-engine`). Four callers dispatch by tool id:

| Caller | How | Consent |
|---|---|---|
| The app (buttons, menus, palette, keymap) | `engine.submit(Caller::Ui, id, params)`; polled once a frame | Never asked by the engine; the UI asks its own questions ("Close all…?") |
| The CLI | `csm <tool-id-in-kebab-case> --json '{…}'` (or flags), talks to the running app | As the person: no consent card |
| MCP clients | `csm mcp` (stdio), bridged to the running app | Tools whose effects need consent wait for an "Allow once / Always for this client / Don't allow" card in the app |
| The engine itself | scheduled work, restore loop, bulk loops | n/a |

So there is no "MCP hook" to add separately: a feature that isn't a tool isn't finished. A button
that does something no tool can do is a review failure.

## The contract (`crates/csm-engine/src/tool.rs`)

- `id`: `snake_case`, `<area>_<verb>[_<object>]`. It's also the MCP tool name; the CLI uses
  kebab-case. Ids are stable once released. Rename means add the new one and keep the old as an
  alias.
- `title` (palette and menus), `summary` (one or two sentences, for agents and the palette's
  detail line), `toolset`, `effects`, `input_schema` (a `schemars` params struct with
  `deny_unknown_fields`; doc comments on fields become the schema's descriptions), `run`.
- **Effects are honest.** They drive consent, the MCP annotations and confirmation prompts:
  `IDEMPOTENT`, `CHANGES_UI_STATE`, `MUTATES_STATE`, `LAUNCHES_PROCESS`, `SENDS_INPUT`,
  `MAY_DISCARD_DATA`, `WRITES_FILESYSTEM`, `ACCESSES_NETWORK`, `READS_SESSION_CONTENT`.
- **A tool never blocks the engine thread.** Spawn the slow part on a worker and finish with
  `ctx.engine.post(job)`; report progress with events. A tool that needs to wait (e.g.
  `session_wait`) parks its reply and completes it from a later event.
- **Results** are `ToolOutput { message, data }`: one plain sentence, plus JSON. Errors are
  `ToolError` with a message that says what to do.
- **Programmatic calls never open dialogs.** A tool that would ask the person in the UI
  (close a busy session) takes a parameter instead (`force: bool`, default false) and refuses
  with a clear error without it.
- **Untrusted text into a PTY is pasted, never typed.** `session_send_text` uses bracketed paste;
  `submit: true` adds the Enter separately. See the WPF repo's `SendToTerminal` vs
  `PasteToTerminal`.

## Consent

`Effects::needs_consent()` (any of `SENDS_INPUT`, `MAY_DISCARD_DATA`, `LAUNCHES_PROCESS`,
`WRITES_FILESYSTEM`, `READS_SESSION_CONTENT`) makes an `External` call park until the person
answers a card in the app. The card names the client, the session it runs in (when it is one of
ours), the tool and its params. "Always for this client" grants that client that tool; grants are
listed and revocable in Settings → MCP. `consent_answer` is refused for every caller but the UI,
so a client can never approve itself. Unanswered requests time out after 120 s.

Two extra rules the engine enforces regardless of grants:
- A session can't send input to **itself** through MCP (an agent typing into its own prompt).
- `session_send_text` into a session that is mid-turn (`working`) is refused unless `force: true`.

## Toolsets (progressive disclosure)

MCP clients see `core` by default and turn others on with `toolset_enable` (as Craft Anything
does; dozens of tool definitions cost context). The app and CLI see everything.

## The catalogue

Status: ✅ built · 🚧 in progress · ⬜ planned. Params in brief; the schema is the reference.

### core: sessions and the inbox
| id | params | effects | what |
|---|---|---|---|
| `tool_list` ✅ | toolset? | read | every tool |
| `session_list` ✅ | group?, status?, include_asleep? | read | id, name, kind, folder, group, status, branch, live/asleep |
| `session_get` ✅ | session_id | read | one session in full, incl. program-status records |
| `session_new` ✅ | folder, command?, name?, group?, kind (local/ssh/wsl)?, ssh{user,host,port,remote_folder}?, wsl{distro,user,folder}?, focus? | launch, state | open a session (the quick-open and the "New session" panel call this) |
| `session_focus` ✅ | session_id | ui | focus it (switches the rail tab; wakes if asleep → `session_wake`) |
| `session_rename` ✅ | session_id, name | state | |
| `session_edit` ✅ | session_id, draft fields… | state (+launch if restart) | apply a `SessionConfigDraft`; `restart: bool` |
| `session_close` ✅ | session_id, force? | discard, state | refuses a busy session without `force`; pushes to recently closed |
| `session_sleep` / `session_wake` ✅ | session_id | discard / launch | |
| `session_restart` ✅ | session_id | discard, launch | waits for exit; guarded against restore/bulk restart |
| `session_reopen_closed` ✅ | index? | launch | Ctrl+Shift+T |
| `session_send_text` ✅ | session_id, text, submit? , force? | input | bracketed paste; refuses self and mid-turn |
| `session_read_screen` ✅ | session_id | read content | the visible screen as text |
| `session_read_output` ✅ | session_id, lines? (≤2000) | read content | tail of scrollback, ANSI-stripped |
| `session_wait` ✅ | session_id, until (idle/done/needs_you/exit/any_change), timeout_s (≤600) | read | completes when the status reaches it; for orchestration |
| `inbox_list` ✅ | | read | sessions that need you, oldest first, plus unseen done/error |
| `inbox_next` ✅ | | ui | focus the next one (Ctrl+J) |
| `session_status_clear` ✅ | session_id | state | "Clear status" |
| `toolset_enable` / `toolset_disable` ✅ | toolset | (MCP only) | |

### groups
All ✅, plus `group_move` (index) and `group_collapse` (collapsed). `group_list`, `group_new` (name), `group_rename`, `group_delete` (sessions → Ungrouped),
`session_move_to_group` (session_id, group_id|null, before?/after? anchor), `group_restart_all`
(confirm via `force`), `group_sleep_all`, `group_close_all` (force), `group_tear_off` / `group_dock`.

### layout
All ✅. `pane_float` / `pane_dock` / `pane_tear_off` are registered by the app (`csm-ui`'s
`pane_tools`), because the Free layout they edit is stored by the engine as an opaque object
whose shape is the UI's (`layout_set { free }`). `view_get`, `view_set` (group view: rail/headers/flat; rail tab), `layout_get` (group),
`layout_set` (group, mode focus/side/overview/grid/free, grid preset or cols×rows, `free`: the Free layout as an opaque object ≤ 64 KiB),
`layout_pin` / `layout_unpin` (session), `pane_float` / `pane_dock` / `pane_tear_off`,
`inspector_show` (tab notes/runs/git/session | hidden), `inbox_panel_set` (open, height).

### Not built yet (runs, notes, search, `open_folder`, `open_with`, `grant_list` / `grant_revoke`)

The app registers these ids with their real params and effects, as **stand-ins that validate the
params and then refuse with "This isn't built yet"** (`csm-ui`'s `demo::tools::stand_ins`), so
the UI, the palette, the CLI and MCP clients see the whole catalogue and get an honest answer.
Implementing one in the engine means deleting its stand-in (the registry refuses duplicate ids,
which `stand_ins_do_not_collide_with_engine_tools` checks). `codeshell --demo` registers echoing
versions instead, applied to the demo's own model.

### runs
`run_list` (session_id), `run_start` (session_id, run_id? = default), `run_stop`, `run_output`
(read content), `run_edit` (the list).

### git
All ✅ (`crates/csm-engine/src/tools/git`, git run by `csm-services::git::run` on worker threads, argv only).
Read-mostly on purpose: no commit, push or merge (Claude is editing that tree).

| id | params | effects | what |
|---|---|---|---|
| `git_status` ✅ | session_id | read, launch | branch, upstream, ahead/behind, changed files, `worktree_of`, the header line |
| `git_fetch` ✅ | session_id | launch, network | `git fetch`; the reply says what arrived (`fetch_summary`) |
| `git_pull` ✅ | session_id | launch, network, writes files | `pull --ff-only --no-rebase`; a diverged branch is refused with git's reason |
| `git_switch_branch` ✅ | session_id, name | launch, writes files | `switch --no-guess -- <name>`; a branch checked out in another worktree is refused, naming it |
| `git_branches` ✅ | session_id | read, launch | local branches; for each one held elsewhere, that worktree, its session and `session_new` params to open it |
| `worktree_list` ✅ | session_id | read, launch | every worktree: label, branch, dirty, ↑/↓, prunable, the sessions in it, `open` (`session_new` params) |
| `worktree_new` ✅ | session_id, branch, base?, path?, open_session? (true) | launch, writes files, state | `worktree add` (`-b` when new) beside the main checkout; opens a session there like this one, placed after it |
| `worktree_remove` ✅ | session_id, path?, delete_branch?, force? | launch, writes files, discard, state | no `--force`; refuses the main checkout, changes, a worktree listed inside it, any repository on disk inside it (fails closed); closes the folder's sessions (only with `force`) and waits for their exit; sleeping ones are deleted after git removed the folder; `delete_branch` uses `-d` |
| `worktree_prune` ✅ | session_id | launch, writes files | `worktree prune`: forgets worktrees whose folder is gone |

One operation per repository at a time (keyed by the canonical repo root, so worktrees of one repo
share it; a second is refused). After any operation every live session on that repo re-reads its
state. `SessionView` carries `branch`, `git_dirty`, `git_upstream`, `git_ahead`, `git_behind`,
`git_changes`, `git_summary`, `worktree_of`, `repo_root`, `git_busy`, filled by a poll (focused 10 s,
others 120 s, WSL 30 s) plus a shared `.git/HEAD` + `index` watcher; an OSC 9001 push wins.

### notes, search
`note_get` / `note_set` (folder or session_id), `search_output` (query, limit), `search_notes`.

### app
✅ `app_info`, `settings_get/set`, `recently_closed_list`, `state_import_wpf`, `app_quit`; ⬜ the rest.
`app_info` (version, ConPTY in use, paths), `settings_get` / `settings_set` (key, value),
`recently_closed_list`, `open_folder` (session_id), `open_with` (session_id, entry?),
`consent_list` ✅, `consent_answer` ✅ (UI only), `grant_list` / `grant_revoke`,
`state_import_wpf` (path to the old `state.json`), `app_quit`.

## Exposing CSM to Claude sessions

Opt-in (Settings → MCP → "Offer CodeShellManager's tools to Claude sessions", default off): a
Claude session launched by CSM gets `--mcp-config` pointing at `csm mcp --session <id>`, so its
calls carry `Caller::External { session_id }` and the consent card can say "Claude in Odulodo
wants to…". Off by default because it changes the user's `claude` command line. Any MCP client
can be pointed at `csm mcp` by hand.

## The bridge (`crates/csm-mcp`, `apps/csm-cli`)

The app calls `csm_mcp::serve_ipc(engine)` once at start-up; `csm` and `csm mcp` find the instance
record (craft-ipc discovery, app id `codeshell`), authenticate with its token and forward calls.
A call is `csm.submit` + repeated `csm.wait` long polls, so a call may wait out the 120 s consent
timeout (craft-ipc replies within 60 s otherwise). Remote callers are `Cli` or `External`, never
`Ui`. **Windows uses craft-ipc's loopback TCP transport**: its named pipe stalls any line over
about 512 bytes (measured on Windows 11), see `USE_TCP_BY_DEFAULT`. Environment: `CSM_APP` (binary
for `--start`), `CSM_RUNTIME_DIR` (instance directory, for tests).

Register with Claude Code: `claude mcp add codeshell -- csm mcp` (add `--session <id>` when CSM
launches the Claude session itself).

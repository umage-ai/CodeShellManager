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
| `session_accent_reset` ✅ | session_id | state | "Reset accent colour": forget an OSC 9001 colour |
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

### Not built yet (runs, git, notes, search, `open_folder`, `open_with`)

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
`git_status` (session_id), `git_fetch`, `git_pull` (ff-only), `git_switch_branch` (name),
`git_branches`, `worktree_list`, `worktree_new` (branch, open_session?), `worktree_remove`
(refuses dirty/nested; `delete_branch?` uses `-d`). All argv-built via `csm-services::git`.

### notes, search
`note_get` / `note_set` (folder or session_id), `search_output` (query, limit), `search_notes`.

### app
✅ `app_info`, `settings_get/set`, `recently_closed_list`, `state_import_wpf`, `app_quit` (force?), `grant_list` / `grant_revoke`, `wsl_distros` (read; the new-session panel's distro list, probed off the engine thread); ⬜ the rest.
`app_info` (version, ConPTY in use, paths), `settings_get` / `settings_set` (key, value),
`recently_closed_list`, `open_folder` (session_id), `open_with` (session_id, entry?),
`consent_list` ✅, `consent_answer` ✅ (UI only), `grant_list` / `grant_revoke`,
`state_import_wpf` (path to the old `state.json`), `app_quit` (`force: true` skips the remaining exit
waits, also while a quit already runs: the shutdown board's "Force close now"). Grants are kept in
`state.json` (`McpGrants`); `grant_revoke` takes `client` and an optional `tool` (none = all).

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

## MCP context cost (measured)

What `csm mcp` costs a client in context, from `cargo xtask mcp-size` (the real `McpService` over
an in-process engine; tokens are bytes / 4, an estimate good enough to compare toolsets).
Measured on 2026-11-19 with the engine's tools only: the git, runs, notes and search tools were
still being written, so those rows are 0 and will grow. Re-run it after adding tools, and keep
`core` small: it is what every client pays on every conversation.

| toolset | tools | bytes | est. tokens |
|---|---|---|---|
| base: `core` + the 3 toolset tools | 22 | 15160 | ~3790 |
| + groups | 12 | 5585 | ~1396 |
| + layout | 8 | 5007 | ~1251 |
| + app | 8 | 3363 | ~840 |
| everything enabled | 50 | 29115 | ~7278 |

Progressive disclosure is worth about 3.5x here: a client that only needs sessions pays ~3.8k
tokens instead of ~7.3k, and the gap grows as the other toolsets fill in.

## Repository checks (`cargo xtask`)

`cargo xtask ci` runs everything CI runs. Individually: `layers` (crate dependencies against the
L0-L6 map), `policy` (`syn` rules: `std::process::Command` only in `csm-engine/src/proc.rs`,
`csm-pty`, `csm-setup`; no `std::fs`/`std::process`/blocking waits in `csm-ui` frame code; `unsafe`
only in csm-pty's ConPTY module; crate-root attributes), `tools --check` (the generated block
below must match the registry: run `cargo xtask tools` after adding a tool), `secrets`
(`--staged` for the pre-commit hook, enabled with `git config core.hooksPath .githooks`),
`mcp-size [--markdown]`.

## Generated catalogue

<!-- xtask:tools:begin (generated by `cargo xtask tools`; do not edit by hand) -->

### Engine registry (50 tools)

Generated from `csm-engine`'s registry. "asks" means an MCP caller waits for the person's consent.

| id | toolset | effects | MCP consent | what |
|---|---|---|---|---|
| `tool_list` | core | read | no | Every tool: id, title, summary, toolset and effects. Filter by toolset. |
| `consent_list` | app | read | no | Requests from MCP clients waiting for the person to allow or deny them. |
| `consent_answer` | app | state | no | Allow once, allow always for this client, or deny a waiting request. App only. |
| `session_list` | core | read | no | Every session in sidebar order: id, name, kind, folder, group, status (working, needs_you_approval, needs_you_question, done_unseen, error, idle, asleep, starting, exited), branch. |
| `session_get` | core | read | no | One session in full: its launch settings and its status, including the program-status message. |
| `session_new` | core | state, launch | asks | Open a session: a folder and a command (default: the configured one, usually claude), locally, over SSH or in WSL. It starts when its pane is measured, or after 1.5 s headless. |
| `session_focus` | core | ui, launch | asks | Bring a session to the front (switches the rail tab to its group, wakes it when asleep) and mark its results seen. |
| `session_rename` | core | state | no | Give a session a new name. |
| `session_edit` | core | state, launch, discard | asks | Change a session's folder, command, arguments, kind or connection. Changes that need a new process apply at the next start, or now with restart: true. |
| `session_close` | core | state, discard | asks | End a session's process and remove it (it goes to Recently closed). Refuses a busy session unless force is true. |
| `session_sleep` | core | state, discard | asks | End a session's process but keep it in the sidebar, with its settings and place, to wake later. |
| `session_wake` | core | state, launch | asks | Start a sleeping session again (a Claude session resumes its conversation). |
| `session_restart` | core | launch, discard | asks | Close and reopen a session in place (to pick up a new CLI build). Waits for Claude to exit first; refused while sessions are being restored or another restart runs. Replies when it has started again. |
| `session_reopen_closed` | core | state, launch | asks | Reopen a recently closed session (index 0 = the last one closed). |
| `session_send_text` | core | input | asks | Paste text into a session (bracketed paste, never typed); submit: true presses Enter after it. Refused into the caller's own session, and while the session is working unless force is true. |
| `session_read_screen` | core | content | asks | The session's visible screen as plain text. |
| `session_read_output` | core | content | asks | The last lines (default 200, at most 2000) of a session's scrollback and screen, as plain text. |
| `session_wait` | core | read | no | Wait until a session is idle, done, needs you, has exited, or changes status at all (until), up to timeout_s (default 60, max 600). |
| `session_status_clear` | core | ui | no | Forget a session's reported status (for a status stuck after a program died mid-turn); the screen heuristic takes over again. |
| `inbox_list` | core | read | no | Sessions waiting for you (oldest first), then finished or failed turns you have not seen. |
| `inbox_next` | core | ui | no | Focus the next session in the inbox. |
| `group_list` | groups | read | no | Every group in order: id, name, session count, how many need you. |
| `group_new` | groups | state | no | Create a group. |
| `group_rename` | groups | state | no | Rename a group. |
| `group_delete` | groups | state | no | Delete a group; its sessions move to Ungrouped (nothing is closed). |
| `group_move` | groups | state | no | Move a group to a position in the group order (0 = first). |
| `group_collapse` | groups | ui | no | Collapse or expand a group's section in the headers view. |
| `session_move_to_group` | groups | state | no | Put a session in a group (group_id null = Ungrouped), optionally right before or after another session of that group. |
| `group_restart_all` | groups | launch, discard | asks | Restart every live session of a group, one after another. Needs force: true when it is more than one session (it takes a while). |
| `group_sleep_all` | groups | state, discard | asks | Put every live session of a group to sleep. |
| `group_close_all` | groups | state, discard | asks | Close every session of a group (live and asleep). Refuses when any is busy unless force is true. |
| `group_tear_off` | groups | ui | no | Show a group in its own OS window. |
| `group_dock` | groups | ui | no | Bring a torn-off group back into the main window. |
| `view_get` | layout | read | no | The group view (rail, headers, flat), the rail tab, the focused session, the inspector and the inbox panel. |
| `view_set` | layout | ui | no | Switch the group view (rail, headers, flat) and/or the rail tab (a group id, or "all"). |
| `layout_get` | layout | read | no | A group's layout (default: the one shown now): mode, grid size, pinned sessions. |
| `layout_set` | layout | ui | no | Set a group's layout: mode focus, side (pinned side by side), overview, grid (preset 1, 2, 2x2, 3x2, 3x3, or cols x rows) or free. |
| `layout_pin` | layout | ui | no | Pin a session into the side-by-side layout. |
| `layout_unpin` | layout | ui | no | Remove a session from the side-by-side layout. |
| `inspector_show` | layout | ui | no | Show the inspector on a tab (notes, runs, git, session) or hide it. |
| `inbox_panel_set` | layout | ui | no | Open or close the Needs-you panel and/or set its height. |
| `app_info` | app | read | no | Version, the ConPTY in use, where the state is kept, session counts. |
| `settings_get` | app | read | no | All settings (or one, by key). Keys as in state.json (PascalCase) or snake_case. Secrets are not returned. |
| `settings_set` | app | state | no | Set one setting by key. Settings that decide what runs (default command, launch commands, editors, API key, ConPTY) can only be changed in the app. |
| `recently_closed_list` | app | read | no | Closed sessions that can be reopened, newest first (index 0). |
| `state_import_wpf` | app | state | no | Import sessions and groups from the WPF app's state.json (default: its usual location). Imported sessions arrive asleep; existing ones are kept. |
| `app_quit` | app | discard | asks | Close every session (Claude ones get time to exit cleanly), save, then signal the app to exit. Sessions restore next time. |
| `toolset_enable` | core | read | no | Offer a toolset's tools to this MCP client (core is always on). |
| `toolset_disable` | core | read | no | Stop offering a toolset's tools to this MCP client. |
| `wsl_distros` | app | read | no | The installed WSL distributions (name, version, state, default), default first. Empty when WSL is not installed or this is not Windows. |

<!-- xtask:tools:end -->

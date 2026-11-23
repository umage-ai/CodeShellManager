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

**Not a tool: `csm attach`.** Attaching a console to a session of a paired office
(`csm attach --list`, `csm attach <session>`) is a terminal stream, not a request and a reply, so it
is a command of its own rather than a tool. It never pairs, and it runs no tool of its own: the
session's keys and screen travel over the peer link (see [remote-access.md](remote-access.md)). The
pairing tools (`peer_*`) are listed below; the ones that start a pairing are refused for the CLI,
MCP clients and paired devices.

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
  `submit: true` adds the Enter separately. See the WPF app's `SendToTerminal` vs
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
| `session_list` ✅ | group?, status?, include_asleep? | read | id, name, kind, folder, group, status, branch, live/asleep, remote_control (+ remote_control_url) |
| `session_get` ✅ | session_id | read | one session in full, incl. program-status records and Remote Control (on, claude.ai link) |
| `session_new` ✅ | folder, command?, name?, group?, kind (local/ssh/wsl)?, ssh{user,host,port,remote_folder}?, wsl{distro,user,folder}?, focus?, look? | launch, state | open a session (the quick-open and the "New session" panel call this) |
| `session_focus` ✅ | session_id | ui | focus it (switches the rail tab; wakes if asleep → `session_wake`) |
| `session_rename` ✅ | session_id, name | state | |
| `session_edit` ✅ | session_id, draft fields… | state (+launch if restart) | apply a `SessionConfigDraft`; `restart: bool` |
| `session_close` ✅ | session_id, force? | discard, state | refuses a busy session without `force`; pushes to recently closed |
| `session_sleep` / `session_wake` ✅ | session_id | discard / launch | |
| `session_restart` ✅ | session_id | discard, launch | waits for exit; guarded against restore/bulk restart |
| `session_reopen_closed` ✅ | index? | launch | Ctrl+Shift+T |
| `session_duplicate` ✅ | session_id, focus? | state, launch | Ctrl+Alt+T, row and pane menus: same folder, command, args, kind, connection, group, profile and run commands (fresh ids); named `X (2)`, placed right after it; a Claude copy starts a new conversation |
| `session_send_text` ✅ | session_id, text, submit? , force? | input | bracketed paste; refuses self and mid-turn |
| `session_send_command` ✅ | session_id, command, force? | input | types a configured Claude slash command (`ClaudeCommands`) and Enter; refuses self and mid-turn |
| `session_read_screen` ✅ | session_id | read content | the visible screen as text |
| `session_read_output` ✅ | session_id, lines? (≤2000) | read content | tail of scrollback, ANSI-stripped |
| `claude_reply_list` ✅ | session_id, limit? (≤50) | read content | a local Claude session's last replies from its transcript, newest first: id, preview, lines, code blocks (lang, preview) |
| `claude_reply_get` ✅ | session_id, index? (0 = latest), reply_id? | read content | one reply exactly as Claude wrote it (Markdown, the turn's assistant text, no tool calls); the pane menu's "Copy last reply", Ctrl+Shift+C |
| `claude_code_block_get` ✅ | session_id, reply_index?, reply_id?, block_index? | read content | one fenced code block of a reply, without the fences; WSL and SSH sessions are "not available" |
| `session_output_stats` ✅ | session_id | read | the output stream's rate: bytes per second over the last minute, the busiest second, total bytes, bytes dropped while the output tap was busy, and the 1 MiB output ring's size, offsets, evictions and resyncs. Numbers only; no text. An asleep session reports `live: false` |
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

The sidebar's multi-select (Ctrl-click toggles, Shift-click selects the visible range from the
anchor, Esc or a plain click clears; `csm_core::sidebar_selection`) is widget state; what it does
goes through tools that take `session_ids` and check every id before acting:
`session_sleep_many`, `session_wake_many`, `session_restart_many` (force for more than one: the
menu's "Restart (N)…" asks first), `session_close_many` (refuses busy ones without force; the
menu's "Close (N)…" asks first) and `session_move_many` (group_id, before?/after? anchor; the
block keeps its order; a dragged selection drops through it). The row menu shows them, with
counts, when the right-clicked row is part of the selection (C# `ResolveActionTargets`).
`session_sort` (by name / folder / last_active / branch / dirty / repo, descending?, group?)
rewrites the order once (C# `ApplySort`; `csm_core::session_sort`): stable, sessions without a
branch or repo last in both directions, a group's sessions sorted within their own slots. The ⇅
control next to the Sessions header and the palette open its menu.

`session_send_text_many` (group_id | session_ids, text, submit?, force?; #59) pastes the same
text into each live session through `session_send_text`'s own path (bracketed paste, Enter as its
own write) and lists the ones it skipped: asleep, starting, exited, mid-turn without `force`, and
the caller's own session. Effects `input`, so an MCP caller asks. A group's menu has "Send to all
sessions…": a one-line prompt, then a confirm with the count.

### layout
All ✅. `pane_float` / `pane_dock` / `pane_tear_off` and `pane_minimize` / `pane_maximize` / `pane_restore` are registered by the app (`csm-ui`'s
`pane_tools`), because the Free layout they edit is stored by the engine as an opaque object
whose shape is the UI's (`layout_set { free }`). `view_get`, `view_set` (group view: rail/headers/flat; rail tab), `layout_get` (group),
`layout_set` (group, mode focus/side/overview/grid/free/board, grid preset or cols×rows, `free`: the Free layout as an opaque object ≤ 64 KiB),
`layout_pin` / `layout_unpin` (session), `pane_float` / `pane_dock` / `pane_tear_off`,
`pane_minimize` / `pane_maximize` / `pane_restore` (session, group; the pane header's window bar,
effects `ui`: minimized and maximized are stored on the engine's layout, `GroupLayout::minimized` /
`maximized`, and `session_focus` brings a minimized pane back and lets a different maximized one
give way),
`inspector_show` (tab notes/docs/runs/git/session/usage | hidden), `inbox_panel_set` (open, height).

### Stand-ins

Every id in this catalogue is now built in the engine or registered by the app (`pane_*`), so the
real app registers no stand-ins. `codeshell --demo` keeps echoing versions only where the demo's
own model needs them.

### runs ✅
Built in `csm-engine` (`runs/`, `tools/runs.rs`).

| id | params | effects | what |
|---|---|---|---|
| `run_list` ✅ | session_id | read | the session's run commands with each one's last run: phase (idle/starting/running/ok/failed/stopped), exit code, times |
| `run_start` ✅ | session_id, run_id? (id or label; default: the default) | launch | start it in its own headless PTY (restarts it if running); refused while asleep or closing |
| `run_stop` ✅ | session_id, run_id? | discard | kill the run's whole tree (Job Object / process group) |
| `run_output` ✅ | session_id, run_id?, max_chars? | read content | ANSI-stripped output, kept up to 1 MB per run |
| `run_edit` ✅ | session_id, runs [{id?, label, command_line, is_default?, mode? process/powershell, post_run_url?}] \| detect: true \| import: true | state, launch, discard | replace the list (≤ 50; one default, the last marked; one-line command lines; URL http/https only), or `detect` to take the folder's project template, or `import` to add the template's commands whose command line isn't in the list yet (keeps the list, its edits and its default; the reply says how many were added and left out). **Asks MCP clients**: it decides what F5 runs |
| `run_set` ✅ | session_id, id? (id or label of the one to update), label?, command_line?, is_default?, mode?, post_run_url? ("" removes it) | state, launch | add one run command, or update the one matching `id` (else `label`); the rest of the list stays. Same checks as `run_edit`. **Asks MCP clients** |
| `run_remove` ✅ | session_id, run_id (id or label) | state, launch, discard | remove one and stop its run; a removed default passes to the first remaining one. **Asks MCP clients** |

Rules carried from the WPF app: a run never types into its session; runs use the **inbox**
ConPTY (`allow_bundled = false`: a headless PTY has nobody to answer OpenConsole's DA1); Process
mode is `cmd.exe /c "<command>"` passed to CreateProcess verbatim (`SpawnOptions::verbatim_args`:
cmd does not parse MSVCRT quoting), PowerShell mode `-EncodedCommand`, SSH and WSL parents go
through `build_run_ssh_args` / `build_run_wsl_args` (`-e`, never `--`). Runs die with their
session (close, sleep, restart) and with the app. `PostRunUrl` opens once per run, on whichever
comes first: a loopback URL (`localhost`, `127.x`, `[::1]`, `0.0.0.0`) starts accepting TCP
connections while the run is alive (probed on a worker every 500 ms for up to 2 minutes, every
resolved address at once; the probe stops when the run ends, stops or restarts), or the run exits
0. Remote URLs and SSH parents open after exit 0 only. Only an absolute `http(s)` URL is opened
(Windows: `rundll32 url.dll,FileProtocolHandler`, argv). New local and WSL sessions are seeded
from a top-level folder scan on a worker: dotnet → cargo → node (pnpm / yarn / bun / npm) →
python → make. A Node project gets one command per `package.json` script (file order; read up to
1 MiB; lifecycle hooks and `pre`/`post` hooks skipped; only names of `[A-Za-z0-9:_.\-/@]`, up to
64 characters and not starting with `-`; at most 50, keeping the default: `dev`, else `start`,
else the first), else the fixed Start/Test/Build list.

The editing tools (`run_edit`, `run_set`, `run_remove`) declare `launch`: what they set is what
F5 later starts, so an MCP client waits for a consent card, which shows the params (the command
lines). Add-ons can never be granted them standing (`never_for_addons`).

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
| `git_diff` ✅ | session_id \| folder, source? (working), base?, head?, paths?, context?, patch?, max_bytes? | read, launch | files (status, `old_path` for a rename), the unified patch, `untracked`, `head`; `working` is the tree against HEAD, `range` is `base...head`. `--no-ext-diff --no-textconv`, paths after `--`, `--literal-pathspecs`; the patch is cut at `max_bytes` (default 512 KiB, at most 4 MiB) and says so |
| `git_show_file` ✅ | session_id \| folder, rev?, path, max_bytes? | read, launch | one file at `rev` (HEAD, a sha, `:0` = the index) or from the working tree (a file git ignores is refused; a symlink out of the repo is refused): `blob`, `head`, `binary`, `truncated`, `text` |
| `git_ls_files` ✅ | session_id \| folder, rev? | read, launch | tracked files from the index or a tree, at most 100,000 (`truncated` says so) |
| `worktree_list` ✅ | session_id \| folder | read, launch | every worktree: label, branch, dirty, ↑/↓, prunable, the sessions in it, `open` (`session_new` params) |
| `worktree_new` ✅ | session_id, branch, base?, path?, open_session? (true) | launch, writes files, state | `worktree add` (`-b` when new) beside the main checkout; opens a session there like this one, placed after it |
| `worktree_remove` ✅ | session_id, path?, delete_branch?, force? | launch, writes files, discard, state | no `--force`; refuses the main checkout, changes, a worktree listed inside it, any repository on disk inside it (fails closed); closes the folder's sessions (only with `force`) and waits for their exit; sleeping ones are deleted after git removed the folder; `delete_branch` uses `-d` |
| `worktree_prune` ✅ | session_id | launch, writes files | `worktree prune`: forgets worktrees whose folder is gone |

One operation per repository at a time (keyed by the canonical repo root, so worktrees of one repo
share it; a second is refused). After any operation every live session on that repo re-reads its
state. `SessionView` carries `branch`, `git_dirty`, `git_upstream`, `git_ahead`, `git_behind`,
`git_changes`, `git_summary`, `worktree_of`, `repo_root`, `git_busy`, filled by a poll (focused 10 s,
others 120 s, WSL 30 s) plus a shared `.git/HEAD` + `index` watcher; an OSC 9001 push wins.

### notes, search, project docs ✅
Built in `csm-engine` (`tools/search.rs`) over the `csm-index` crate (SQLite + FTS5, one DB thread that owns the connection: this replaces the WPF `DbGate`).

| id | params | effects | what |
|---|---|---|---|
| `note_get` ✅ | folder? \| session_id? | read content | the note for a folder (sessions in one folder share it); returns `text`, `updated_at`, `shared_by`. Refused for SSH sessions and sessions with no folder |
| `note_set` ✅ | folder? \| session_id?, text | state | replace the note (blank deletes; ≤ 256 KiB; control characters other than tab/newline dropped) |
| `notes_import_wpf` ✅ | path? | state, read content | import `project_notes` from the WPF app's `output.db` (opened read-only; newer wins) |
| `search_output` ✅ | query, limit? (≤ 500), session_id?, group_id?, advanced? | read content | FTS5 search of everything sessions printed, newest first: session, time, excerpt, matched ranges. The query is one quoted phrase unless `advanced: true` (AND / OR / NOT / prefix*); a syntax error is a plain message |
| `search_notes` ✅ | query, limit? | read content | case-insensitive literal search over notes (and folder names) |
| `search_reopen_session` ✅ | session_id | state, launch | reopen a session a search result named but that is closed, from its kept configuration (`session_history`) |
| `project_docs_list` ✅ | session_id? \| path? | read | the project docs of a session's folder (or an absolute folder): `README.md`, `CLAUDE.md`, `AGENTS.md`, `CLAUDE.local.md`, `GEMINI.md`, `.claude/CLAUDE.md`, matched ignoring case, in that order, plus the same names at the top of the git repository when that is another folder (found by walking up to `.git`; no git process). Each with `name`, `rel`, `path`, `scope`, `size`, `modified_ms`; `default` is CLAUDE.md, else AGENTS.md, else README.md, else the first. Refused for SSH sessions. Built in `tools/docs.rs` over `csm_services::project_docs`, on a worker |
| `project_doc_get` ✅ | session_id? \| path?, file | read | one listed doc's text (`file`: its `rel`, `path` or `name`); anything else is refused, compared by canonical path (no traversal, no links out). Cut at 1 MiB (`truncated`); not UTF-8 is decoded lossily (`lossy`). The inspector's Docs tab polls `project_docs_list` every 2 s and re-reads when the shown doc's size or time changes |

**Storage.** `index.db` in the *local* (non-roaming) app data dir, `CodeShellManager2\index.db` (moved once from the pre-v2 `CodeShellManager-rs` folder); a state file elsewhere (`--config-dir`, `CSM_CONFIG_DIR`, `CSM_STATE_PATH`, portable) gets it beside it. `--clean` and an isolated run (`CSM_CONFIG_DIR` empty) index **in memory**: nothing reaches the disk. Output is fed from the PTY reader thread as raw bytes (non-blocking; over a byte bound a chunk is dropped and counted), then ANSI-stripped (including the bundled ConPTY's `ESC[1t ESC[c`), cut into lines, batched and written on the DB thread, and trimmed to the newest 20 MiB per session. Schema is versioned with `PRAGMA user_version`; a file that cannot be opened is moved aside as `index.db.bad-<ms>`.

### app
✅ `app_info`, `settings_get/set`, `recently_closed_list`, `session_recent` (the Recent menu, Ctrl+Shift+E), `state_import_wpf`, `app_quit` (force?), `grant_list` / `grant_revoke`, `wsl_distros` (read; the new-session panel's distro list, probed off the engine thread), `open_folder`, `open_with`, `open_url` (a link Ctrl+clicked in a terminal; absolute http/https only), `open_with_list`, `open_with_set`, `open_with_detect`, `open_with_pending`, `open_with_pending_answer`, `consent_list`, `consent_answer` (UI only).
`app_quit` with `force: true` skips the remaining exit waits, also while a quit already runs (the shutdown board's "Force close now"). Grants are kept in `state.json` (`McpGrants`); `grant_revoke` takes `client` and an optional `tool` (none = all).

**Upgrading from 0.x** (`tools/app.rs`, `tools/open.rs`, mapping in `csm_core::wpf_settings`).
`state_import_wpf` (path?) reads the WPF `state.json` on a worker (never writes it): sessions and
groups merge by id (asleep), and the 0.x settings that map onto ours are taken value by value
(garbage skipped and logged; `SettingChanged` is emitted for each, so the UI follows even when the
first-run import calls the engine directly). Its Open with list runs programs, so it is sanitised
(`sanitize_imported`) and only offered: the entries not listed yet wait in `PendingOpenWith`
(state.json, the snapshot's `pending_open_with`), shown on a one-time card. ✅ `open_with_pending`
(read: the entries and the review text) and ✅ `open_with_pending_answer` (`adopt_open_with`:
true adds them, 0.x's default becomes the default; false discards). `state_import_wpf` and the
answer are refused for MCP clients before any consent card (they decide what runs). The table of
every 0.x setting is in docs/release.md ("0.x settings").

**Update check** (`tools/update.rs`, rules in `csm_core::update`, request in
`csm_services::update`). ✅ `update_check` (network, no consent card) asks GitHub's releases of
`umage-ai/CodeShellManager` on a worker (10 s deadline; `releases/latest` for a final build, the
release list for a pre-release build, which also sees newer pre-releases of its own kind) and
replies with `current`, `latest`, `update_available`, `url`; it runs whatever `CheckForUpdates`
says, also under `--clean`. On its own the engine checks 5 s after start and then daily, only with
`CheckForUpdates` on, in a run that saves state and for a released 2.x build
(`StateOptions::update`; tests and the demo have no network). A newer release is the snapshot's
`update` (the title bar's badge, which opens the release page with `open_url`). ✅ `update_dismiss`
(version?; ui) hides it for that version. Details: docs/release.md ("Update check").

**Terminal profiles and looks** (`tools/look.rs`, parsing in `csm-services::wt_profiles`). ✅ `profile_list` (read; opt-in via the `ImportWindowsTerminalProfiles` setting, refused otherwise) reads Windows Terminal's `settings.json` (Stable, Preview, unpackaged; JSONC) **on a worker thread** and returns each visible profile with its command line, starting folder and `look`. ✅ `session_look_set` (session_id + exactly one of `profile` (name), `look`, `clear: true`; state) stamps a look onto the session; `session_new` takes `look` too. A look is colours (16 + fg/bg/cursor/selection), font size, cursor shape/blink, padding, opacity, retro; it is stored in the WPF app's `Profile*` fields of `state.json` (same formats), survives restart and Recently closed, and stays put if the WT profile changes later. The renderer draws the colours, font size, cursor shape and padding; font face, weight, ligatures, blink, opacity and retro are stored but not drawn.

**Backup, usage, slash commands** (`tools/backup.rs`, `tools/usage.rs`, `tools/slash.rs`).
`state_export` (path) writes the whole state as `state.json`-shaped JSON (the WPF `.csm`) on a
worker; an MCP caller's export blanks the API key and grants. `state_import` (path, dry_run?,
force?, adopt_open_with?) follows the WPF Import: the backup's sessions and groups *replace* ours
(ours are closed, so they sit in Recently closed; replacing needs `force`), the WPF list of
settings is taken (a blank API key never erases ours; the MCP offer and grants stay ours), and the
backup's Open with list is sanitised with `sanitize_imported` and adopted only with
`adopt_open_with` (the UI shows the `dry_run` listing and a default-off checkbox). Deliberate
difference: imported sessions arrive asleep (WPF restored them, running whatever the file named).
Refused for MCP clients before any consent card. `usage_stats` reads the index's `usage_stats`
table (schema 2; per command and UTC day, the WPF shape): a start is counted when a session's
process spawns, its running time when it exits or is torn down. `session_send_command`
(session_id, command, force?) types one of the `ClaudeCommands` setting's entries (default /help,
/clear, /compact, /status, /memory, /init, /remote-control, exit; app-only key) and presses Enter separately; any
other text is refused (use `session_send_text`, which pastes). File drops onto a pane go through
`session_send_text` with `csm_core::dropped_paths` quoting (control and format characters
rejected, Windows double quotes / POSIX single quotes, ≤ 64 paths).

**Claude usage** (`tools/usage.rs`, reader in `usage_tail.rs`, parsing and pricing in
`csm-services::claude_usage`, summaries in `csm_core::claude_usage`; a port of ContextCost). App
toolset, all read-only but `usage_forget_history`. Costs are **API-equivalent** (API list prices, not what a subscription
bills); a model without a price is listed as unpriced, never counted as $0.

The Claude numbers are the built-in **Claude usage** add-on (`builtin:claude-usage`, off by
default; `addon_enable` / `addon_disable`, see docs/addons.md "Built-in add-ons"). While it is off,
`usage_session`, `usage_summary` and `usage_report` are refused ("Claude usage is off — enable it in
Settings › Add-ons"), `usage_context` answers the context fill only (`cost_usd` null) from an
in-memory tail, and `session_subagents` lists subagents without cost; nothing is stored. The
history import also needs the setting `ClaudeUsageHistory` (default on) and never runs under
`--clean` or an isolated run.

| id | params | effects | what |
|---|---|---|---|
| `usage_session` | session_id | read | context fill of the last main-thread request vs the window (200k; 1M for a `[1m]` model or a prompt over 200k), with `ok`/`warn`/`critical` near auto-compact; cost and tokens total, today (local) and of subagents; cache hit ratio; the last cache miss and the idle gap before it; compactions (count, last pre → post); data quality (unreadable lines) |
| `usage_context` | | read | fill and cost of every live Claude session (the sidebar chips) |
| `usage_summary` | from? (today/week/month/all/YYYY-MM-DD, default week), to? (today/YYYY-MM-DD), group_by? (project/session/model) | read | cost and tokens per group, most expensive first, with a total |
| `usage_report` | from? (today/7d/30d/all/YYYY-MM-DD, default 7d; `week`/`month` too), to? (today/YYYY-MM-DD, inclusive), group_by? (session/project/model/day, default session), session? / model? / project? (a group key: drill down) | read | the Usage dashboard's numbers over every transcript on this machine: `total`, `days` (one per local day of the range, oldest first) and `rows` (per group, costliest first; days newest first; at most 500, `truncated`), each with tokens split into input, output, cache read, 5m and 1h cache writes, `cost_usd` and `cost_by_class`, `cache_hit_rate`, `cache_savings_usd`, `cache_write_premium_usd`, `cache_miss_cost_usd`, `miss_events` and their `miss_event_cost_usd`, unpriced requests and models; session rows name their CSM session (or project and Claude's conversation id), projects, models (costliest first) and first/last request; `misses`: the 25 costliest cache-miss events; `history` (the reader's progress), `price_version`, `definitions` |
| `usage_forget_history` | | state, discard | deletes what the history import stored (rows of transcripts no CSM session ran); refused while `ClaudeUsageHistory` is on |

- **Which transcript.** A new Claude session starts with `--session-id <uuid>` chosen by CSM and
  stored on the session (`ClaudeSessionId` in `state.json`); a resume passes `--resume <that id>`
  (or, when it has no transcript yet, `--session-id` again). Sessions from before fall back to the
  newest transcript whose `cwd` is their folder. A resume of a session that is the only CSM Claude
  session in its folder takes a newer transcript of that folder over the stored id (a `/clear`
  starts a new conversation id CSM is not told about); with two in one folder the stored id
  always wins. Duplicates get their own id; editing the folder, command or kind forgets it. Not
  followed live: `/clear` (until the next resume), WSL and SSH sessions (`usage_session` refuses
  them). A conversation id with no transcript yet is looked for in its folder's project
  directory every 2 s and in every project folder every 10 s.
- **Reading.** A background thread (`csm-usage`) follows `<root>/projects/*/<id>.jsonl` and
  `<id>/subagents/**/agent-*.jsonl` under `$CLAUDE_CONFIG_DIR`, `~/.claude` and the other
  `~/.claude-*` profiles, reading only bytes past the stored offset (a shorter file or a different
  head is read again from 0). Requests are de-duplicated by `message.id` keeping the maximum per
  token field; `<synthetic>` is skipped; a missing 5m/1h split counts as 5m writes.
- **History.** The same thread also reads every other transcript under the roots (ContextCost's
  import): listed newest first at start and every 60 s, up to 8 changed files per pass between
  the followed sessions' passes, unchanged ones skipped by size and time. Their rows have no CSM
  session (a report keys them by Claude's conversation id) until a CSM session follows that
  transcript, which then takes them over (`claim`). This is what `usage_report` reads.
- **Storage.** `index.db` schema 3: `claude_requests` (numbers only, never content), `claude_files`
  (offsets, counters), `claude_compactions`, `claude_meta`; in memory under `--clean`.
- **Prices.** Embedded (`csm-services/src/claude_usage/prices.json`, ContextCost's table with a
  `version`: the date it was checked against the list prices), overridden or extended by
  `usage-prices.json` beside `state.json` (same shape); a changed table reprices every stored row.
- **Report definitions** (`csm_core::usage_report`, ContextCost's `queries.js`): prompt tokens =
  input + cache read + cache writes; cache hit rate = cache read / prompt tokens; cache savings =
  cache read × (input rate − read rate); write premium = 5m writes × (5m rate − input rate) + 1h
  writes × (1h rate − input rate); cache-miss cost = input × (input rate − read rate) + writes ×
  (write rate − read rate), what the tokens not read from the cache cost above cache reads; a
  cache-miss event is a request after the first of its stream (a conversation's main thread or
  one subagent) writing over 20,000 tokens while reading under half the previous request's
  prompt, its impact its writes × (write rate − read rate). These use the request's tier
  (long-context) rates without the fast multiplier, as ContextCost does; `cost_by_class` includes
  it, so the classes add up to the cost. Days are local. The report runs on the index thread
  (about 0.2 s for 150,000 requests in a release build).
- **UI.** Inspector → Usage (the focused session, refreshed every 5 s while shown), a fill chip on
  Claude rows in the sidebar (`usage_context` every 10 s; cost too on wide rows; it gives way on
  narrow ones), Settings → Usage: cost per project today and this week.
- **Usage dashboard** (`screens/usage_page.rs`, `docs/screens/21-usage*.png`): a page over the
  work area like Settings, opened from the palette (`usage_report`), the Usage tab's "Usage by
  model and day" (drilled into that session) and Settings → Usage; Esc closes it. Range (Today, 7
  days, 30 days, All, Custom from–to), five figures (cost, tokens, cache hit rate, cache-miss cost
  with the miss events, saved by caching; the whole story on hover), cost or tokens per day as
  bars stacked by token class (cache read, 5m and 1h writes, input, output; a day's split on
  hover), a table by session, project, model or day (sortable by any column, locally) whose rows
  drill down — a session to its models and days, a model to its sessions, a day to that day —
  with a breadcrumb back, and the costliest cache misses. It asks `usage_report` when what it
  shows changes and every 30 s while open; the demo answers from a synthetic history.

**Open folder / Open with** (`tools/open.rs`, rules in `csm-services::open_with`). `open_folder`
(session_id) opens only an existing *directory* (a file would be run); Windows normalises to
backslashes and hands Explorer the path as one always-quoted argument — measured on Windows 11, a
forward-slash path or an unquoted path with a comma opened Documents instead; macOS `open`, Linux
`xdg-open`; SSH sessions are refused. `open_with` (session_id, entry? id or label) runs
`prepare` (template split first, one placeholder = one argv element, nothing option-shaped, no
`/switch` on Windows, batch files refuse cmd metacharacters, the program's real path via
`std::fs::canonicalize`: not on a network drive, not under `%SystemRoot%`, not a shell, not a
renamed shell by its PE `OriginalFilename`) and starts the program with argv, no shell.
`open_with_list` (session_id?) adds each entry's exact command for that session (the menus show
it as the tooltip, with the program's file name beside the label). `open_with_set` (entries)
replaces the list after the same checks and is **refused for MCP clients** (as `settings_set`
refuses `OpenWith`). `open_with_detect` finds VS Code / Insiders / Cursor / Windsurf / VSCodium
(PATH shim → exe, App Paths, install folders, Scoop), Visual Studio (vswhere), JetBrains
(standalone, Toolbox 1.x and 2.x), Zed, Sublime, Notepad++; on macOS `/Applications` bundles via
`open -a`, on Linux `.desktop` files and PATH. The list from state.json is sanitised on load and
seeded once by detection (`OpenWithSeeded` set only when something was found).

### tasks ✅ (the Board)
Built in `csm-engine` (`tools/tasks.rs`); the model, state machine and permission rules are pure,
in `csm_core::tasks`. Its own toolset (`tasks`), not `core`.

| id | params | effects | what |
|---|---|---|---|
| `task_list` ✅ | project?, state?, assignee?, include_closed? | read | open tasks (done/cancelled with include_closed); an agent gets its own project's |
| `task_get` ✅ | task_id | read | one task, with its body |
| `task_new` ✅ | title (≤ 200 chars), body? (≤ 16 KiB), project? \| session_id? | state | a Backlog task; an agent's goes to its own project |
| `task_update` ✅ | task_id, title?, body?, progress? (≤ 500), order?, cancel? | state | cancel is the person's |
| `task_claim` ✅ | task_id, session_id? | state | an unassigned backlog task → Assigned (an agent: to itself); nothing is sent |
| `task_done` ✅ | task_id, note? | state | Assigned → Review |
| `task_handoff` ✅ | task_id, note? | state | Assigned → Backlog, unassigned |
| `task_assign` ✅ | task_id, session_id, send? (true), force? | state, input | → Assigned; with send, pastes `Task <id> from <creator>: <title>` + body and Enter through `session_send_text`'s path (bracketed paste and its refusals: asleep, starting, exited, mid-turn without force, the caller's own session); a refused paste changes nothing; force also takes it from another assignee |
| `task_accept` ✅ | task_id | state | Review → Done; refused for MCP callers (in precheck, before any card) |
| `task_reopen` ✅ | task_id, note? | state | Review/Done/Cancelled → Assigned (to whoever had it) or Backlog; refused for MCP callers |
| `task_delete` ✅ | task_id | state, discard | asks MCP callers |

- **Project** = the session's git repository (`repo:<main checkout root>`, shared by its
  worktrees), else its group (`group:<id>`; `group:` when ungrouped). Every session view has
  `project` / `project_name`. A session whose git probe hasn't answered yet counts as its group.
- **Agents** (`Caller::External { session_id }`), decided 2026-11-20: task bookkeeping (new,
  claim, update, done, handoff) runs without a consent card, but only in the agent's own project,
  only on tasks assigned to itself (or unassigned ones, to claim), and with at most 50 open tasks
  created per session. No cancel, accept or reopen. Assign and delete stay in its project and
  ask (their effects). An MCP client outside our sessions may read, and assign or delete (asking),
  but does no bookkeeping. The rules also run in `precheck`, so a call they refuse never asks.
- **Untrusted text**: titles, bodies and notes lose control characters (ESC included, so no
  sequence such as a fake end-of-paste survives) and bidi overrides; text over the caps is refused.
- **Storage**: `tasks.json` beside `state.json` (atomic write, one `.bak`, an unreadable file kept
  aside as `tasks.json.bad-<ms>`), written by its own saver thread on a debounce and on exit.
  Nothing is read or written under `--clean` or an isolated run. A closed session's Assigned
  tasks go back to the backlog (also at start-up, for sessions gone while the app was closed).
- **Status log** (for a later timeline): `index.db` schema 4, `status_log (session_id, status,
  start_ms)`; a status shorter than 2 s is merged into the next; rows older than 30 days are
  deleted when the index opens. Every session view has `status_since_ms`.
- **UI**: the Board layout (`layout_set { mode: "board" }`, beside Overview), scoped by the rail
  tab; a group's Board goes along with the group tear-off ("Own window" → `group_tear_off`).
  Columns Backlog | Working | Needs you | Review | Idle | Done; drag a backlog card onto an agent
  card → a confirm with the exact prompt → `task_assign`; Review task cards have Accept / Reopen;
  an idle agent card has "+ task" (`task_new`, then `task_assign`); a task card's context menu
  cancels or deletes (with a confirm).

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
Measured on 2026-11-20 after the Board merge (the `tasks` toolset; 112 engine tools; the app's own `pane_*` tools are not in the engine registry, so they are not counted). Re-run it after adding tools, and keep
`core` small: it is what every client pays on every conversation.

| toolset | tools | bytes | est. tokens |
|---|---|---|---|
| base (core + toolset tools) | 26 | 20543 | ~5135 |
| + groups | 19 | 11434 | ~2858 |
| + layout | 9 | 6197 | ~1549 |
| + runs | 5 | 3069 | ~767 |
| + git | 9 | 6312 | ~1578 |
| + notes | 3 | 2142 | ~535 |
| + search | 3 | 2395 | ~598 |
| + app | 27 | 18679 | ~4669 |
| + tasks | 11 | 7639 | ~1909 |
| everything enabled | 112 | 78410 | ~19602 |

Progressive disclosure is worth about 3x here: a client that only needs sessions pays ~5.1k
tokens instead of ~19.6k.

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

### Engine registry (181 tools)

Generated from `csm-engine`'s registry. "asks" means an MCP caller waits for the person's consent.

| id | toolset | effects | MCP consent | what |
|---|---|---|---|---|
| `tool_list` | core | read | no | Every tool: id, title, summary, toolset and effects. Filter by toolset. |
| `consent_list` | app | read | no | Requests from MCP clients waiting for the person to allow or deny them. |
| `consent_answer` | app | state | no | Allow once, allow always for this client, or deny a waiting request. App only. |
| `session_list` | core | read | no | Every session in sidebar order: id, name, kind, folder, group, status (working, needs_you_approval, needs_you_question, done_unseen, error, idle, asleep, starting, exited), branch, remote_control (Claude Code Remote Control is on) and remote_control_url. |
| `session_get` | core | read | no | One session in full: its launch settings and its status, including the program-status message and whether Claude Code Remote Control is on (with its claude.ai link). |
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
| `session_output_stats` | core | read | no | A session's output stream: bytes per second (average over the last minute and the busiest second), total bytes, bytes dropped while the output tap was busy, and the output ring's size, offsets, evictions and resyncs. Numbers only; no output text. |
| `session_wait` | core | read | no | Wait until a session is idle, done, needs you, has exited, or changes status at all (until), up to timeout_s (default 60, max 600). |
| `session_status_clear` | core | ui | no | Forget a session's reported status (for a status stuck after a program died mid-turn); the screen heuristic takes over again. |
| `session_accent_reset` | core | state | no | Forget the accent colour a program pushed with OSC 9001; the session goes back to its folder's colour. |
| `inbox_list` | core | read | no | Sessions waiting for you (oldest first), then finished or failed turns you have not seen. |
| `inbox_next` | core | ui, launch | asks | Focus the next session in the inbox. Wakes it if it is asleep. |
| `claude_reply_list` | core | content | asks | The last replies (default 10, at most 50) Claude wrote in a local Claude session, newest first (index 0 is the latest): each with its id, a one-line preview, its line count and its code blocks (language and preview). Read from the conversation's transcript; WSL and SSH sessions are not available. |
| `claude_reply_get` | core | content | asks | One reply Claude wrote in a local Claude session, exactly as written (Markdown; the text of the turn's assistant messages, without tool calls). index 0 (the default) is the latest; reply_id picks one from claude_reply_list instead. Read from the conversation's transcript; WSL and SSH sessions are not available. |
| `claude_code_block_get` | core | content | asks | One code block of a reply Claude wrote, exactly as written, without the fences: block_index (default 0, the first) of the reply at reply_index (default 0, the latest) or reply_id. Read from the conversation's transcript; WSL and SSH sessions are not available. |
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
| `session_duplicate` | core | state, launch | asks | Open a copy of a session: same folder, command, arguments, kind, connection, group, terminal profile and run commands, named like "api (2)" and placed right after it. A Claude copy starts a new conversation. |
| `session_sort` | groups | state | no | Reorder the sidebar once by name, folder, last_active, branch, dirty or repo (descending defaults to the field's natural direction: newest and dirty first, text A to Z). group limits it to one group's sessions ("" = ungrouped), which keep their slots; default all. |
| `session_sleep_many` | groups | state, discard | asks | Put several sessions to sleep (sleeping ones are skipped). |
| `session_wake_many` | groups | state, launch | asks | Wake several sleeping sessions (awake ones are skipped). Claude sessions resume their conversations, spaced by the launch stagger. |
| `session_restart_many` | groups | launch, discard | asks | Restart several running sessions one after another (sleeping ones are skipped). Needs force: true for more than one (it takes a while). Replies when all have started again. |
| `session_close_many` | groups | state, discard | asks | Close several sessions (they go to Recently closed). Refuses when any is busy unless force is true. |
| `session_move_many` | groups | state | no | Put several sessions in a group (group_id null = Ungrouped), keeping their order, at the end of the group or right before or after another session of that group. |
| `session_send_text_many` | groups | input | asks | Paste the same text into every live session of a group (group_id) or into the given sessions (session_ids), as session_send_text does for one: bracketed paste, submit: true presses Enter after it. Asleep, starting and exited sessions, sessions working mid-turn (unless force) and the caller's own session are skipped and listed. |
| `view_get` | layout | read | no | The group view (rail, headers, flat), the rail tab, the focused session, the inspector and the inbox panel. |
| `view_set` | layout | ui | no | Switch the group view (rail, headers, flat) and/or the rail tab (a group id, or "all"). |
| `layout_get` | layout | read | no | A group's layout (default: the one shown now): mode, grid size, pinned sessions. |
| `layout_set` | layout | ui | no | Set a group's layout: mode focus, side (pinned side by side), overview, grid (preset 1, 2, 4x1, 6x1, 2x2, 3x2, 6x2, 3x3, or cols x rows; a custom size is remembered for the Grid menu, newest first, at most 6), free, or board (sessions as cards by status, beside the tasks). |
| `grid_preset_forget` | layout | ui | no | Remove a remembered custom grid size (cols x rows) from the Grid menu. |
| `layout_pin` | layout | ui | no | Pin a session into the side-by-side layout. |
| `layout_unpin` | layout | ui | no | Remove a session from the side-by-side layout. |
| `inspector_show` | layout | ui | no | Show the inspector on a tab (notes, docs, runs, git, session, usage) or hide it. |
| `inbox_panel_set` | layout | ui | no | Open or close the Needs-you panel and/or set its height. |
| `inspector_panel_set` | layout | ui | no | Set the inspector's width in logical pixels (200-2000), as dragging its left edge does; `reset: true` goes back to the default. |
| `window_placement_set` | layout | ui | no | Remember where a window is: the main window, a torn-off group or a torn-off pane (physical pixels, the monitor's DPI, maximized). The app sends it as windows move; it applies at the next start or tear-off. |
| `app_info` | app | read | no | Version, the ConPTY in use, where the state is kept, session counts. |
| `settings_get` | app | read | no | All settings (or one, by key). Keys as in state.json (PascalCase) or snake_case. Secrets are not returned. |
| `settings_set` | app | state | no | Set one setting by key. Settings that decide what runs (default command, launch commands, editors, API key, ConPTY) can only be changed in the app. |
| `recently_closed_list` | app | read | no | Closed sessions that can be reopened, newest first (index 0). |
| `session_recent` | core | read | no | The sessions used most recently (up to 10, live or asleep, newest first), without the focused one: the first is the session used before it. Each has its status and how long ago it was used (5m, 3h, 2d). `session_focus` goes to one (waking it). |
| `state_import_wpf` | app | state | no | Import sessions, groups and settings from CodeShellManager 0.x's state.json (default: its usual location; the file is only read). Imported sessions arrive asleep; existing ones are kept. Its Open with list is not applied: it waits for the person's answer (open_with_pending). App and CLI only. |
| `app_quit` | app | discard | asks | Close every session (Claude ones get time to exit cleanly), save, then signal the app to exit. Sessions restore next time. force: true skips the exit waits (also while a quit is already running). |
| `window_show` | app | ui | no | Bring the app's window to the front, restoring it if it is minimized (the tray icon's Show window). |
| `notification_test` | app | ui | no | Show a sample OS notification now, to check that notifications reach you (sent even with notifications off; the sound follows ShowNotificationSound). |
| `grant_list` | app | read | no | Clients allowed to call a tool without asking ("Always for this client" on a consent card). |
| `grant_revoke` | app | state | no | Make a client ask again before calling a tool (or, without `tool`, before calling any tool). |
| `toolset_enable` | core | read | no | Offer a toolset's tools to this MCP client (core is always on). |
| `toolset_disable` | core | read | no | Stop offering a toolset's tools to this MCP client. |
| `storage_info` | core | read | no | How much disk the search index uses: file size, free space inside it, terminal output (bytes, rows, sessions), Claude usage rows and status rows, with the limits that keep it in check (IndexMaxMb, ClaudeUsageRetentionDays, the 30-day grace for closed sessions). |
| `storage_clean` | core | state, discard | asks | Prune the search index now, as the hourly clean-up does: output of sessions that are gone and were not seen for 30 days, the oldest output over IndexMaxMb, Claude usage rows older than ClaudeUsageRetentionDays; then give the freed space back to the disk. |
| `update_check` | app | network | no | Ask GitHub now whether a newer CodeShellManager is released (a beta or rc build also sees newer pre-releases of its kind). Returns the running and the newest version and the release page; nothing is downloaded. Runs even with CheckForUpdates off. |
| `update_dismiss` | app | ui | no | Hide the title bar's "Update available" badge for this version until the app restarts (a newer release shows it again). |
| `app_autostart_get` | app | read | no | Whether the OS starts CodeShellManager when you sign in, read from the OS itself (the Windows Run key, a macOS LaunchAgent, an XDG autostart entry): enabled, whether Task Manager or the desktop turned it off, and the program it starts. |
| `app_autostart_set` | app | launch, files | asks | Start CodeShellManager when you sign in (enabled: true) or not (false). Writes or removes the current user's OS entry for this copy of the app, started with --autostart; turning it on also clears a Task Manager disable. |
| `git_status` | git | launch | asks | A session folder's git state, read now: branch, upstream, ahead/behind, the changed files, which repo it is a worktree of, and its GitHub pages when `origin` (else the tracked remote) is on github.com. |
| `git_fetch` | git | launch, network | asks | git fetch in the session's repository (never prompts for a password; sign in from a terminal once if it fails). |
| `git_pull` | git | launch, files, network | asks | git pull --ff-only --no-rebase: moves the branch only when that loses nothing; a diverged branch or a conflicting local edit is refused with git's reason. |
| `git_switch_branch` | git | launch, files | asks | git switch --no-guess to a local branch. A branch checked out in another worktree is refused (open that worktree instead; `git_branches` says which). |
| `git_branches` | git | launch | asks | Local branches; for each one checked out in another worktree, that worktree and its session. |
| `worktree_list` | git | launch | asks | Every worktree of the session's repository (or, with `folder`, of the repository a folder without a session is in): branch, changes, ahead/behind, the sessions in it, and the `session_new` params to open one there. |
| `worktree_new` | git | state, launch, files | asks | git worktree add for a branch (created from `base` or HEAD when it doesn't exist), next to the main checkout by default; opens a session there with the same command, placed after this one, unless open_session is false. |
| `worktree_remove` | git | state, launch, discard, files | asks | Remove a linked worktree with git worktree remove (no --force). Refuses the main checkout, a worktree with changes, and one with another repository inside it. Closes the sessions in that folder first (needs force: true when there are any) and waits for them to exit; delete_branch uses branch -d, which keeps an unmerged branch. |
| `worktree_prune` | git | launch, files | asks | git worktree prune: forgets worktrees whose folder is already gone. Only git's bookkeeping; never a working tree. |
| `git_diff` | git | launch | asks | The changed files and the unified patch of a session's repository (or an absolute `folder`): `working` (working tree against HEAD, plus untracked files), `staged`, or `range` (`base...head`). Read-only: no external diff program, no textconv. The patch is cut at max_bytes (default 512 KiB) and says so. |
| `git_show_file` | git | launch | asks | One file's text: at `rev` (HEAD, a sha, `:0` for the index) or, with no rev, from the working tree (a file git ignores is refused). Returns the blob id, whether it is binary, and the text cut at max_bytes (default 512 KiB). |
| `git_ls_files` | git | launch | asks | The tracked files of a session's repository (or an absolute `folder`), from the index or from the tree of `rev`. At most 100,000 paths. |
| `run_list` | runs | read | no | A session's run commands and the state of each one's last run (running, ok, failed, stopped). |
| `run_start` | runs | launch | asks | Start a run command (default: the session's default) in its own background terminal; never types into the session. Restarts it if it is already running. |
| `run_stop` | runs | discard | asks | Stop a running run command (default: the default one) and everything it started. |
| `run_output` | runs | content | asks | The ANSI-stripped output of a run command's last run (kept up to 1 MB; `max_chars` returns the tail). |
| `run_edit` | runs | state, launch, discard | asks | Replace a session's run commands (label, command line, mode process/powershell, URL, default), `detect: true` to take the project-type template instead, or `import: true` to add the template's commands that aren't in the list yet (a Node project's package.json scripts). Asks MCP clients first: it decides what F5 runs. |
| `run_set` | runs | state, launch | asks | Add one run command, or update the one whose id or label matches (`id`, else `label`); the rest of the list is kept. Asks MCP clients first: it decides what F5 runs. |
| `run_remove` | runs | state, launch, discard | asks | Remove one run command (by id or label) and stop its run. If it was the default, the first remaining one becomes the default. Asks MCP clients first. |
| `new_session_sources` | addons | read | no | The New session forms enabled process add-ons offer (their title, the choices they have loaded and the state of the last submit), for the dialog and MCP clients. |
| `new_session_load` | addons | state, launch, network | asks | Ask the add-on for the choices of one of its New session forms (its own choices tool). Starts the add-on's process if needed and may reach the network, so MCP clients are asked first. |
| `new_session_submit` | addons | state, launch, files, network | asks | Run one of an add-on's New session forms with a choice from its loaded list (for example create an agent in the chosen project), and the name typed in its name field when it has one. The add-on's tool does the work. Its remote opens only for the person in the app; any other caller is told which remote to open. Starts processes and may reach the network, so MCP clients are asked first, and the consent card names the add-on tool the form runs. |
| `open_folder` | app | launch | asks | Open the session's folder in the file manager (existing folders only; local and WSL sessions). |
| `open_with` | app | launch | asks | Open the session's folder in an editor from the Open with list (default: the default entry). `entry` is an entry id or label. |
| `open_url` | app | launch | asks | Open an absolute http or https URL in the default browser (a link clicked in a terminal). With `session_id` of an SSH session, a loopback link (localhost, 127.x, ::1) opens through a port forward to that remote host, or is not opened and reports `needs_forward`. Other schemes, relative URLs and URLs with spaces or quotes are refused; no shell is involved. |
| `open_with_list` | app | read | no | The Open with list; with `session_id`, also the exact command each entry would run for that session. |
| `open_with_set` | app | state | no | Replace the Open with list (label, program, argument templates per session kind, default). Checked and cleaned; app and CLI only. |
| `open_with_detect` | app | state, launch | asks | Look for installed editors and IDEs and add the ones not listed yet. |
| `open_with_pending` | app | read | no | The Open with entries imported from CodeShellManager 0.x that wait for the person's answer, each with the program and arguments it would run. open_with_pending_answer adds or discards them. |
| `open_with_pending_answer` | app | state | no | Answer the pending Open with entries from CodeShellManager 0.x (open_with_pending lists them): adopt_open_with: true adds the ones not listed yet (0.x's default becomes the default), false discards them. App and CLI only. |
| `session_port_forward` | core | state, launch, network | asks | Start an SSH port forward: a port on an SSH session's remote host (its loopback) answers at 127.0.0.1 on this computer, until the session closes, restarts or the app quits. The local port is the remote one when free, else a free one (or `local_port`). `open: true` opens the address. Refused for local and WSL sessions. |
| `session_port_forwards` | core | read | no | The running SSH port forwards: session, remote port, local port, the ssh process and its address. With `session_id`, only that session's. |
| `session_port_unforward` | core | state | no | Stop an SSH session's port forward on `local_port`, or all of them with `all: true`. |
| `wsl_distros` | app | read | no | The installed WSL distributions (name, version, state, default), default first. Empty when WSL is not installed or this is not Windows. |
| `profile_list` | app | read | no | The profiles in Windows Terminal's settings (Stable, Preview and unpackaged installs): name, command line, starting folder and look, ready for `session_look_set` or `session_new`'s `look`. Needs the ImportWindowsTerminalProfiles setting. |
| `session_look_set` | app | state | no | Give a session its own terminal look: a Windows Terminal profile's (by name), an explicit look (colours, font size, cursor shape, padding), or none (`clear`). A running session changes at once. |
| `theme_list` | app | read | no | The themes to choose from: the built-in Mocha (dark) and Latte (light), and the themes of enabled add-ons (`addon-id.theme-id`), with the mode and the dark and light choices in force. A choice that is no longer available says so (the app draws the built-in instead). |
| `theme_set` | app | ui | no | Choose the theme: `mode` (dark, light or system, which follows the desktop), the `dark` theme and the `light` theme, by the ids `theme_list` gives. Give any of them. A dark theme only goes in `dark`, a light one only in `light`. The window changes at once. |
| `note_get` | notes | content | asks | The note kept for a folder (give folder, or session_id to use that session's folder). Sessions in one folder share one note; SSH sessions have none. |
| `note_set` | notes | state | no | Replace the note kept for a folder (folder or session_id). An empty text deletes the note. Refused for SSH sessions and sessions without a folder. |
| `notes_import_wpf` | notes | state, content | asks | Import the per-folder notes from the WPF app's output.db (default: its usual location). The file is opened read-only; a note already here is kept unless the imported one is newer. |
| `search_output` | search | content | asks | Full-text search of everything every session printed (ANSI stripped). The query is taken literally as a phrase unless advanced is true (FTS5 syntax: AND, OR, NOT, prefix*). Newest matches first. |
| `search_notes` | search | content | asks | Search every folder's notes for a piece of text (case-insensitive, literal). |
| `search_reopen_session` | search | state, launch | asks | Start a new session from the configuration a closed session had (session_id from a search result). Refused when that session is still in the sidebar. |
| `project_docs_list` | notes | read | no | The project docs of a session's folder (or an absolute `path`): README.md, CLAUDE.md, AGENTS.md, CLAUDE.local.md, GEMINI.md and .claude/CLAUDE.md, matched ignoring case, in that order; also those at the top of the git repository when that is another folder. Name, relative and absolute path, size and modified time of each, and the one to show first. |
| `project_doc_get` | notes | read | no | The text of one project doc (`file`: its relative path, absolute path or name from project_docs_list). Anything that is not one of the listed docs is refused. Cut at 1 MiB (`truncated`); a file that is not UTF-8 is decoded lossily (`lossy`). |
| `session_send_command` | core | input | asks | Type one of the configured Claude slash commands (settings ClaudeCommands: /help, /clear, /compact, /status, /memory, /init, /remote-control, exit) into a session and press Enter. Refused into the caller's own session, and while the session is working unless force is true. |
| `state_export` | app | files | asks | Write a backup of the whole setup (sessions, groups, layouts, settings) to a .csm file at `path` (absolute). The same JSON as state.json; the WPF app's Import reads it. An MCP client's export leaves out the API key and MCP grants. |
| `state_import` | app | state, discard | asks | Read a .csm backup and replace the sessions and groups with its own (ours are closed; Recently closed keeps the last ten; imported ones arrive asleep), taking its settings. dry_run: true only reports what it would do and lists the backup's Open with commands; replacing needs force: true; the backup's Open with list is adopted only with adopt_open_with: true. App and CLI only. |
| `usage_stats` | app | read | no | Per command (claude, pwsh, …): how many sessions started, their total running time in seconds, and the last day (UTC) it was used. Counted on this machine only. |
| `usage_session` | app | read | no | A Claude session's context fill (last main-thread request vs the model's window), API-equivalent cost and tokens (total and today), cache hit ratio, the last cache miss with the idle gap before it, compactions, and subagent cost. Read from Claude's transcripts; numbers only. Needs the built-in Claude usage add-on (off by default). |
| `session_subagents` | app | read | no | The subagents a Claude session has started (Task/Agent calls and dynamic-workflow agents): label, type, model, state (running, idle after 5 minutes without output, done), workflow run and phase, tokens and API-equivalent cost, plus workflow runs with per-phase done/total. Read from Claude Code's own transcript folder (an undocumented layout, parsed leniently); numbers and the agents' short descriptions only. |
| `usage_context` | app | read | no | For every live Claude session: how full its context is (tokens, window, ratio, ok/warn/critical near auto-compact) and its API-equivalent cost so far. With the Claude usage add-on off: the context fill only, cost null. |
| `usage_summary` | app | read | no | API-equivalent cost and tokens of Claude sessions between from and to (today, week, month, all, or a YYYY-MM-DD local date; default the last 7 days), grouped by project, session or model. Unpriced models are listed, never counted as $0. Needs the built-in Claude usage add-on (off by default). |
| `usage_report` | app | read | no | Claude Code's tokens and API-equivalent cost between from and to (today, 7d, 30d, all, or YYYY-MM-DD local dates; default 7d), from every transcript on this machine: totals, one row per local day, and groups (session, project, model or day), each split into input, output, cache writes (5m and 1h) and cache reads, with cost per token class, cache hit rate, cache savings, write premium, cache-miss cost and cache-miss events. Filter by session, model or project to drill down. Numbers only. Needs the built-in Claude usage add-on (off by default); past transcripts are read only with ClaudeUsageHistory on and never under --clean. |
| `usage_forget_history` | app | state, discard | asks | Delete the Claude usage imported from past transcripts that no CSM session ran (the history import). What the sessions CSM ran recorded stays. Refused while "Read past Claude history" (setting ClaudeUsageHistory) is on, since it would be read again. |
| `state_snapshots` | app | read | no | The rolling snapshots of state.json (one an hour for a day, one a day for a week), newest first, with their files and the folder. To go back to one, quit the app and copy it over state.json. |
| `task_list` | tasks | read | no | The Board's tasks: id, project, title, state (backlog, assigned, review, done, cancelled), assignee, progress. An agent sees its own project's; filter by project, state or assignee. Done and cancelled ones only with include_closed. |
| `task_get` | tasks | read | no | One task in full, with its body. |
| `task_new` | tasks | state | no | Add a task to a project's backlog: a title (at most 200 characters) and an optional body (at most 16 KiB). An agent's task goes to its own project; the person names a project or a session_id whose project it is. |
| `task_update` | tasks | state | no | Change a task's title, body, progress note or order, or cancel it (the person only). An agent may update only the tasks assigned to itself. |
| `task_claim` | tasks | state | no | Take an unassigned backlog task: an agent claims it for itself; the person names the session_id. Nothing is sent to the session. |
| `task_done` | tasks | state | no | The assignee says the task is done: it moves to Review, where the person accepts or reopens it. An optional note says what was done. |
| `task_handoff` | tasks | state | no | Give an assigned task back to the backlog, unassigned, with an optional note (why, and how far it got). |
| `task_assign` | tasks | state, input | asks | Assign a task to a session in its project and (send, default true) paste "Task <id> from <creator>: <title>" and the body into it with Enter, as session_send_text does: refused when the session is asleep, starting, exited or working mid-turn (unless force), and for the caller's own session. force also takes a task from another assignee. |
| `task_accept` | tasks | state | no | Accept a task in Review: it is Done. The person only (the app or the CLI), never over MCP. |
| `task_reopen` | tasks | state | no | Send a task in Review (or Done, or Cancelled) back: to its assignee when it has one, else to the backlog, with an optional note. The person only, never over MCP. |
| `task_delete` | tasks | state, discard | asks | Delete a task for good. |
| `addon_list` | addons | read | no | The add-ons in the add-ons folder: each one's name, version, kind, state (loaded, disabled or error) and why, what it contributes (profiles, run templates, slash presets, palette commands) and what was skipped. |
| `addon_reload` | addons | state | no | Read the add-ons folder again. Enabled add-ons whose addon.toml changed are off until enabled again. Answers once the folder is read. |
| `addon_enable` | addons | state | asks | Turn an add-on on. `version` and `digest` must be the ones the person was shown (from addon_list); the engine refuses if the file has changed since. Asks MCP clients first: an add-on can add commands, profiles and process effects. |
| `addon_disable` | addons | state | no | Turn an add-on off. Its contributions go away; enabling it again asks for consent again. |
| `addon_open_folder` | addons | launch, files | asks | Open the add-ons folder in the file manager (created if it is missing). Not available for a --clean or isolated run. |
| `addon_install_samples` | addons | state, files | asks | Copy the sample add-ons that ship with the app (Codex CLI, Gemini CLI, Session summary) into the add-ons folder. A sample whose folder already exists is left alone, never overwritten. They arrive disabled; enabling each one asks for consent as usual. Not available for a --clean or isolated run. |
| `addon_run_apply` | addons | state | no | Add one of an enabled add-on's run templates (`addon-id.name`, from addon_list) to a session's Runs. Refused for MCP clients (run_set adds a run command after asking the person). |
| `plugin_status` | addons | content | asks | Each enabled process add-on: starting, running, crashed (restarting), given up or stopped; its process id, restart count, last error, its tools, and the end of what it wrote to stderr (which can contain anything the add-on printed). |
| `plugin_restart` | addons | launch, discard | asks | Stop an enabled process add-on and start it again (also brings back one that gave up after repeated crashes). Its running tools are unavailable until the handshake is done again. |
| `remote_list` | addons | read | no | The places enabled process add-ons offer to open a session on ("Remote… › add-on name › label"): the add-on, the id to pass to remote_open, a label, a detail line, and whether it opens as given or asks the add-on's tool. |
| `remote_open` | addons | state, launch, network | asks | Open the session an add-on's remote entry describes (`addon` and `remote_id` from remote_list): an ssh session, or whatever the add-on's own tool answers. Starts a process and may reach the network, so MCP clients are asked first. |
| `session_annotate` | addons | ui | no | For an add-on only (through csm/call): set a status override and a short note (for example "running remotely") on a session that add-on opened. Both clear when the add-on stops or is disabled. Other callers are refused. |
| `addon_secret_set` | addons | secrets | no | For an add-on only (through csm/call), and only one that declared `stores_secrets`: keep a secret in the OS keychain under the add-on's own name. Never written to state.json. Other callers are refused. |
| `addon_secret_get` | addons | secrets | no | For an add-on only (through csm/call), and only one that declared `stores_secrets`: read one of its own secrets from the OS keychain (`value` is null when there is none). Other callers are refused. |
| `addon_secret_delete` | addons | secrets | no | For an add-on only (through csm/call), and only one that declared `stores_secrets`: remove one of its own secrets from the OS keychain. Other callers are refused. |
| `peer_status` | app | read | no | Whether another CodeShellManager can connect to this one: on or off, the address it listens on, an open pairing offer, a device waiting for Allow or Deny, and how many devices are paired and connected. No keys and no pairing link. |
| `peer_interfaces` | app | read | no | The addresses of this machine that remote access could listen on: adapter name, IP, prefix length, whether it looks like a WireGuard tunnel. Loopback is listed last. |
| `peer_relay_status` | app | read | no | Whether remote access is also reachable through a relay, which relay, and whether this machine is connected to it right now. No secrets. |
| `peer_relay_set` | app | state, files, network | asks | Also let paired devices reach remote access through a relay (`wss://` address), for networks without a VPN. The relay only copies encrypted frames. The operator's enrolment token, when the relay needs one, is kept in the OS keychain, never in a file. The relay runs only while remote access is on. An empty `url` turns the relay off. Refused for every caller except the app's own window. |
| `peer_server_enable` | app | state, files, network | asks | Let another CodeShellManager connect to this one, on the one address and port you choose (never all interfaces unless `all_interfaces` is set). A paired device can type into your sessions. Refused for every caller except the app's own window: not the CLI, not MCP, not a paired device. |
| `peer_server_disable` | app | state, network | no | Stop listening, close every connected device with a goodbye, close any open pairing offer. Paired devices stay paired (revoke them to delete their keys). Allowed for every caller. |
| `peer_pair_start` | app | state, files, network | asks | Open a pairing offer, valid for 10 minutes and one use, and return its link (`csm://pair#...`) for the other CodeShellManager to open. The link is a secret. Refused for every caller except the app's own window. |
| `peer_pair_answer` | app | state, files | asks | Answer the device waiting to pair: `allow` with the four-digit `check_code` both screens show, or deny. Refused for every caller except the app's own window. |
| `peer_device_list` | app | read | no | The devices paired with this app: id, name, level, when paired, when last seen, whether connected now. |
| `peer_device_revoke` | app | state, network | no | Delete one paired device's key (`id`) or every device's (`all`: true) and close their connections at once. They need a fresh pairing to come back. Allowed for every caller. |
| `peer_connect_open` | app | ui | no | Open the dialog that pairs this app with another CodeShellManager from its `csm://pair#...` link, shows the check code, and lists the paired offices with Connect and Forget. Only the app's own window opens it; refused for every other caller. |
| `remote_window_open` | app | ui, launch | asks | Start a window of this app that runs on a paired CodeShellManager (the office) instead of a local engine. It is a separate process with its own layout, focus and theme. Only the app's own window starts one; refused for every other caller. |
| `peer_client_pair` | app | state, files, network | asks | Pair this app with another CodeShellManager from its `csm://pair#...` link. It starts in the background and returns at once; `peer_client_pair_status` shows the four-digit check code to compare with the other screen, and the result once the other person has allowed or denied it. Refused for every caller except the app's own window. |
| `peer_client_pair_status` | app | read | no | The state of the latest pairing with another CodeShellManager: connecting, the check code to compare, paired, failed (with why) or cancelled. No keys and no link. The check code is shown to the app's own window only. |
| `peer_client_pair_cancel` | app | state | no | Stop a pairing that waits for the other CodeShellManager to answer. Nothing is stored. Allowed for every caller. |
| `peer_hosts_list` | app | read | no | The other CodeShellManagers this app is paired with: id, name, address, when paired and when last connected. |
| `peer_host_forget` | app | state, files | asks | Remove one paired host from this app and the host key stored for it (`host_id`, from `peer_hosts_list`). This app then cannot connect to that host until it is paired again. This app's own device key is kept, since other hosts may still know it. |
| `screen_list` | addons | read | no | The screens running add-ons offer (id, title, scope: project, session or global) and the screens that are open now (instance, its add-on, phase, whether it is in its own window, why the last view was refused). |
| `screen_open` | addons | ui | no | Open one of a running add-on's screens (from screen_list), on a session, a project or a folder, as a tab above the panes. The same screen on the same context is focused instead of opened twice. Returns the instance id. An add-on has at most 8 screens open. |
| `screen_close` | addons | ui | no | Close an open add-on screen (the add-on is told). Its drafts are lost; the add-on's own data is not touched. |
| `screen_pop_out` | addons | ui | no | Move an open add-on screen into its own window. |
| `screen_dock` | addons | ui | no | Move an add-on screen from its own window back into the tab strip. |

<!-- xtask:tools:end -->

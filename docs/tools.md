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
| `session_list` ✅ | group?, status?, include_asleep? | read | id, name, kind, folder, group, status, branch, live/asleep |
| `session_get` ✅ | session_id | read | one session in full, incl. program-status records |
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
`layout_set` (group, mode focus/side/overview/grid/free, grid preset or cols×rows, `free`: the Free layout as an opaque object ≤ 64 KiB),
`layout_pin` / `layout_unpin` (session), `pane_float` / `pane_dock` / `pane_tear_off`,
`pane_minimize` / `pane_maximize` / `pane_restore` (session, group; the pane header's window bar,
effects `ui`: minimized and maximized are stored on the engine's layout, `GroupLayout::minimized` /
`maximized`, and `session_focus` brings a minimized pane back and lets a different maximized one
give way),
`inspector_show` (tab notes/runs/git/session | hidden), `inbox_panel_set` (open, height).

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
| `run_edit` ✅ | session_id, runs [{id?, label, command_line, is_default?, mode? process/powershell, post_run_url?}] \| detect: true | state | replace the list (≤ 50; one default, the last marked; one-line command lines; URL http/https only), or take the folder's project template. **Refused for MCP clients**: it decides what F5 runs |

Rules carried from the WPF app: a run never types into its session; runs use the **inbox**
ConPTY (`allow_bundled = false`: a headless PTY has nobody to answer OpenConsole's DA1); Process
mode is `cmd.exe /c "<command>"` passed to CreateProcess verbatim (`SpawnOptions::verbatim_args`:
cmd does not parse MSVCRT quoting), PowerShell mode `-EncodedCommand`, SSH and WSL parents go
through `build_run_ssh_args` / `build_run_wsl_args` (`-e`, never `--`). Runs die with their
session (close, sleep, restart) and with the app. `PostRunUrl` opens only after exit 0 and only
an absolute `http(s)` URL (Windows: `rundll32 url.dll,FileProtocolHandler`, argv). New local and
WSL sessions are seeded from a top-level folder scan on a worker: dotnet → cargo → node (pnpm /
yarn / bun / npm) → python → make.

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
| `worktree_list` ✅ | session_id \| folder | read, launch | every worktree: label, branch, dirty, ↑/↓, prunable, the sessions in it, `open` (`session_new` params) |
| `worktree_new` ✅ | session_id, branch, base?, path?, open_session? (true) | launch, writes files, state | `worktree add` (`-b` when new) beside the main checkout; opens a session there like this one, placed after it |
| `worktree_remove` ✅ | session_id, path?, delete_branch?, force? | launch, writes files, discard, state | no `--force`; refuses the main checkout, changes, a worktree listed inside it, any repository on disk inside it (fails closed); closes the folder's sessions (only with `force`) and waits for their exit; sleeping ones are deleted after git removed the folder; `delete_branch` uses `-d` |
| `worktree_prune` ✅ | session_id | launch, writes files | `worktree prune`: forgets worktrees whose folder is gone |

One operation per repository at a time (keyed by the canonical repo root, so worktrees of one repo
share it; a second is refused). After any operation every live session on that repo re-reads its
state. `SessionView` carries `branch`, `git_dirty`, `git_upstream`, `git_ahead`, `git_behind`,
`git_changes`, `git_summary`, `worktree_of`, `repo_root`, `git_busy`, filled by a poll (focused 10 s,
others 120 s, WSL 30 s) plus a shared `.git/HEAD` + `index` watcher; an OSC 9001 push wins.

### notes, search ✅
Built in `csm-engine` (`tools/search.rs`) over the `csm-index` crate (SQLite + FTS5, one DB thread that owns the connection: this replaces the WPF `DbGate`).

| id | params | effects | what |
|---|---|---|---|
| `note_get` ✅ | folder? \| session_id? | read content | the note for a folder (sessions in one folder share it); returns `text`, `updated_at`, `shared_by`. Refused for SSH sessions and sessions with no folder |
| `note_set` ✅ | folder? \| session_id?, text | state | replace the note (blank deletes; ≤ 256 KiB; control characters other than tab/newline dropped) |
| `notes_import_wpf` ✅ | path? | state, read content | import `project_notes` from the WPF app's `output.db` (opened read-only; newer wins) |
| `search_output` ✅ | query, limit? (≤ 500), session_id?, group_id?, advanced? | read content | FTS5 search of everything sessions printed, newest first: session, time, excerpt, matched ranges. The query is one quoted phrase unless `advanced: true` (AND / OR / NOT / prefix*); a syntax error is a plain message |
| `search_notes` ✅ | query, limit? | read content | case-insensitive literal search over notes (and folder names) |
| `search_reopen_session` ✅ | session_id | state, launch | reopen a session a search result named but that is closed, from its kept configuration (`session_history`) |

**Storage.** `index.db` in the *local* (non-roaming) app data dir, `CodeShellManager2\index.db` (moved once from the pre-v2 `CodeShellManager-rs` folder); a state file elsewhere (`--config-dir`, `CSM_CONFIG_DIR`, `CSM_STATE_PATH`, portable) gets it beside it. `--clean` and an isolated run (`CSM_CONFIG_DIR` empty) index **in memory**: nothing reaches the disk. Output is fed from the PTY reader thread as raw bytes (non-blocking; over a byte bound a chunk is dropped and counted), then ANSI-stripped (including the bundled ConPTY's `ESC[1t ESC[c`), cut into lines, batched and written on the DB thread, and trimmed to the newest 20 MiB per session. Schema is versioned with `PRAGMA user_version`; a file that cannot be opened is moved aside as `index.db.bad-<ms>`.

### app
✅ `app_info`, `settings_get/set`, `recently_closed_list`, `session_recent` (the Recent menu, Ctrl+Shift+E), `state_import_wpf`, `app_quit` (force?), `grant_list` / `grant_revoke`, `wsl_distros` (read; the new-session panel's distro list, probed off the engine thread), `open_folder`, `open_with`, `open_url` (a link Ctrl+clicked in a terminal; absolute http/https only), `open_with_list`, `open_with_set`, `open_with_detect`, `consent_list`, `consent_answer` (UI only).
`app_quit` with `force: true` skips the remaining exit waits, also while a quit already runs (the shutdown board's "Force close now"). Grants are kept in `state.json` (`McpGrants`); `grant_revoke` takes `client` and an optional `tool` (none = all).

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
/clear, /compact, /status, /memory, /init, exit; app-only key) and presses Enter separately; any
other text is refused (use `session_send_text`, which pastes). File drops onto a pane go through
`session_send_text` with `csm_core::dropped_paths` quoting (control and format characters
rejected, Windows double quotes / POSIX single quotes, ≤ 64 paths).

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
Measured on 2026-11-20 after the parity merges (96 engine tools; the app's own `pane_*` tools
are not in the engine registry, so they are not counted). Re-run it after adding tools, and keep
`core` small: it is what every client pays on every conversation.

| toolset | tools | bytes | est. tokens |
|---|---|---|---|
| base (core + toolset tools) | 26 | 20518 | ~5129 |
| + groups | 18 | 10032 | ~2508 |
| + layout | 9 | 6125 | ~1531 |
| + runs | 5 | 3069 | ~767 |
| + git | 9 | 6312 | ~1578 |
| + notes | 3 | 2142 | ~535 |
| + search | 3 | 2395 | ~598 |
| + app | 23 | 16114 | ~4028 |
| everything enabled | 96 | 66707 | ~16676 |

Progressive disclosure is worth about 3x here: a client that only needs sessions pays ~5.1k
tokens instead of ~16.7k.

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

### Engine registry (97 tools)

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
| `session_accent_reset` | core | state | no | Forget the accent colour a program pushed with OSC 9001; the session goes back to its folder's colour. |
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
| `layout_set` | layout | ui | no | Set a group's layout: mode focus, side (pinned side by side), overview, grid (preset 1, 2, 2x2, 3x2, 3x3, or cols x rows) or free. |
| `layout_pin` | layout | ui | no | Pin a session into the side-by-side layout. |
| `layout_unpin` | layout | ui | no | Remove a session from the side-by-side layout. |
| `inspector_show` | layout | ui | no | Show the inspector on a tab (notes, runs, git, session) or hide it. |
| `inbox_panel_set` | layout | ui | no | Open or close the Needs-you panel and/or set its height. |
| `window_placement_set` | layout | ui | no | Remember where a window is: the main window, a torn-off group or a torn-off pane (physical pixels, the monitor's DPI, maximized). The app sends it as windows move; it applies at the next start or tear-off. |
| `app_info` | app | read | no | Version, the ConPTY in use, where the state is kept, session counts. |
| `settings_get` | app | read | no | All settings (or one, by key). Keys as in state.json (PascalCase) or snake_case. Secrets are not returned. |
| `settings_set` | app | state | no | Set one setting by key. Settings that decide what runs (default command, launch commands, editors, API key, ConPTY) can only be changed in the app. |
| `recently_closed_list` | app | read | no | Closed sessions that can be reopened, newest first (index 0). |
| `session_recent` | core | read | no | The sessions used most recently (up to 10, live or asleep, newest first), without the focused one: the first is the session used before it. Each has its status and how long ago it was used (5m, 3h, 2d). `session_focus` goes to one (waking it). |
| `state_import_wpf` | app | state | no | Import sessions and groups from the WPF app's state.json (default: its usual location). Imported sessions arrive asleep; existing ones are kept. |
| `app_quit` | app | discard | asks | Close every session (Claude ones get time to exit cleanly), save, then signal the app to exit. Sessions restore next time. force: true skips the exit waits (also while a quit is already running). |
| `window_show` | app | ui | no | Bring the app's window to the front, restoring it if it is minimized (the tray icon's Show window). |
| `notification_test` | app | ui | no | Show a sample OS notification now, to check that notifications reach you (sent even with notifications off; the sound follows ShowNotificationSound). |
| `grant_list` | app | read | no | Clients allowed to call a tool without asking ("Always for this client" on a consent card). |
| `grant_revoke` | app | state | no | Make a client ask again before calling a tool (or, without `tool`, before calling any tool). |
| `toolset_enable` | core | read | no | Offer a toolset's tools to this MCP client (core is always on). |
| `toolset_disable` | core | read | no | Stop offering a toolset's tools to this MCP client. |
| `git_status` | git | launch | asks | A session folder's git state, read now: branch, upstream, ahead/behind, the changed files, and which repo it is a worktree of. |
| `git_fetch` | git | launch, network | asks | git fetch in the session's repository (never prompts for a password; sign in from a terminal once if it fails). |
| `git_pull` | git | launch, files, network | asks | git pull --ff-only --no-rebase: moves the branch only when that loses nothing; a diverged branch or a conflicting local edit is refused with git's reason. |
| `git_switch_branch` | git | launch, files | asks | git switch --no-guess to a local branch. A branch checked out in another worktree is refused (open that worktree instead; `git_branches` says which). |
| `git_branches` | git | launch | asks | Local branches; for each one checked out in another worktree, that worktree and its session. |
| `worktree_list` | git | launch | asks | Every worktree of the session's repository (or, with `folder`, of the repository a folder without a session is in): branch, changes, ahead/behind, the sessions in it, and the `session_new` params to open one there. |
| `worktree_new` | git | state, launch, files | asks | git worktree add for a branch (created from `base` or HEAD when it doesn't exist), next to the main checkout by default; opens a session there with the same command, placed after this one, unless open_session is false. |
| `worktree_remove` | git | state, launch, discard, files | asks | Remove a linked worktree with git worktree remove (no --force). Refuses the main checkout, a worktree with changes, and one with another repository inside it. Closes the sessions in that folder first (needs force: true when there are any) and waits for them to exit; delete_branch uses branch -d, which keeps an unmerged branch. |
| `worktree_prune` | git | launch, files | asks | git worktree prune: forgets worktrees whose folder is already gone. Only git's bookkeeping; never a working tree. |
| `run_list` | runs | read | no | A session's run commands and the state of each one's last run (running, ok, failed, stopped). |
| `run_start` | runs | launch | asks | Start a run command (default: the session's default) in its own background terminal; never types into the session. Restarts it if it is already running. |
| `run_stop` | runs | discard | asks | Stop a running run command (default: the default one) and everything it started. |
| `run_output` | runs | content | asks | The ANSI-stripped output of a run command's last run (kept up to 1 MB; `max_chars` returns the tail). |
| `run_edit` | runs | state | no | Replace a session's run commands (label, command line, mode process/powershell, post-run URL, default), or `detect: true` to take the project-type template. App and CLI only. |
| `open_folder` | app | launch | asks | Open the session's folder in the file manager (existing folders only; local and WSL sessions). |
| `open_with` | app | launch | asks | Open the session's folder in an editor from the Open with list (default: the default entry). `entry` is an entry id or label. |
| `open_url` | app | launch | asks | Open an absolute http or https URL in the default browser (a link clicked in a terminal). Other schemes, relative URLs and URLs with spaces or quotes are refused; no shell is involved. |
| `open_with_list` | app | read | no | The Open with list; with `session_id`, also the exact command each entry would run for that session. |
| `open_with_set` | app | state | no | Replace the Open with list (label, program, argument templates per session kind, default). Checked and cleaned; app and CLI only. |
| `open_with_detect` | app | state, launch | asks | Look for installed editors and IDEs and add the ones not listed yet. |
| `wsl_distros` | app | read | no | The installed WSL distributions (name, version, state, default), default first. Empty when WSL is not installed or this is not Windows. |
| `profile_list` | app | read | no | The profiles in Windows Terminal's settings (Stable, Preview and unpackaged installs): name, command line, starting folder and look, ready for `session_look_set` or `session_new`'s `look`. Needs the ImportWindowsTerminalProfiles setting. |
| `session_look_set` | app | state | no | Give a session its own terminal look: a Windows Terminal profile's (by name), an explicit look (colours, font size, cursor shape, padding), or none (`clear`). A running session changes at once. |
| `note_get` | notes | content | asks | The note kept for a folder (give folder, or session_id to use that session's folder). Sessions in one folder share one note; SSH sessions have none. |
| `note_set` | notes | state | no | Replace the note kept for a folder (folder or session_id). An empty text deletes the note. Refused for SSH sessions and sessions without a folder. |
| `notes_import_wpf` | notes | state, content | asks | Import the per-folder notes from the WPF app's output.db (default: its usual location). The file is opened read-only; a note already here is kept unless the imported one is newer. |
| `search_output` | search | content | asks | Full-text search of everything every session printed (ANSI stripped). The query is taken literally as a phrase unless advanced is true (FTS5 syntax: AND, OR, NOT, prefix*). Newest matches first. |
| `search_notes` | search | content | asks | Search every folder's notes for a piece of text (case-insensitive, literal). |
| `search_reopen_session` | search | state, launch | asks | Start a new session from the configuration a closed session had (session_id from a search result). Refused when that session is still in the sidebar. |
| `session_send_command` | core | input | asks | Type one of the configured Claude slash commands (settings ClaudeCommands: /help, /clear, /compact, /status, /memory, /init, exit) into a session and press Enter. Refused into the caller's own session, and while the session is working unless force is true. |
| `state_export` | app | files | asks | Write a backup of the whole setup (sessions, groups, layouts, settings) to a .csm file at `path` (absolute). The same JSON as state.json; the WPF app's Import reads it. An MCP client's export leaves out the API key and MCP grants. |
| `state_import` | app | state, discard | asks | Read a .csm backup and replace the sessions and groups with its own (ours are closed; Recently closed keeps the last ten; imported ones arrive asleep), taking its settings. dry_run: true only reports what it would do and lists the backup's Open with commands; replacing needs force: true; the backup's Open with list is adopted only with adopt_open_with: true. App and CLI only. |
| `usage_stats` | app | read | no | Per command (claude, pwsh, …): how many sessions started, their total running time in seconds, and the last day (UTC) it was used. Counted on this machine only. |

<!-- xtask:tools:end -->

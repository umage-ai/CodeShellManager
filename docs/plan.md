# CodeShellManager → Rust, cross-platform: evaluation and plan

> **Historical plan.** Accepted 2026-11-19, before the rewrite was built; kept for the reasons behind
> the decisions. Where it says "adopt", "planned" or "in progress", read the status below. For how
> the app works now, read the [README](../README.md), [tools.md](tools.md), [keyboard.md](keyboard.md),
> [quit.md](quit.md) and [remote-access.md](remote-access.md).
>
> | Item in this plan | What happened |
> |---|---|
> | Phase 0 terminal spike and the Tauri fallback | Passed on egui, so the fallback was not used. The spike is in [phase0-spike.md](phase0-spike.md). |
> | egui 0.37 move (section 7) | Not done: the app is on upstream egui 0.36 (`Cargo.lock`). |
> | Phases 1 to 5 | Built. The plan's "1.0" is the 2.0 release. |
> | Layouts (section 4) | Focus, Side by side, Overview, Grid and Free as planned, plus **Board** (agents and tasks in columns). |
> | One status vocabulary (section 4) | Built, with one more value than planned: **Starting** (launching: waiting for the first measured size, the Claude stagger or the process). **Exited** is also separate from Error. |
> | Inspector tabs | Notes, Docs, Runs, Git, Session, Usage. |
> | Crates marked "adopt" (section 1) | Only `craft-mcp-server`, `craft-ipc` (vendored, with one macOS fix) and `craft-dialogs` are dependencies. Settings, the keymap, menus, title bar, update check and widgets were written in this repository (`csm-core`, `csm-services`, `csm-ui`); `craft-ui`, `craft-settings`, `craft-keymap`, `craft-menu`, `craft-window`, `craft-quit`, `craft-update`, `craft-platform` and `craft-i18n` are not used. |
> | Dev tooling (section 7) | Built: `cargo xtask` with `layers`, `policy`, `tools`, `secrets`, `mcp-size` and `ci`; the pre-commit hook; CI with `-D warnings`; `deny.toml` and `about.toml`. `rustfmt.toml` sets `max_width = 150` (not 160) and is not applied workspace-wide. `mcp-size` replaces `mcp-probe.py`. Not done: a `disallowed-types` rule for `std::sync::Mutex`. The crate-root deny of `unwrap_used` and friends is a workspace `warn` lint, not a `deny`. |
> | Patterns to copy (section 7) | A run marker and a file log exist (`csm-engine` `diag`). The graphics start-up fallback chain (DX12, WARP, Vulkan, GL) was not built. Other items: see the code, not this plan. |
> | Crate list (section 3) | Out of date. The table in section 3 is the current list. |
>
> The WPF app's `CLAUDE.md`, which this plan calls the record of hard-won behaviour, now lives in
> this repository as [wpf/lessons.md](wpf/lessons.md).

## 1. What craft-anything is, and what it can be used for

`artcraft/craft-anything` is a reverse-engineering app that "studies" a target (website, app, game)
and has a coding agent rebuild it. It's pure Rust (edition 2024, MSRV 1.97) and dual-licensed
MIT/Apache-2.0. It ships as an egui desktop app, a CLI and an MCP server, and its CI builds for
Windows, macOS and Linux.

There are two ways it could help. Only the second one is worth doing.

### a) As a tool ("Recreate this app in Rust"): no

- Its check step compares the rebuild with the original (screenshots, runs), so the result would
  copy today's UI, which is the opposite of the redesign we want.
- What CodeShellManager is really worth is its hard-won invariants (the WPF app's CLAUDE.md, now [wpf/lessons.md](wpf/lessons.md): the ConPTY
  DA1/OSC behaviour, fit/size reporting, git off the UI thread, argv-only command lines, the
  restart and shutdown races). An automated rebuild would not carry them over.
- The project describes itself as early alpha and is aimed at websites and games, not 21k lines of
  orchestration.

### b) As a donor stack and a set of reusable libraries: yes

| Need in CSM | Available in the Craft ecosystem | Verdict |
|---|---|---|
| Cross-platform GUI, GPU-rendered | eframe/egui 0.36 + wgpu, running on Win/macOS/Linux | adopt |
| Design system (tokens, scale, control sizes, contrast tests) | `craft-anything/crates/ui-egui/src/theme.rs`, `widgets.rs`, `icons.rs`, `docs/design-system.md` | copy the approach; license allows it |
| Icon buttons, tabs, split geometry, resize handles, docking | `craft-libs/craft-ui` | adopt |
| Settings: load-never-fails, atomic save, backup, migrations | `craft-libs/craft-settings` + `craft-atomic` | adopt (replaces StateService/#88 work) |
| Keybindings with user overrides and conflict detection | `craft-libs/craft-keymap` | adopt |
| Native menus on each platform, custom title bar | `craft-libs/craft-menu`, `craft-window`, `craft-quit` | adopt |
| Single instance / focus hand-off | `craft-libs/craft-ipc` | adopt (for `csm open <folder>` from a shell) |
| Child processes: deadline, output cap, kill the tree (Job Object / process group) | `craft-anything/crates/platform/src/process.rs` (process-wrap) | copy the pattern |
| PTY + screen model | `platform/capture.rs`: portable-pty 0.9 + vt100 0.16, **headless capture only** | partial, see below |
| Finding Claude Code, `claude auth status`, cost parsing | `crates/ai/src/cli_agents` | small reuse |
| i18n | `craft-libs/craft-i18n` | optional, later |

Pinning (corrected 2026-11-19, see section 7): the only craft-libs release, `v0.1.0` (egui 0.36),
contains just ui, settings, quit, atomic and print. Everything newer (mcp-server, ipc, control,
keymap, menu, window, update's egui UI) is on `main`, which is on **egui 0.37 with patched egui
forks**. One commit pin for every craft-libs dependency, and egui-free crates only (decided in
section 7: upstream egui, no forks).

### What the Craft ecosystem does not have: an interactive terminal widget

This is the biggest risk in the rewrite, and the decision rests on it.

- craft-anything only screen-scrapes headless programs (`vt100`). No Craft app embeds an
  interactive terminal. `vt100` can't do that job: it doesn't handle mouse reporting, synchronized
  output, or the selection and scrollback a host needs.
- **portable-pty loads `conpty.dll` by bare name** (`portable-pty-0.9.0/src/win/psuedocon.rs:55`),
  which is the DLL search-order hijack that CLAUDE.md "Bundled ConPTY" forbids. It also panics
  (`expect`) on a system without ConPTY. On Windows we need our own ConPTY binding: port
  `ConPtyApi.cs` (load from the full path, fall back to inbox, apply the 3-failure policy). On Unix,
  portable-pty (openpty) is fine.

The candidate terminal stack is **`alacritty_terminal`** (Apache-2.0, the emulator inside Alacritty
and Zed: VT state, scrollback, selection, DEC 2026 synchronized output, true colour) with **our
own egui renderer**. Custom OSC (9001 shell integration, 7501 program status, 133 prompt marks,
7 cwd) is handled by running a `vte` parser in front of the emulator and reading those sequences
before the bytes reach alacritty_terminal. Because the emulator runs in-process, replies such as
DA1 and the 7501 probe are written in order with no web round trip. That removes most of the
ordering work in CLAUDE.md ("the reply is written synchronously on the UI thread…").

## 2. Feasibility verdict

**Feasible, and it fits the Craft stack well, provided a terminal-widget spike passes first.**

What disappears entirely with a native renderer:
- WebView2 airspace (no WPF over panes, no input routing through HwndHost)
- the WebView2 data folder / DPI process sharing (0x8007139F, `--clean` folders)
- the fit / size-report / options-token handshake (the size is known in-process)
- the `terminal.html` / `terminal-transparent.html` split
- dispatcher priority and output coalescing between processes

What needs a port per platform:

| Feature | Windows | macOS / Linux |
|---|---|---|
| PTY | own ConPTY binding (bundled OpenConsole + inbox fallback) | portable-pty (openpty) |
| Session wrapper so profile functions resolve | `pwsh -NoExit -Command` (PwshLocator) | `$SHELL -l -i -c` |
| WSL sessions | keep, behind `cfg(windows)` | n/a (consider a "container/devcontainer" kind later) |
| SSH sessions | system `ssh` | system `ssh` |
| Kill the process tree | Job Object | process group (`setsid` + `killpg`) |
| Notifications / tray | `tray-icon` + `notify-rust` | the same crates |
| Reveal in file manager | ShellExecute (port FolderOpener rules) | `open` / `xdg-open` (same rule: existing directories only) |
| Open with… detection | current list | add `/Applications/*.app`, `.desktop` files, JetBrains Toolbox paths |
| Terminal profile import | Windows Terminal | iTerm2 / Ghostty / Alacritty theme import (later) |
| Search index | `rusqlite` (bundled, FTS5) | same |
| Git | git CLI as argv (exec, never a shell) + `notify` watcher on HEAD/index | same; evaluate `gix` for HEAD/branch reads to cut spawns |

Size estimate: about 21k lines of C# (MainWindow alone is 6.6k) and 11.7k lines of tests.
Roughly half the services are WPF-free logic with good tests (payload parsers, order math, git
parsing, OpenWith hardening, busy rules, restart estimates). They port nearly one to one, and their
tests port as golden cases. The UI is rewritten, not ported.

## 3. Architecture

**The crate list below is the plan as first written. The table after it is the current one**, and `cargo xtask layers` (`xtask/src/layers.rs`) enforces it. Change both together.

Follow craft-anything's "headless first" layering. Nothing in the UI crate owns processes or
persistence.

```
crates/
  csm-core      L0  models (Session, Group, RunCommand), validation, OSC 9001/7501 payloads,
                    order math, busy rules, recent-sessions wording. Pure, no I/O, unit-tested.
  csm-pty       L1  Pty trait; ConPTY (own, full-path load) on Windows; openpty on Unix;
                    EarlyOutputBuffer; process-tree kill
  csm-term      L1  alacritty_terminal wrapper + OSC tap (vte) + reply writer; no egui
  csm-services  L2  git (argv, watcher, worktrees), search/FTS5, notes, claude (resume lookup,
                    launch stagger, shutdown budget), wsl (cfg windows), ssh args, open-with,
                    settings (craft-settings), import of the old state.json
  csm-engine    L3  session supervisor: launch / sleep / wake / restart / close state machine,
                    restore loop, attention inbox, and the tool registry: every action is a
                    ToolSpec (docs/tools.md), with consent for MCP callers derived from effects
  csm-mcp       L4  craft-mcp-server `Tools` over the registry; progressive toolsets
  csm-ui        L4  egui: the terminal renderer widget, sidebar, panes, palette, inspector
apps/
  codeshell     desktop app
  csm-cli       `csm mcp` (stdio MCP server) and every tool as `csm <tool-id>`; both talk to the
                running app over craft-ipc and start it if needed
```

**Current crates and layers.** A crate may depend only on crates of a lower layer; only `csm-ui` and the apps use egui.

| Layer | Crates | Role |
|---|---|---|
| L0 | `csm-core`, `csm-remote-proto` (vendored), `csm-peer-proto` | Pure logic and wire types: models, validation, payloads; the phone protocol; the peer protocol |
| L1 | `csm-pty`, `csm-term` | PTY (ConPTY on Windows) and terminal model |
| L2 | `csm-services`, `csm-index` | Git, ssh, WSL, open-with, update check; the FTS5 output index and notes |
| L3 | `csm-engine` | Session supervisor, tool registry, consent, peer host |
| L4 | `csm-mcp`, `csm-ui`, `csm-peer` | MCP and IPC bridge; the egui UI; the peer link (Noise over TCP) |
| L5 | `csm-setup`, `csm-remote` | `csm setup` and `doctor`; the remote window's client |
| L6 | `codeshell`, `csm-cli`, `csm-session-summary`, `csm-review`, `term-spike` | Programs |

Rules carried over from both projects:
- Nothing slow runs on the UI thread. egui's frame loop makes this explicit: work goes to
  background threads, and the UI polls channels.
- No interpolated command lines, anywhere (the GitService/WSL/SSH rules, plus the `PasteToTerminal`
  vs `SendToTerminal` distinction, which becomes a type: `AppAuthored` vs `Untrusted` input).
- Never crash: no `unwrap` on input-derived values; state, imports and OSC payloads are hostile
  input.
- Every user action is a registered command with an id, so the palette, keymap, menus and CLI
  share one list.

## 4. UX redesign: what to change and why

Pain points, taken from the current UI and from CLAUDE.md's own history:

1. **Too many controls on each pane toolbar.** 📝 >_ 📁 { }▼ ▶▼ ⎇chip ⚙ ↻ 💤 ✕, plus status dots,
   all emoji, all the same visual weight. Nothing tells you what matters.
2. **Status is shown three ways:** heuristic alert dots, OSC 7501 states, and the
   "needs attention" pill, spread across the sidebar row, the pane toolbar and a global counter.
3. **Three drawers stack under the toolbar** (notes, run output, git panel), each with its own
   open/close logic, and they shrink the terminal.
4. **Layout presets are cryptic** (`4 6 6×2 6×3` glyphs). Users choose a geometry when what they
   want is "these sessions, side by side".
5. **Sidebar rows are tall**, with four hover icons on each, and worktree clusters as a separate
   concept. (The three group views themselves work well and stay; see section 4.)
6. **New Session is a modal form** with Local / SSH / WSL radios, even though most sessions are
   "this folder, claude".
7. **Discoverability.** Many actions live only in right-click menus (Restart all, Clear status,
   Reset accent, Open with), and shortcuts are scattered.

Proposed design (dark Catppuccin palette kept as theme tokens, light theme added, with contrast
tested as in craft's `theme.rs`):

- **One status vocabulary.** Every session is in exactly one of *Working · Needs you (approval) ·
  Needs you (question) · Done, unseen · Idle · Asleep · Error*. One coloured glyph and one word,
  used identically in the sidebar, the pane header, the palette and the tray. OSC 7501 drives it
  when present; the heuristic fills in when it isn't. That is today's fallback rule, given one
  face instead of three.
- **"Needs you" inbox.** A strip at the top of the sidebar listing the sessions waiting on the
  user, oldest first. One shortcut (e.g. `Ctrl+J`) jumps to the next one. Running many Claude
  sessions comes down to answering them in turn, and today that is spread over pink badges.
  The inbox can be closed (a count badge next to "Sessions" reopens it; `Ctrl+J` keeps working),
  and the divider between it and the session list drags vertically (double-click resets). Both
  the open state and the height persist.
- **Groups** are collapsible sidebar sections. A collapsed group still shows the status glyph of
  any of its sessions that need you. Right-clicking a header gives: new session in the group,
  rename, layout (per group), move to its own window / dock back, collapse, restart all, sleep
  all, delete group (sessions move to Ungrouped), close all.
- **Slim pane header.** Accent stripe, name, branch chip, status, and a single `⋯` menu. Run
  (▶) appears only when the session has run commands. Everything else (folder, editor, notes,
  git, restart, sleep, edit) moves to the `⋯` menu, the palette and the inspector.
- **Inspector, one right-hand panel** with tabs *Notes · Runs · Git · Session*, replacing the
  three drawers. It docks beside the active pane instead of stacking above it (craft-ui docking),
  and it is collapsible. (Built since: *Notes · Docs · Runs · Git · Session · Usage*.)
  - *Docs* (added 2026-11-21) shows the project docs of the active session's folder, read-only
    and rendered as Markdown: `README.md`, `CLAUDE.md`, `AGENTS.md`, `CLAUDE.local.md`,
    `GEMINI.md` and `.claude/CLAUDE.md` (any case), from the folder and from the top of its git
    repository when that is another folder. A picker chooses the file (CLAUDE.md first, then
    AGENTS.md, then README.md; the choice is kept per folder for the run); links to another
    listed doc switch to it, `http(s)` links open through `open_url`. The header has the path,
    size and age, *Open in editor* (`open_with`, the folder) and *Copy path*. The tab polls
    `project_docs_list` every 2 s and re-reads the shown doc when its size or time changes;
    files over 1 MiB are not shown. Screens: `docs/screens/20-inspector-docs.png` (and `-latte`).
- **Layouts (decided 2026-11-19):** a layout belongs to a **group**, not to the whole app; each
  group remembers its own. Five modes, with the intent modes as the default:
  - *Focus*: one pane; the sidebar is the switcher. Intent mode.
  - *Side by side*: the sessions you pin, auto-tiled. Intent mode, and the default.
  - *Overview*: every live session as an auto-fitting grid of live mini-terminals; click one to
    focus it. Intent mode.
  - *Grid*: the fixed presets users already rely on (1, 2, 2×2, 3×2, 3×3, custom N×M; built as 8 presets, up to 8×8). **Kept.** (A sixth mode, *Board*, was added later.)
  - *Free*: free-hand tiling. Panes form a split tree you resize by dragging any divider, split by
    dropping a pane on another pane's edge, and every ratio persists. Panes can also **float**:
    either inside the main window (an egui window over the canvas, which a native renderer allows
    and WebView2 never did) or **torn off into their own OS window** (eframe multi-viewport).
  - **A whole group can be torn off into its own OS window**, e.g. a "client X" group on the second
    monitor with its own layout. The sidebar shows which window each group lives in, and closing
    that window docks the group back. Window bounds persist per group, using the DPI- and
    monitor-aware rules from today's `WindowPlacementMath`.
  - Caveat: on Wayland an app cannot position its own windows, so torn-off windows open where the
    compositor puts them. Sizes still persist.
- **Command palette (`Ctrl/Cmd+K`)** over every registered command and every session, live or
  asleep ("restart all in group X", "open Odulodo in VS Code", "sleep idle sessions").
- **New session becomes a quick-open.** Type a folder (recent folders, git repos under configured
  roots, fuzzy), press Enter, and claude starts there. `Tab` cycles the command (claude / shell /
  preset). Connection kinds (SSH, WSL) stay as a secondary "Remote…" choice. Editing a session
  uses the same panel.
- **Sidebar:** compact single-line rows (accent · name · branch · status glyph); a second line only
  on hover or for the selected row. Worktrees are shown nested under their repo automatically
  rather than as a separate cluster concept.
- **Three group views, kept (decided 2026-11-19)**, switched from a small control next to
  "Sessions" and in Settings:
  - *Rail* (the default, as today's FilterStrip): a 48px strip left of the sidebar with an "All"
    tab and one tab per group. A tab shows initials, two letters for a one-word name so "Odulodo"
    and "Ops" don't both read "O". Each tab carries its group's Needs-you count, or a dot for
    done/error. One group at a time: the sidebar *and the panes* show only that group
    (today's `FilterGridByActiveGroup`), and the layout is that group's own. Jumping to a session
    in another group (inbox, Ctrl+J, palette, Recent) switches the tab. Right-click a tab for the
    group menu; drop a session on a tab to move it.
  - *Headers*: every group in one list, collapsible (as above).
  - *Flat*: no group UI.
- **Lifecycle words:** keep Sleep/Wake, show a short tooltip that explains what "asleep" keeps, and
  put Restart in the `⋯` menu with the bulk versions in the palette.
- **Waiting states:** keep the restore rail and the shutdown board. They become easier now that
  nothing composites over the UI.

Before any code, mock these in the Design artifact type or Figma and do a short pass with two or
three daily users.

## 5. Phased plan

Each phase ends in something runnable. Gates are explicit go/no-go points.

**Phase 0: terminal spike (2–3 weeks). GATE.**
Build an egui widget on alacritty_terminal + a PTY (ConPTY full-path on Windows, openpty on macOS).
Run the real `claude` and pass all of these:
- correct size on the first frame
- true colour, box drawing, wide chars/emoji, IME (CJK input on Windows and macOS), selection +
  copy, bracketed paste, mouse reporting, links
- OSC 7501 probe answered → claude's debug log says `programStatus=yes`
- 20 panes streaming at once at ≥60 fps, with idle panes costing nothing (only dirty panes
  re-tessellate; use a glyph atlas)
- a font with ligatures
- a live terminal in a floating egui window inside the main window, and in a torn-off second OS
  window (multi-viewport), with input, resize and focus correct in both

If ligatures, IME or performance fail in egui, the fallback decision is **Tauri 2 + xterm.js in a
single webview**. That reuses `terminal-init.js` and drops the multi-WebView2/airspace problems, at
the cost of WebKitGTK performance on Linux. A spike that is a few days late is fine; a failed gate
is not to be argued around.

**Phase 1: core and services (3–4 weeks).** `csm-core`, `csm-pty`, `csm-services`; port the
WPF-free services together with their tests as golden cases (OSC payloads, git porcelain v2 parser
+ argv injection tests through `CommandLineToArgvW` on Windows, OpenWith hardening, order math, busy
rules, restart estimate). Add an importer for the existing `state.json` (sessions, groups, run
commands, open-with, recently closed), with `output.db` kept as is (same schema).

**Phase 2: engine (3 weeks).** Session state machine (launch/sleep/wake/restart/close and their
failure fallbacks to dormant), restore loop with the Claude stagger, shutdown budget with
`HasExited` semantics, attention inbox, command registry, `--clean`.

**Phase 3: UI, the redesign (5–6 weeks).** Theme tokens, sidebar, panes and layouts, palette,
inspector (notes/runs/git/session), quick-open, settings, tray, recent menu. Snapshot tests with
`egui_kittest` (craft-anything does the same).

**Phase 4: platform parity and packaging (3 weeks).** WSL (Windows), Open with detection per OS,
notifications, MSI/winget/choco (keep the existing pipeline), `.dmg` (signed + notarized), AppImage
/ `.deb`, an auto-update check. CI matrix: windows-latest, macos-latest, ubuntu-latest.

**Phase 5: beta and cutover (2–3 weeks).** Run beside the WPF app (separate state folder; import
on first run), dogfood, close the parity checklist, then release as 1.0 and freeze the WPF app
for fixes only.

Total: about **4–5 months for one developer working with agents**, with Phase 0 deciding whether
the stack holds.

## 6. Decisions (2026-11-19)

1. **Phase 0 gate with the Tauri fallback: accepted.** The gate now also covers floating panes
   and torn-off OS windows (see Phase 0).
2. **New repository**, local commits only for now: `umage-ai/CodeShellManager-rs`. (Superseded 2026-11-20: the rewrite now lives on the `v2` branch of `umage-ai/CodeShellManager`.) The WPF app
   stays where it is and keeps receiving fixes until cutover.
3. **Session kinds are pluggable transports.** A `Transport` trait owns: build the launch argv,
   map paths (host ↔ session), run git there, open a folder there, and run a run-command there.
   1.0 ships **Local, SSH and WSL**, with WSL first-class on Windows since it is in daily use and
   already hardened. A **Container** transport (`docker`/`podman exec` into a running container
   or devcontainer, on every OS) follows after 1.0. It is added as one implementation, with no
   `match` arms spread across the code, which is what made WSL expensive in the C# version.
4. **`unsafe`: forbidden everywhere except one place.** The workspace sets
   `unsafe_code = "deny"`; every crate root except one adds `#![forbid(unsafe_code)]`. The
   exception is `csm-pty`'s Windows module, the ConPTY binding (full-path `LoadLibraryExW`,
   `CreatePseudoConsole`, `ResizePseudoConsole`, `ClosePseudoConsole`, process attributes). That is
   unavoidable FFI, and portable-pty cannot be used for it (bare-name DLL load). Every unsafe
   block carries a `// SAFETY:` comment, and the module is kept small enough to review in one
   sitting. Rationale: one auditable FFI surface instead of forking portable-pty, while Craft's
   safe-only rule holds everywhere else.
5. **Redesign review:** The maintainers go through the mock-ups before Phase 3. Mock-ups are
   published as a private page.

## 7. From the Craft ecosystem (survey, 2026-11-19)

A read-only survey of craft-libs, craft-anything and the suite docs. Adopted items are marked
with the phase they land in.

### Decisions (2026-11-19)
1. **CodeShellManager is a umage.ai app, not a Craft app.** It uses the relevant, best parts of
   the Craft ecosystem (crates, patterns, tooling) and isn't bound by the suite's house rules.
   The Phase 0 Tauri fallback therefore stands.
2. **Upstream egui only.** craft-libs `main` is on egui 0.37 with storytold's egui forks patched
   in; we don't take a dependency on another project's egui forks. So:
   - we depend only on **egui-free** craft-libs crates, all at one commit pin;
   - from egui-using crates (craft-ui, keymap's dialog, menu, window, update's dialogs) we
     **copy code with attribution** (MIT/Apache) into `csm-ui`;
   - we move to upstream egui 0.37 as its own step **after** the engine/MCP/UI branches are
     merged, not in the middle of the UI work. If a craft-libs egui crate later works on upstream
     egui, it can replace our copy then.

### Crates (egui-free unless noted)
| Crate | Use | Phase |
|---|---|---|
| `craft-mcp-server`, `craft-ipc` | `csm mcp` and CLI → app bridge. ipc's Windows named-pipe path has never run on Windows: our tests are its first run; `craft-control` (loopback TCP + per-launch token) is the fallback | 1–2 (in progress) |
| `craft-dialogs` | Folder/file pickers that never block the event loop (a blocking rfd dialog aborts the app on macOS) | 3 |
| `craft-update` | Release check, verified download, self-update; leaves the update to winget/choco/scoop when they own the install. Core is egui-free; its dialogs are egui 0.37 | 4 |
| `craft-platform` (`appearance`) | Follow the Linux/BSD desktop dark/light preference | 4 |

### Patterns to copy (craft-anything, MIT/Apache)
- **Graphics start-up fallback** (`apps/craft-anything/src/start_guard.rs`, `renderer.rs`): a mark
  file around the first frames to detect a driver crash on the previous start; renderer chain
  DX12 → WARP → Vulkan → GL saved per app version (DX12 only first on Windows); `WGPU_*`
  overrides; `desktop.log`. Phase 3.
- **Pitfalls** (craft-anything's `docs/pitfalls.md`; not in this repository): image paste (egui-winit drops Ctrl+V when the clipboard has
  no text, so pass the raw key through to the PTY or pasting a screenshot into Claude Code does
  nothing); per-frame repaint from spinners; `scroll_to_me` every frame; `CREATE_NO_WINDOW` for
  every child process; `.cmd` shims and the macOS Dock PATH when finding `claude` (`which.rs`).
  Passed to the engine and UI agents.
- **Settings location**: `--config-dir`, then `CSM_CONFIG_DIR` (set but empty means isolated),
  then a `portable` marker beside the exe, then the platform data dir; caches and the search
  index in local (non-roaming) app data; a version field in every persisted file. Phase 1
  (in progress).
- **Crash recovery records** (`cli_agents/unfinished.rs`): OS-file-locked records swept on the next
  start; for sessions that were mid-restart or mid-shutdown when the app died. Phase 2.
- **Deadlock-reporting mutex** (`platform/src/sync.rs`), enforced by clippy `disallowed-types`
  for `std::sync::Mutex`. Phase 1.

### Dev tooling (adopt in Phase 1)
- `cargo xtask`: `layers` (crate dependencies against the L0–L4 map), source policy via `syn`
  (`std::process::Command` only in the process module, no I/O or spawning in UI code, which
  enforces "never block the UI"), `tools --check` (generated `docs/tools.md` catalogue in sync
  with the registry), `secrets` + a pre-commit hook, `ci` running all of them.
- CI with `-D warnings`; the production crate roots `#![deny(clippy::unwrap_used,
  expect_used, panic, unimplemented, todo, unreachable)]` as the suite does (today they are only
  workspace warnings). Record the ConPTY module as the named `unsafe` exception.
- `cargo-deny` (licence allowlist, check alacritty_terminal and the font stack) and
  `cargo-about` (third-party licence file for the MSI).
- Suite `rustfmt.toml` (`max_width = 150` as built; this plan said 160; `use_small_heuristics = "Max"`).
- `artcraft/scripts/mcp-probe.py` to measure `csm mcp` tool definitions per toolset.

### Agent integration (Phase 2)
- `csm setup [--dry-run]` / `csm doctor` / `csm uninstall` after craft-anything's `crates/setup`.
  `setup` detects Claude Code (honouring `CLAUDE_CONFIG_DIR`), Codex and others, and merges
  `csm mcp` into their config without losing comments, with backup, verification and rollback.
  `doctor` gives a remedy for each failed check and self-tests the MCP server (spawn,
  initialize, tools/list). This complements the per-session `--mcp-config` opt-in.
- Suite CLI conventions: `csm commands`, `csm mcp --compact`, folder limits
  (`--automation-read-root` / `--automation-write-root`).
- Note: Claude Code's `--permission-prompt-tool` only applies to headless `-p` runs, not to our
  interactive sessions; "Needs you" stays on OSC 7501 plus the heuristic.

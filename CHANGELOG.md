# Changelog

All notable changes to CodeShellManager. Versions follow [Semantic Versioning](https://semver.org/).
Release notes for 0.x (the WPF app, up to 0.11) are on the
[Releases page](https://github.com/umage-ai/CodeShellManager/releases).

## 2.0.0 (unreleased)

CodeShellManager 2.0.0 is a ground-up rewrite of the WPF app (0.x, the last being 0.11) in Rust,
with a native, GPU-rendered terminal (egui + wgpu over Windows ConPTY) in place of WebView2 and
xterm.js, and a redesigned UI. Version numbers jump from 0.11 to 2.0 to mark the new app. It
replaces 0.x: winget and Chocolatey upgrade it like any other release.

### Upgrading from 0.x

- 2.0 keeps its data in its own folder and never writes to 0.x's (`%AppData%\CodeShellManager`).
- On first start, with no sessions yet, **Import from CodeShellManager 0.x** brings 0.x's
  sessions and groups across, asleep: nothing starts until you wake it.
- *Import notes from CodeShellManager 0.x…* (palette) brings the per-folder notes from 0.x's
  `output.db`, read-only; a newer note wins.
- Settings: export a `.csm` backup in 0.x and use *Import setup* in 2.0. It previews the change,
  keeps your API key if the backup's is blank, and adopts the backup's Open with list only if you
  tick it. A 2.0 export can be read by 0.x's Import.
- Not carried over: 0.x's search history (`output.db` output). 2.0 starts a new index.
- No WebView2 runtime or .NET is needed any more.
- **CodeShellManager 2 installs under `C:\Program Files\umage.ai\`** (`umage.ai\CodeShellManager`;
  the beta in `umage.ai\CodeShellManager Beta`), so the company name reads "umage.ai" everywhere.
  It is a clean break: the upgrade removes 0.x (and any older 2.x or beta) from
  `C:\Program Files\umage\` and leaves nothing there. **Taskbar pins to the old app need
  re-pinning**; Start menu and desktop shortcuts are re-created. The sign-in entry and the PATH
  entry follow the app, and the agent registrations `csm setup` wrote (Claude Code, Claude
  Desktop, Codex, Cursor) are repaired to the new `csm.exe` when CodeShellManager starts (comments
  kept, a `.csm-backup` made; `csm doctor` reports any still naming the old folder).
- **Check for updates**, as in 0.x: once a day the app asks GitHub whether a newer release is
  out and shows "Update available" in the title bar, which opens the release page. Nothing is
  downloaded. Settings › General › Updates turns it off and has *Check now*.

### New

- **New session offers to create a folder that does not exist.** Type a path such as
  `d:\dev\shapesbyme` on This PC and the folder list gets a row, *Create folder "..." and start
  here*, when the nearest folder above it exists (a drive is never created). Enter or a click asks
  once, then makes the folder and starts the session in it. Tools: `session_new` takes
  `create_folder` (default false; it now declares the file-system-write effect, so MCP clients
  are asked), and the new read-only `folder_probe` says what would be made. SSH and WSL are
  unchanged.
- **Shift+Enter inserts a newline** in a session's prompt (Claude Code, OpenCode, Copilot CLI)
  instead of submitting it: Esc then Enter, or `CSI 13;2 u` when the program enabled the kitty
  keyboard protocol. Settings › Terminal › *Shift+Enter inserts a newline* (`ShiftEnterNewline`, on)
  turns it off for shells that read Esc then Enter as clear-and-run.
- **Fun themes sample add-on** (`theme-fun`, Settings > Add-ons > Install samples): Emacs (dark),
  Windows 98 and Blossom (light), original themes that also change how the window draws.
- **Themes can style the window, not only colour it** (`[theme.style]`, docs/addons.md, Themes):
  `corners` (rounded, square, soft), `borders` (flat or a 3D `bevel`), title bar colours, and
  `ui_font` / `terminal_font` (the bundled fonts, or a font file the theme add-on brings: read on
  a worker, at most 4 MiB, inside the add-on's folder). Invalid values are listed as problems;
  a theme without the table looks as before. The sample themes use it: Windows 98 has bevels, a
  navy title bar and the open-licensed W95FA pixel font, Emacs a monospace UI, Blossom soft corners
  and Varela Round.

- **One status vocabulary**: every session is Working, Needs you (approval or question), Done,
  Idle, Asleep or Error, shown the same way in the sidebar, pane header, palette and tray. OSC 7501
  Program Status drives it when the program reports; screen detection fills in when it doesn't.
- **Needs you inbox** at the top of the sidebar, oldest first; `Ctrl+J` jumps to the next one. It
  can be hidden and resized.
- **Keyboard**: `F6` cycles the focus between the regions of the window and `Ctrl+Alt+Arrow` moves
  between panes. Menus take arrows, Enter, Escape and access letters. **Main menu** (`F10`, or the `⋯`
  in the title bar) lists every action by area, with its shortcut;
  an action that needs a session is shown muted until one is active.
- **Dialogs by keyboard**: in the confirm, close and one-line prompt dialogs, Tab and Shift+Tab move
  between the controls, Enter presses the focused button (or the default one), Space toggles the
  "Remember my choice" box, and Escape cancels. The default button keeps a focus ring until a
  control takes the focus.
- **Quick-open by keyboard**: Tab and Shift+Tab move through the folder field, Browse, the command row, Remote, Group and the New session controls, each with a focus ring. Left and Right change the command on its row, which Tab used to cycle; Enter starts the session from the field or the row.
- **Keyboard shortcuts you can change**: Settings › Keyboard lists every action with its keys. Click
  a key and press the new one; Clear, Reset and Reset all put them back. A key another action uses
  is refused, and the message names it. The help list (F1), the palette and the menus show the same
  keys. Tools `keymap_list`, `keymap_set` and `keymap_reset` do the same for agents. See
  [docs/keyboard.md](docs/keyboard.md).
- **Lists by keyboard**: the arrows move through the sidebar rows, group headers, the rail tabs, the
  inbox, the add-on screen tabs and the inspector tabs; Space selects a row, Shift+arrows extend the
  selection, F2 renames the focused row or group, Left and Right collapse and expand a group.
  `Ctrl+PgUp`/`Ctrl+PgDn` switch the add-on screen tab (with one open) and `Ctrl+Alt+PgUp`/`Ctrl+Alt+PgDn` the rail tab.
- **Layouts per group**: Focus, Side by side (pins), Overview (every live session as a live
  mini-terminal), Board, Grid (presets or custom up to 6×6) and Free (split tree with draggable
  dividers, floating panes, panes torn off into their own window).
- **Groups in their own window**, with their own layout; closing the window docks the group back.
- **Group views**: a rail of group tabs (`Ctrl+1…9`), collapsible headers, or flat.
- **The inspector**, one panel with Notes, Docs, Runs, Git, Session and Usage tabs, replaces the
  notes, run-output and git drawers.
- **Docs tab**: the project's `CLAUDE.md`, `AGENTS.md`, `README.md` and related files, rendered as
  Markdown and refreshed when they change.
- **Claude usage**: context-window fill per session (also as a chip in the sidebar), API-equivalent
  cost today, in total and for subagents, cache hits, compactions; a cost summary per project in
  Settings → Usage.
- **The Board**: tasks and agents in Backlog, Working, Needs you, Review, Idle and Done columns.
  Drag a task onto an agent to send it (after a confirm showing the exact prompt), accept or reopen
  it in Review. Each Claude session's card lists its subagents and workflow runs.
- **Command palette** (`Ctrl+K`) over every command and every session.
- **Files and images into a remote session.** In a remote window, dropping files onto a pane, or
  pasting an image (Ctrl+V with an image on the laptop's clipboard), uploads them to the office over
  the encrypted link and pastes their office paths into the session, quoted for its shell. A progress
  card shows each drop, with Cancel. Files go to the session's folder under the office's app data,
  never into a project; folders are refused. Limits: 64 MB a file, 32 MB an image, 256 MB and 20 files
  a drop. Uploads older than 7 days are removed when the office starts and every hour; the uploads folder holds at most 1 GB. A new office tool,
  `session_drop_paths`, does the paste for local and remote drops alike.
- **New session dialog** (`Ctrl+T`): type a folder, press Enter. A **This PC / SSH / WSL** selector
  replaces *Remote…*; up to eight recent folders, wrapping command chips, a profile and group, form
  tabs from add-ons (New agent), **Start** and **Start and keep open**, `Alt+1…9`, `Alt+R`,
  `Alt+G`.
- **Remote access (CSM to CSM)**: Settings › Remote access turns it on, pairs devices and can reach
  the office through a relay; remote windows and `csm attach`. Off until you switch it on. See
  docs/remote-access.md.
- **The `csm` command line**: every action in the app is a tool, and every tool is a `csm`
  subcommand. `csm setup`, `csm doctor` and `csm uninstall` connect `csm mcp` to Claude Code,
  Claude Desktop, Codex and Cursor.
- **MCP server** (`csm mcp`), with toolsets that clients turn on as needed, and **consent cards**:
  a tool that sends input, reads session content, starts a process, writes files or may discard
  data waits for *Allow once / Always for this client / Don't allow*. Grants are revocable in
  Settings → MCP. Optionally offered to the Claude sessions the app starts (off by default).
- **Add-ons**: declarative packs (agent launch profiles with status rules, run templates, slash
  presets, palette commands) and process add-ons (their own tools and screens), enabled only after
  you see what they add. Samples ship with the app: Codex CLI, GitHub Copilot CLI, Gemini CLI, OpenCode, Review and Session
  summary.
- **Run commands from `package.json`**: a Node project gets one run command per script; MCP
  clients can add and edit run commands (with consent); a local dev server's URL opens as soon as
  it is listening.
- **Multi-select** in the sidebar (Ctrl/Shift-click) for sleep, wake, restart, close and move;
  **sort** by name, folder, last active, branch, changes or repository.
- **Send to all sessions** of a group.
- **Light theme** (Catppuccin Latte) beside Mocha, and interface zoom.
- **Tray**: the Needs you count on the icon. Closing the window asks whether to keep running in
  the tray or quit (Remember my choice; Settings › General › When I close the window).
- **One title bar** on Windows: CSM's own, with minimize, maximize and close, in place of the OS
  title bar above it.
- **Start at sign-in**, optionally minimized or hidden in the tray (Settings → General →
  Start-up; `app_autostart_set`). Launching the app again shows the running window instead of
  opening a second one on the same sessions.
- **Remote windows reopen at start.** When the laptop starts, each office whose remote window was
  open at quit opens again (a window you closed yourself stays closed; an office no longer paired is
  skipped). Setting **Reopen remote windows at start**, Settings › Remote access, on by default.

### Kept from 0.x

Local, SSH and WSL sessions; sleep and wake; restart (single, group, all); restore with Claude
`--resume` and a launch stagger; full-text search over all output, including closed sessions;
per-folder notes; run commands with `F5` / `Shift+F5`; git chip, fetch, fast-forward pull, switch
branch and worktrees (read-mostly: no commit, push or merge); Open with… detection; Windows
Terminal profile import; OSC 9001 shell integration; close warnings for busy sessions; Recently
closed (`Ctrl+Shift+T`) and Recent sessions (`Ctrl+Shift+E`); `.csm` backup export and import;
file drop; Claude slash-command menu; `--clean`.

### Security

- **MCP clients and paired devices may change only cosmetic settings** with `settings_set` (an allow-list; the rest is changed in the app). Paired devices are refused the "decides what runs" tools like MCP clients, and may not turn remote access off or revoke every device.
- **Git cannot run repository-defined programs**: every git call the app makes disables `core.fsmonitor`, the untracked cache and the `ext::` transport, so opening a session in a copied repo cannot run its configured programs. Hooks still run for commits and other operations you start.
- **Importing a backup** no longer takes its default command, launch commands or Claude commands unless you tick the new box; the preview lists each session's SSH and run commands, and a network or relative SSH config path refuses the import. **Exporting** needs a `.csm` name, never replaces a file unless `overwrite` is set, and never writes the API key.
- **Project docs** are read only from sessions' folders for MCP clients; links and network folders are refused. The Review add-on only works on repositories CSM knows (no network paths, at most 32 per caller).
- **Add-on pack palette commands** may call only read-only and navigation tools, and a plugin screen button may open only its own add-on's screens.
- **A terminal can set the clipboard (OSC 52) only while it is the focused session**, and a toast says which session did.
- **Paired devices are trusted per device** (SEC-9): a device you pair is not trusted unless you tick "Trust this device (no prompts)"; an untrusted device needs your approval on the office screen before it types into a session or starts a process, and cannot answer those approvals itself. Existing pairings were migrated as trusted. `peer_device_trust` (the app only) and a Trusted box per device change it.
- **Remote access holds up better** (COR-19 to COR-26, COR-50, SEC-33 to SEC-39): a Wi-Fi flap or a listener restart no longer makes a remote window give up for good (it retries a silent close during the handshake five times before calling it a refusal); a reconnect starts each terminal's follower over, so half an old screen is never glued to a new one and a lost follower asks for a fresh screen instead of resuming; a connection gets its Welcome before it is sent anything else; an open pairing link is not replayed to the relay after it expired and is sent to a relay you set up after opening it; **you choose Viewer, Operator or Full when you allow a pairing** (Full by default); the office clamps the relay's announced rates so one frame always fits and sends control messages first; one stalled device no longer holds up the others (each connection has its own bounded queue and writer, and consent answers are capped); a host added without a name gets a short valid name; a device may hold up to four connections at once (the oldest beyond that is replaced) instead of a second one cutting off the first. Security: remote access that comes back at start says so ("Remote access resumed: N devices, listening on ..."), and a saved "every interface" choice is only honoured when its keyed hash (key in the keychain) shows this app wrote it, otherwise the office listens on this PC only until you confirm it in Remote access; the relay enrolment token is saved with its address and dropped if the address differs; the pre-authentication pool is separate for direct and relay connections, limited per address, evicts the oldest connection when full, expects the first frame within 3 s, and a pairing offer is no longer used up by anyone who saw its cleartext id; **each new pairing gets its own random relay routing token** (pairings made before keep the token derived from the device key until you pair again; an older laptop pairing with this office, or this laptop with an older office, falls back to that as well); the log records session-content reads and terminal subscriptions, counts repeats instead of writing them, and keeps five rotated files; keys are never written when the folder cannot be limited to your account, and the limit names your account by the SID of the process token, not by environment variables.

- **A plaintext 0.x API key is no longer kept** (SEC-25): version 2 never uses it, so it is not written to `state.json`, its `.bak`, the snapshots or an export, and an old one is removed from those files at start.
- **Claude config backups and `csm setup`** (SEC-24, SEC-40, SEC-41): the `~/.claude.json` copies are named after the folder plus a short hash of its path (no collisions between same-named profiles), kept in the local (not roaming) app data, private (0700/0600 on Unix) and pruned, torn copies included. `csm setup` copies the original file mode to its config and backups, writes through a symlinked config, makes temp files with `create_new`, keeps the newest three backups, and re-reads the file just before the rename and stops if it changed. The start-up repair of moved registrations waits while a Claude process is running.
- **The per-session MCP config folder** (SEC-26) is per-user (local app data, or `XDG_RUNTIME_DIR`), never the shared temp folder; 0700, refused if owned by someone else, files made new with 0600.
- **A beta's first-run copy** of the stable app's data leaves out remote access (`peer`), paired hosts (`peer-client`) and the Claude config backups (SEC-49).
- **Cleanup batch** (SEC-29, SEC-61 to SEC-65, COR-4, COR-9, COR-14, COR-17, COR-18, COR-46 to COR-49): a link in a remote window that points at a private network (192.168.x, 10.x, `.local`, a bare host name, link-local, IPv6 unique-local) asks before it opens, like a loopback link; `peer_status` hides the pairing check code from everyone but the app's window; a remote window will not mirror more than about 4 million terminal cells in all; add-on tool text reaching MCP clients is labelled as written by the add-on; the add-on confirm shows deeply nested parameters in full; on Windows the discovery folder `Craft` is limited to your account when it is created. A custom Shift+punctuation chord is no longer shadowed by the same chord without Shift. An agent can no longer edit a task after handing it in for review. The quit's start gaps are measured from the real previous start. The Windows environment block keeps `=C:`-style variables, a failed exit-monitor duplicate or a failed reader or writer now kills the new process instead of reporting it exited, and a failed Job Object is logged as an error.


### Fixed

- **An older pre-release can no longer silently replace a newer one** (SEC-48): the MSI records a build order (see `docs/release.md`, "Version scheme") and refuses to install an older build over it, with `ALLOWOLDER=1` as the way round it. Dev builds neither check nor record.
- **A full disk no longer makes saving fail silently** (COR-5): a red strip under the title bar says sessions are not being saved, the save is retried with backoff, and it clears (with a toast) when a save works.
- **A `state.json` that is locked or unreadable is never replaced** (COR-6): the app retries the read, then starts read-only (nothing saved, nothing moved aside) with a notice, instead of treating it as corrupt and saving an empty state over it. Likewise `tasks.json`, and the Review add-on's threads (COR-28), which also keep memory and disk in step when a save fails.
- **Quit waits for a restart's old Claude** before it checks `~/.claude.json` (COR-7), and a restart whose worker thread cannot start no longer leaves the session stuck or the restart guard held (COR-8).
- **`csm setup` exits non-zero** when a client was blocked or a named one was not written (COR-30), and an update merges into the existing entry, keeping extra arguments and `env` (COR-31).
- **Beta to final** (COR-36): the stable app's first run offers to import a beta's sessions and groups (*Import from the beta*, tool `state_import_beta`). The beta's first-run copy skips a live database's `-wal`/`-shm` files and retries a partly done copy (COR-37).
- **Switching groups in a remote window** (clicking a group tab, or the next/previous tab keys) now switches the laptop's own window. It used to do nothing there and switch the office's window instead. Focusing a session from the laptop (a group tab, or a click) no longer moves the office's window either: the office only wakes the session and marks its results seen; its own focused session and group tab stay where you left them.

### Fixed (runtime)

- **`csm` and `csm mcp` no longer pick the highest pid** (COR-2): a `--clean` or scratch instance is skipped by default, `--instance <pid|state-path>` / `CSM_INSTANCE` select one, and a second launch is handed only to an instance on the same state file.
- **A relative add-on program** (`./bin/server.exe`) runs from the add-on folder on Windows (COR-27); one that leaves the folder is refused.
- **`addon-enable --version` and `update-dismiss --version`** work as flags again (COR-29); only names that really collide are reserved.
- **Stopping a process add-on ends its whole tree** (COR-32), not just the direct child.
- **Session summary** queues a second call that arrives while it waits for the app instead of dropping it (COR-33).
- **Overlapping add-on scans** install only the latest result (COR-34).

### Changed
- **Restored sessions do not notify** (popups or screen-text alerts) until you have addressed them.
- **`csm doctor`** reports its channel, the app id it looks for and which apps are published.
- Development (dispatch) builds are signed and verified like release builds.
- **A slashed zero in the terminal**: at small sizes the dot inside the default font's `0` filled in and
  it read as an `8`. The default terminal font now draws `0` with a slash through it. A font you
- **A Sleep button in the pane header**, left of Minimize: ends the process and keeps the session in the sidebar (`session_sleep`). A header too narrow for it leaves Sleep to the Actions menu.
- **Starting a run that is already running shows it instead of restarting it.** Clicking a running run in the pane header, its dropdown, the Runs tab, the menus or the palette, or pressing `F5`, opens its output and leaves it running (the Run button reads "Running"). **Restart** is separate (`run_restart`, `Ctrl+Shift+F5`, a Restart row beside Stop) and asks first when the run has been going for more than a few seconds; the `run_start` tool no longer kills a running run.
  chose yourself (`--font`) is drawn as designed.

- **Quitting closes Claude sessions several at a time** (Settings > General > "Claude sessions closed at once when quitting", `QuitClaudeConcurrency`, default 4, 1 to 8). With about 70 Claude sessions a quit took 5 to 8 minutes; it now takes around a minute and a half. Claude rewrites `~/.claude.json` as it exits, so the quit keeps a copy of it first and restores that copy if the file is left unreadable (some individual updates to it can still be lost); set the number to 1 for the old one-at-a-time quit. The shutdown board says how many close at once and shows several rows closing together. See docs/quit.md.
- The relay server (`csm-relay`) and the Phone remote add-on moved out of this repository to the private
  `umage-ai/csm-relay` repository, until they are released. The wire protocol CSM uses for its own
  peer link is vendored in `vendor/csm-remote-proto`. CSM ships no phone add-on sample any more.
- The fixed 1–18 pane layouts become the Grid layout; the default is Side by side.
- The pane toolbar becomes a slim header with one Actions menu.
- Imported sessions arrive asleep (0.x started them).

### Platforms

Windows 10 (1903 or later) and Windows 11, x64. macOS and Linux are coming: the code builds and
its tests pass on both in CI, without packages yet.

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
- On first start, with no sessions yet, **Import from CodeShellManager (WPF)** brings 0.x's
  sessions and groups across, asleep: nothing starts until you wake it.
- *Import notes from CodeShellManager (WPF)* (palette) brings the per-folder notes from 0.x's
  `output.db`, read-only; a newer note wins.
- Settings: export a `.csm` backup in 0.x and use *Import setup* in 2.0. It previews the change,
  keeps your API key if the backup's is blank, and adopts the backup's Open with list only if you
  tick it. A 2.0 export can be read by 0.x's Import.
- Not carried over: 0.x's search history (`output.db` output). 2.0 starts a new index.
- No WebView2 runtime or .NET is needed any more.
- Taskbar pins, shortcuts and scripts that start `CodeShellManager.exe` keep working: the
  installer keeps a small `CodeShellManager.exe` beside the new `codeshell.exe` that starts it.
- **Check for updates**, as in 0.x: once a day the app asks GitHub whether a newer release is
  out and shows "Update available" in the title bar, which opens the release page. Nothing is
  downloaded. Settings › General › Updates turns it off and has *Check now*.

### New

- **One status vocabulary**: every session is Working, Needs you (approval or question), Done,
  Idle, Asleep or Error, shown the same way in the sidebar, pane header, palette and tray. OSC 7501
  Program Status drives it when the program reports; screen detection fills in when it doesn't.
- **Needs you inbox** at the top of the sidebar, oldest first; `Ctrl+J` jumps to the next one. It
  can be hidden and resized.
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
- **Quick-open for new sessions** (`Ctrl+T`): type a folder, press Enter; SSH and WSL under
  *Remote…*.
- **The `csm` command line**: every action in the app is a tool, and every tool is a `csm`
  subcommand. `csm setup`, `csm doctor` and `csm uninstall` connect `csm mcp` to Claude Code,
  Claude Desktop, Codex and Cursor.
- **MCP server** (`csm mcp`), with toolsets that clients turn on as needed, and **consent cards**:
  a tool that sends input, reads session content, starts a process, writes files or may discard
  data waits for *Allow once / Always for this client / Don't allow*. Grants are revocable in
  Settings → MCP. Optionally offered to the Claude sessions the app starts (off by default).
- **Add-ons**: declarative packs (agent launch profiles with status rules, run templates, slash
  presets, palette commands) and process add-ons (their own tools and screens), enabled only after
  you see what they add. Samples ship with the app: Codex CLI, Gemini CLI, Review and Session
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

### Kept from 0.x

Local, SSH and WSL sessions; sleep and wake; restart (single, group, all); restore with Claude
`--resume` and a launch stagger; full-text search over all output, including closed sessions;
per-folder notes; run commands with `F5` / `Shift+F5`; git chip, fetch, fast-forward pull, switch
branch and worktrees (read-mostly: no commit, push or merge); Open with… detection; Windows
Terminal profile import; OSC 9001 shell integration; close warnings for busy sessions; Recently
closed (`Ctrl+Shift+T`) and Recent sessions (`Ctrl+Shift+E`); `.csm` backup export and import;
file drop; Claude slash-command menu; `--clean`.

### Changed

- The fixed 1–18 pane layouts become the Grid layout; the default is Side by side.
- The pane toolbar becomes a slim header with one Actions menu.
- Imported sessions arrive asleep (0.x started them).

### Platforms

Windows 10 (1903 or later) and Windows 11, x64. macOS and Linux are coming: the code builds and
its tests pass on both in CI, without packages yet.

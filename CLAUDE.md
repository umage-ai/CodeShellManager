# CodeShellManager

A WPF .NET 10 multi-terminal host for Claude Code (and other CLI tools). Runs multiple pseudo-terminal sessions in a tabbed/grid layout with full-text search, alert detection, git status, and session persistence.

## Build & Run

```bash
# From repo root
dotnet build src/CodeShellManager/CodeShellManager.csproj
dotnet run --project src/CodeShellManager/CodeShellManager.csproj

# Or open CodeShellManager.slnx in Visual Studio / Rider
```

**Requirements:** .NET 10 SDK, Windows 10/11 (uses ConPTY + WebView2)

### Command-line flags

| Flag | Effect |
|---|---|
| `--clean` | Debug isolation mode — see below. |

**`--clean`** (parsed in `App.OnStartup`, exposed as `App.CleanStart`):
- `MainWindow.OnLoaded` skips the restore loop and clears the in-memory `SessionManager` so any new sessions in the run don't co-mingle with the persisted set.
- `MainViewModel.SaveStateAsync` short-circuits — **nothing is written to `state.json`** for the entire run. Window bounds, layout changes, settings tweaks, and any sessions created during the clean run are all discarded on exit.
- The user's prior `state.json` survives the run untouched, so this is the safe way to test from a blank slate.

## Architecture

### Key layers

```
PTY (ConPTY) → PseudoTerminal → TerminalBridge → WebView2 (xterm.js)
                                     ↓
                           OutputIndexer → SQLite FTS5
                           AlertDetector → SessionViewModel.RaiseAlert()
```

- **PseudoTerminal** (`Terminal/PseudoTerminal.cs`): Windows ConPTY wrapper, P/Invoke only
- **TerminalBridge** (`Terminal/TerminalBridge.cs`): Routes bytes between PTY and xterm.js via WebView2 messages. Surfaces accelerator keys (Ctrl-combos, F-keys, Esc) via `_webView.PreviewKeyDown` — the newer WPF WebView2 wrapper forwards accelerators through standard key events rather than a separate `CoreWebView2Controller.AcceleratorKeyPressed`. Bridge re-raises them as `AcceleratorKeyPressed` so `MainWindow.OnBridgeAcceleratorKey` can run global shortcuts even when the terminal has focus.
- **OutputIndexer** (`Terminal/OutputIndexer.cs`): Async channels → SQLite, strips ANSI
- **AlertDetector** (`Services/AlertDetector.cs`): Regex on raw PTY output, fires after 1.5s idle

### MVVM

- `MainViewModel` — sessions collection, layout mode, search state, alert count
- `SessionViewModel` — per-session state: alert, git info, waiting state, bridge/pty refs
- `MainWindow.xaml.cs` — heavy orchestration code-behind (sidebar building, layout, search handlers)

### Services

| Service | Purpose |
|---|---|
| `SessionManager` | CRUD for ShellSession models |
| `StateService` | JSON persistence → `%AppData%/CodeShellManager/state.json` |
| `SearchService` | SQLite FTS5 search of all terminal output |
| `ColorService` | FNV-1a hash of folder path → 12-color palette |
| `GitService` | Async `git branch --show-current` + `git status --porcelain` |
| `AlertDetector` | Pattern matching for Claude prompts/approvals |

## Project Structure

```
src/CodeShellManager/
├── App.xaml / App.xaml.cs          # App startup, tray icon, crash log
├── MainWindow.xaml / .cs           # Main UI (toolbar, sidebar, terminal grid)
├── Models/
│   ├── AppState.cs                 # AppSettings + AppState (JSON root)
│   ├── ShellSession.cs             # Session data model (SSH fields, BuildSshArgs, IsDormant)
│   ├── SessionGroup.cs             # Group model
│   └── AlertEvent.cs               # Alert types: InputRequired, ToolApproval
├── Services/
│   ├── SessionManager.cs           # Session CRUD + events
│   ├── StateService.cs             # JSON persistence
│   ├── SearchService.cs            # SQLite FTS5 search
│   ├── ColorService.cs             # Folder-path → accent color
│   ├── GitService.cs               # Git branch + dirty detection
│   ├── AlertDetector.cs            # PTY output pattern matching
│   ├── CommandPresetsService.cs    # Launch presets + in-session shortcuts
│   └── ToastHelper.cs              # Tray balloon notifications
├── Terminal/
│   ├── PseudoTerminal.cs           # ConPTY P/Invoke wrapper
│   ├── TerminalBridge.cs           # WebView2 ↔ PTY bridge
│   └── OutputIndexer.cs            # Async ANSI-stripped SQLite writer
├── ViewModels/
│   ├── MainViewModel.cs            # App-level state
│   └── SessionViewModel.cs         # Per-session state + git/alert/waiting props
├── Views/
│   ├── NewSessionDialog.xaml/.cs   # New session modal
│   └── SettingsWindow.xaml/.cs     # Settings modal
└── Assets/
    ├── terminal.html               # xterm.js host page
    ├── xterm.js / xterm.css
    └── xterm-addon-fit.js

tests/
├── CodeShellManager.Tests/         # xunit unit tests (model logic, headless)
└── CodeShellManager.UITests/       # FlaUI UI tests (requires live desktop)
```

## Color / Theme

**Dark theme** (Catppuccin-inspired, hardcoded throughout):
- Background: `#1e1e2e`, Toolbar: `#181825`, Panel: `#11111b`
- Foreground: `#cdd6f4`, Muted: `#6c7086`, Border: `#313244`
- Accent blue: `#89b4fa`, Green: `#a6e3a1`, Alert pink: `#f38ba8`
- Hover: `#45475a`, Selected: `#585b70`

**Session accent colors** — `ColorService.GetHexColor(key)` uses FNV-1a hash to deterministically assign one of 12 colors. For local sessions the key is `WorkingFolder`; for SSH sessions it is `user@host`. Used as sidebar stripe + terminal toolbar top border.

**Active-terminal highlight** — every terminal pane is wrapped in an outer "active ring" Border (constant 2px thickness, transparent by default) so toggling it doesn't shift content. `UpdateActiveTerminalHighlight` (called from `UpdateSidebarActiveState`, which fires on every `MainViewModel.ActiveSession` change) paints the ring of the active session's pane in its accent color and clears all others. The ring's accent hex is stashed on `Border.Tag` at build time so the highlight method doesn't need to look up the VM.

## Session Lifecycle

1. User clicks **＋ New Session** → `NewSessionDialog` modal (Local or Remote SSH)
2. `SessionManager.CreateSession()` creates `ShellSession` model; caller copies SSH fields if remote
3. `LaunchSessionAsync()` creates: `SessionViewModel` → `WebView2` → `TerminalBridge` → `PseudoTerminal`
4. `OutputIndexer` indexes all output to SQLite; `AlertDetector` watches for prompts
5. Termination paths:
   - **Close** (`vm.CloseCommand`) → `MainViewModel.OnSessionCloseRequested` → `vm.Dispose()` + remove from `Sessions` + `SessionManager.RemoveSession()`. Session is gone from `state.json`.
   - **Sleep** (`SleepSession(vm)`) → `vm.Dispose()` + remove from `Sessions` but **keep** the `ShellSession` in `SessionManager` with `IsDormant = true`. A muted dormant sidebar entry replaces the active one.
   - **Wake** (`WakeSessionAsync(session)`) → re-runs `LaunchSessionAsync(session, restoring: true)` — same path as restore-on-startup.
6. On app close: `_vm.SaveStateAsync()` flushes `_sessionManager.Sessions` (live + dormant) to `state.json` (unless `--clean`).

## SSH Remote Sessions

Remote sessions use the system `ssh` client as the PTY command — no extra library.

- `ShellSession.IsRemote` flag distinguishes remote from local sessions
- SSH config fields on `ShellSession`: `SshUser`, `SshHost`, `SshPort` (default 22), `SshRemoteFolder`
- `ShellSession.BuildSshArgs()` (internal) produces: `-t [–p PORT] user@host "cd 'folder' && shell"`
- `LaunchSessionAsync()` branches on `IsRemote`: uses `ssh` + `BuildSshArgs()`, skips Claude auto-resume
- `PseudoTerminal.BuildCmdLine` passes `ssh` through directly (same as `cmd`/`pwsh`) — not wrapped in PowerShell
- `SessionViewModel.RefreshGitInfoAsync()` early-returns for remote sessions (no local working folder)
- SSH fields serialize to `state.json` automatically — sessions restore and relaunch on next startup

## Windows Terminal Profile Import (opt-in)

When `AppSettings.ImportWindowsTerminalProfiles` is on, the New Session dialog reads the user's Windows Terminal `settings.json` and offers each profile in a "Profile (optional)" combobox.

**Service:** `WindowsTerminalProfileService.GetProfiles()` probes Stable / Preview / Unpackaged install paths, parses each `settings.json`, flattens `profiles.defaults`, filters hidden profiles, and emits `WindowsTerminalProfile` POCOs with appearance fields already mapped to xterm equivalents.

**Per-session overrides** (all on `ShellSession`, all nullable, all persisted to `state.json`):

- `ProfileFontFamily`, `ProfileFontSize`, `ProfileFontWeight`, `ProfileFontLigatures`
- `ProfileCursorShape` (`"block" | "underline" | "bar"`), `ProfileCursorBlink`
- `ProfilePadding` (CSS shorthand)
- `ProfileBackgroundOpacity` (0.0–1.0; 1.0 = opaque)
- `ProfileRetroEffect` (CSS scanlines overlay only — not a real CRT shader)
- `ProfileColorSchemeJson` (pre-baked xterm theme)

When any override is set, `LaunchSessionAsync` calls `bridge.ApplyProfileOverrides(session)` after `ApplyFontSettings`, posting a `setOptions` message that wins over the global font.

**Transparency:** xterm.js requires `allowTransparency` in the constructor, so transparent sessions navigate to `Assets/terminal-transparent.html` instead of `terminal.html`. Both files share `Assets/terminal-init.js`. (Acrylic blur is not reachable from WebView2 — we get flat alpha over the WPF chrome instead.)

**Once stamped, profile overrides are independent.** A session keeps its appearance even if the user later edits or deletes the source profile in Windows Terminal.

## Sleep / Wake (Dormant Sessions)

Sessions can be put to sleep instead of closed — the PTY is torn down but the `ShellSession` is kept in `state.json` (`IsDormant = true`) so it can be relaunched from the sidebar later. Useful when you have many long-running projects but only need a few live at once.

**UI:**
- 💤 button appears in both the sidebar action panel (next to ✕) and the terminal toolbar.
- Dormant entries render at the bottom of the sidebar with a muted (55% opacity) appearance. Clicking anywhere on a dormant entry wakes it; the small ✕ on a dormant entry permanently deletes (with confirmation).

**Implementation (`MainWindow.xaml.cs`):**
- `SleepSession(vm)` — sets `session.IsDormant = true`, removes from `_vm.Sessions` directly (bypassing `CloseCommand` so the `ShellSession` is **not** removed from `SessionManager`), disposes the VM, and calls `AddDormantSidebarItem(session)`.
- `WakeSessionAsync(session)` — clears `IsDormant`, removes the dormant sidebar entry, then `await LaunchSessionAsync(session, restoring: true)`. On launch failure it restores the dormant entry.
- `BuildDormantSidebarItem(ShellSession)` — builds a static (no-VM) sidebar Border with muted accent stripe + 💤 icon. Click handler resolves to `WakeSessionAsync`.
- Dormant entries are tracked in `_dormantSidebarItems: Dictionary<string, Border>` so `RebuildSidebarOrder` (called after drag-reorder) can re-append them at the bottom.
- `OnLoaded` partitions saved sessions: dormant ones go through `AddDormantSidebarItem`; live ones through `LaunchSessionAsync`.
- The empty-state placeholder hides whenever `_vm.Sessions.Count > 0` **or** `_dormantSidebarItems.Count > 0`.

## Per-Session Run Commands

Each session can have a list of "run commands" — labelled command lines invoked by the toolbar ▶ button, the F5 keybinding, or the sidebar right-click submenu. Runs spawn a **separate headless `PseudoTerminal`** in the session's working folder (or a fresh `ssh` connection for SSH parents); they do **not** type into the parent PTY, so a Claude session is untouched.

**Data:** `ShellSession.RunCommands: List<RunCommandItem> { Id, Label, CommandLine, IsDefault }`. Exactly one item has `IsDefault=true`; see `RunCommandItem.EnsureSingleDefault`. Persisted to `state.json`.

**Templates:** `RunCommandTemplatesService.SeedFor(folder)` detects project type (top-level scan, first-match: dotnet → cargo → node → python → make) and returns a seed list with fresh Ids. Templates are *copied* onto new sessions at creation time; subsequent edits don't propagate back. SSH sessions skip detection (empty list).

**Runtime:** `SessionRunner` (one per `SessionViewModel`) owns a dictionary of `RunInstance` keyed by item Id. Each `RunInstance` wraps a `PseudoTerminal` started with `useJobObject: true` so the whole child tree dies when the PTY is disposed (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`). Output is captured to an ANSI-stripped string buffer (capped at 1MB). Not persisted.

**UI:**
- Toolbar `[▶][▼]` next to 💤. Hidden when `RunCommands` is empty.
- Chips strip between toolbar and terminal — one chip per active/finished run, color-coded (blue=running, green=ok, pink=failed). Click a chip to open the drawer; ✕ on a chip to dismiss it.
- Drawer (slide-down panel, like Notes) shows the selected run's output with `[⏹ Stop] [📋 Copy] [↗ Send to terminal]`.
- **Send to terminal:** for Claude parents (`ClaudeSessionService.IsClaudeCommand`), wraps in fenced preamble and writes to PTY (no trailing `\r`). For non-Claude shells, falls back to clipboard with a toast — auto-paste would risk executing pasted lines.

**Editor:** `SessionRunCommandsDialog` modal — reachable from right-click on ▶, the ▼ dropdown's "Edit commands…" entry, and the sidebar right-click "Session commands" submenu. Inline-edit rows with up/down reorder, default-radio column, +Add / 🗑 Delete, Cancel/Save.

**Keybindings:** `F5` runs the active session's default. `Shift+F5` stops it. Mirrors Visual Studio; deliberately not `Ctrl+R` (collides with shell history search).

**Lifecycle:** All runs are killed on session close, session sleep, and app exit. `SessionViewModel.Dispose()` calls `Runner.Dispose()` which iterates and disposes every instance. `SleepSession` also calls `vm.Runner.StopAll()` defensively before UI teardown.

## Alert / Waiting State

`AlertDetector` fires `AlertRaised(AlertEvent)` after 1.5s idle when it detects:
- **ToolApproval**: Claude asking to run a tool (regex on approval phrases)
- **InputRequired**: Claude's `❯` prompt or generic `y/N` prompts

`SessionViewModel.RaiseAlert(message, alertType)` sets:
- `NeedsAttention` → shows pink badge + global alert count
- `IsWaitingForInput` → green dot in sidebar + terminal toolbar
- `IsWaitingForApproval` → orange dot in sidebar + terminal toolbar

`AlertDetector.NotifyUserInteracted()` clears alert state on user input.

## Search

- All PTY output is stripped of ANSI and indexed to SQLite FTS5 by `OutputIndexer`
- `SearchService.SearchAsync(query, limit)` uses FTS5 `snippet()` for result excerpts
- Clicking a result navigates to the matching session; panel auto-closes (configurable)

## Settings (AppSettings)

Persisted in `state.json`. Key settings:
- `AutoRestoreSessions` — restore open sessions on next launch
- `AutoResumeClaude` — when restoring, append `--resume <sessionId>` to claude commands so the prior conversation is picked up. Toggle off if you want fresh sessions on restart.
- `ShowGitBranch` — show `⎇ branch` in sidebar
- `ShowTerminalStatusDot` — show status dot in terminal toolbar
- `SidebarActionIconsMode` — `OnHover` (default) / `Always` / `Hidden`. Controls the per-row `➕ 💤 ✕` button stack in the sidebar. `Hidden` collapses the panel and reclaims the horizontal space; `OnHover` keeps the panel laid out (no text shift on hover) but transparent + non-interactive until the row is hovered. Rename / Open in Explorer / Open PowerShell here remain reachable via the right-click context menu in all modes, and the terminal toolbar's `✕` is unconditional.
- `SearchCollapseAfterNavigate` — auto-close search after clicking result
- `MaxSearchResults` — FTS5 result limit (default 100)
- `DefaultWorkingFolder` / `DefaultCommand` — pre-fill new session dialog

**Layout persistence**: `AppState.LastLayout` (string, e.g. `"TwoByTwo"`) persists the active grid layout. On startup, `MainViewModel.LoadStateAsync` parses it into `Layout`, which fires `MainViewModel.PropertyChanged`; the `MainWindow` constructor subscribes and syncs `_currentLayout` + calls `RefreshTerminalLayout`, so the saved layout is what the user sees on relaunch.

## Keyboard Shortcuts

| Key | Action |
|---|---|
| `Ctrl+T` | New session |
| `Ctrl+W` | Close active session |
| `Ctrl+F` | Toggle search |
| `Ctrl+Tab` | Cycle sessions |
| `F5` | Run the active session's default run command |
| `Shift+F5` | Stop the active session's default run command |
| `Escape` (in search) | Close search panel |
| `Enter` (in search) | Execute search |

## Testing

| Project | Type | Command |
|---|---|---|
| `tests/CodeShellManager.Tests/` | Unit tests (xunit) | `dotnet test tests/CodeShellManager.Tests/` |
| `tests/CodeShellManager.UITests/` | FlaUI UI tests | `dotnet test tests/CodeShellManager.UITests/` |

Unit tests cover model logic (`ShellSession`, etc.) and run headless. UI tests require the app running on a live Windows desktop.

`ShellSession.BuildSshArgs()` is `internal` — accessible from tests via `[assembly: InternalsVisibleTo("CodeShellManager.Tests")]` in `AssemblyInfo.cs`.

## Releases

CI/CD is in `.github/workflows/build.yml`. Releases are triggered by pushing a `v*.*.*` tag:

```bash
git tag v1.2.3 -m "v1.2.3 - description"
git push origin v1.2.3
```

The tag value overrides the csproj `<Version>` at publish time (`-p:Version=` flag). **Do not rely on the csproj version number** — bump it for local build clarity only. CI produces a signed exe, MSI installer, and portable ZIP, then creates a GitHub Release automatically.

A second workflow, `.github/workflows/winget.yml`, fires on the `release: released` event and submits the signed MSI to microsoft/winget-pkgs as `UmageAI.CodeShellManager` via [vedantmgoyal9/winget-releaser](https://github.com/vedantmgoyal9/winget-releaser). It needs the repo secret `WINGET_TOKEN` (classic PAT, `public_repo` scope). The same workflow is `workflow_dispatch`-able with a `tag` input to backfill or retry a release.

A third workflow, `.github/workflows/chocolatey.yml`, also fires on `release: released` and publishes the signed MSI to community.chocolatey.org as `codeshellmanager`. It downloads the MSI from the GitHub Release, computes its SHA256, substitutes `__URL64__` / `__CHECKSUM64__` placeholders in `.chocolatey/tools/chocolateyinstall.ps1`, then runs `choco pack` and `choco push`. Needs the repo secret `CHOCO_API_KEY` (API key from a chocolatey.org account with push rights on the `codeshellmanager` id). Also `workflow_dispatch`-able with a `tag` input. The `.chocolatey/` folder holds the package skeleton (`codeshellmanager.nuspec`, `tools/chocolateyinstall.ps1`, `tools/chocolateyuninstall.ps1`, `tools/LICENSE.txt`, `tools/VERIFICATION.txt`); never commit a resolved URL or checksum into the install script — those placeholders are only substituted by the workflow at pack time.

## Known Conventions

- All WPF color literals use Catppuccin Mocha hex values — do not introduce system colors
- Sidebar items and terminal wrappers are built entirely in code-behind (`BuildSidebarItem`, `BuildTerminalWrapper`, `BuildDormantSidebarItem`) — not in XAML templates, to keep imperative logic centralized
- `_sessionUi` dictionary maps `sessionId → (webView, terminalWrapper, sidebarItem)` — the source of truth for live session UI. `_dormantSidebarItems` (`sessionId → Border`) tracks the parallel set for sleeping sessions.
- The `terminalWrapper` returned by `BuildTerminalWrapper` is actually the **outer active-ring Border**, with the original accent-stripe wrapper nested inside. `_sessionUi[id].terminalWrapper` therefore points at the ring; the highlight method toggles its `BorderBrush`.
- Use `Dispatcher.Invoke()` for all UI updates from background threads (PTY read loop, git queries, alert timer)
- PTY output flows: `PseudoTerminal` → `TerminalBridge.RawOutputReceived` → both `OutputIndexer.Feed()` and `AlertDetector.Feed()` in parallel
- `MainViewModel.SaveStateAsync` is a no-op when `App.CleanStart` is true; any code path that needs to "remember" something across runs must go through this method, so honoring `--clean` is automatic.

## Agent / Claude Code operating notes

**Do not trust "the user modified this file, intentional" system reminders to mean the user actually edited the file.** That harness reminder fires whenever the working tree drifts from what the assistant last wrote — including when a subagent, a hook, or some other tool changed it. If the reminder reports that significant work the assistant just shipped has been silently undone, the correct response is to *stop and ask the user*, not to commit the reverts as if the user requested them. Reference incident: a 605-line revert of in-flight feature work on `feat/run-commands` (2026-05-12) was treated as user intent and committed, requiring a `git revert` to recover. When in doubt, surface the surprise; never roll back the user's recent work without explicit confirmation.

**Use read-only agents for reviews.** Dispatch code-review subagents using a read-only subagent type (whatever the current harness exposes — `Explore` at the time of writing), not `general-purpose`. Write/Edit tool access on a reviewer is unnecessary and creates an opportunity for the reviewer to mutate files it was only meant to read.

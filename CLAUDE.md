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

## Architecture

### Key layers

```
PTY (ConPTY) → PseudoTerminal → TerminalBridge → WebView2 (xterm.js)
                                     ↓
                           OutputIndexer → SQLite FTS5
                           AlertDetector → SessionViewModel.RaiseAlert()
```

- **PseudoTerminal** (`Terminal/PseudoTerminal.cs`): Windows ConPTY wrapper, P/Invoke only
- **TerminalBridge** (`Terminal/TerminalBridge.cs`): Routes bytes between PTY and xterm.js via WebView2 messages
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
│   ├── ShellSession.cs             # Session data model (incl. SSH fields + BuildSshArgs)
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

## Session Lifecycle

1. User clicks **＋ New Session** → `NewSessionDialog` modal (Local or Remote SSH)
2. `SessionManager.CreateSession()` creates `ShellSession` model; caller copies SSH fields if remote
3. `LaunchSessionAsync()` creates: `SessionViewModel` → `WebView2` → `TerminalBridge` → `PseudoTerminal`
4. `OutputIndexer` indexes all output to SQLite; `AlertDetector` watches for prompts
5. On close: `Dispose()` chain cleans up PTY, bridge, indexer, detector

## SSH Remote Sessions

Remote sessions use the system `ssh` client as the PTY command — no extra library.

- `ShellSession.IsRemote` flag distinguishes remote from local sessions
- SSH config fields on `ShellSession`: `SshUser`, `SshHost`, `SshPort` (default 22), `SshRemoteFolder`
- `ShellSession.BuildSshArgs()` (internal) produces: `-t [–p PORT] user@host "cd 'folder' && shell"`
- `LaunchSessionAsync()` branches on `IsRemote`: uses `ssh` + `BuildSshArgs()`, skips Claude auto-resume
- `PseudoTerminal.BuildCmdLine` passes `ssh` through directly (same as `cmd`/`pwsh`) — not wrapped in PowerShell
- `SessionViewModel.RefreshGitInfoAsync()` early-returns for remote sessions (no local working folder)
- SSH fields serialize to `state.json` automatically — sessions restore and relaunch on next startup

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
- `ShowGitBranch` — show `⎇ branch` in sidebar
- `ShowTerminalStatusDot` — show status dot in terminal toolbar
- `SearchCollapseAfterNavigate` — auto-close search after clicking result
- `MaxSearchResults` — FTS5 result limit (default 100)
- `DefaultWorkingFolder` / `DefaultCommand` — pre-fill new session dialog

## Keyboard Shortcuts

| Key | Action |
|---|---|
| `Ctrl+T` | New session |
| `Ctrl+W` | Close active session |
| `Ctrl+F` | Toggle search |
| `Ctrl+Tab` | Cycle sessions |
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

## Known Conventions

- All WPF color literals use Catppuccin Mocha hex values — do not introduce system colors
- Sidebar items and terminal wrappers are built entirely in code-behind (`BuildSidebarItem`, `BuildTerminalWrapper`) — not in XAML templates, to keep imperative logic centralized
- `_sessionUi` dictionary maps `sessionId → (webView, terminalWrapper, sidebarItem)` — the source of truth for all session UI references
- Use `Dispatcher.Invoke()` for all UI updates from background threads (PTY read loop, git queries, alert timer)
- PTY output flows: `PseudoTerminal` → `TerminalBridge.RawOutputReceived` → both `OutputIndexer.Feed()` and `AlertDetector.Feed()` in parallel

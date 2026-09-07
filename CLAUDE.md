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

### Visual Studio: Hot Reload disabled

`Properties/launchSettings.json` ships with `"hotReloadEnabled": false`, and the csproj sets `<MetadataUpdaterSupport>false</MetadataUpdaterSupport>` in Debug. Both are workarounds for a `System.ExecutionEngineException` that crashes the app on F5 under .NET 10.0.8 + VS 18 — `Microsoft.Extensions.DotNetDeltaApplier.dll` faults during its own startup before any managed code runs. Ctrl+F5 (Start Without Debugging) is unaffected either way. **Remove both when the runtime bug is fixed upstream.**

### Command-line flags

| Flag | Effect |
|---|---|
| `--clean` | Debug isolation mode — see below. |

**`--clean`** (parsed in `App.OnStartup`, exposed as `App.CleanStart`):
- `MainWindow.OnLoaded` skips the restore loop and clears the in-memory `SessionManager` — both sessions AND groups — so any new work in the run starts from a blank slate.
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

- **PseudoTerminal** (`Terminal/PseudoTerminal.cs`): Windows ConPTY wrapper, P/Invoke only. Implements `IPseudoTerminal` (`Terminal/IPseudoTerminal.cs`) — the minimum surface needed by `RunInstance` (`DataReceived`, `Exited`, `ExitCode`, `Start`). Tests inject a fake via the `internal RunInstance(item, Func<IPseudoTerminal>)` constructor. `BuildCmdLine` passes shells through verbatim (`cmd`/`powershell`/`pwsh`/`wsl`/`bash`/`zsh`/`sh`/`ssh`/`nu`/`fish`); anything else is wrapped in `pwsh.exe -NoExit -Command …` (falls back to `powershell.exe` when pwsh isn't on PATH). The pwsh preference is deliberate — it loads the user's PowerShell 7 profile so functions defined there (e.g. `claudelocal`, `claudework`) resolve as bare session commands.
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
| `StateService` | JSON persistence → `%AppData%/CodeShellManager/state.json`. Writes are **atomic**: serialize to `.tmp`, then `File.Replace` into place, rotating the previous file to `.bak`. `LoadAsync` falls back to `.bak` when the primary won't parse, and logs every step to `crash.log` rather than silently starting empty. A static `SemaphoreSlim` serializes saves — 29 of the ~32 `SaveStateAsync` call sites are fire-and-forget, and overlapping saves would otherwise race on the shared temp file. See issue #88. |
| `SearchService` | SQLite FTS5 search of all terminal output; also owns the `project_notes` table |
| `ColorService` | FNV-1a hash of folder path → 12-color palette |
| `GitService` | Async `git branch --show-current` + `git status --porcelain` |
| `AlertDetector` | Pattern matching for Claude prompts/approvals |
| `CommandPresetsService` | Launch presets + in-session shortcuts |
| `ClaudeSessionService` | Detects `claude` invocations; finds last `--resume` session id under `~/.claude/projects/` |
| `UpdateService` | GitHub Releases version check; caches result for 24h at `%AppData%/CodeShellManager/update-cache.json` |
| `ImportExportService` | Read/write a full `AppState` to a JSON file (settings + sessions backup) |
| `SessionConfigEditor` | Diffs/applies a `SessionConfigDraft` onto a `ShellSession`; decides whether the change needs a PTY restart |
| `DbGate` | Serializes every use of the shared `output.db` `SqliteConnection`. That one connection is handed to `SearchService` *and* to every `OutputIndexer`, and it is not thread-safe — concurrent create/dispose corrupts its internal command list. Acquire as `using var _ = await DbGate.AcquireAsync();` at the top of anything touching it. See issue #102 |
| `PwshLocator` | Single answer to "pwsh or powershell?", shared by `RunInstance` (run commands) and `PseudoTerminal.BuildCmdLine` (session wrapper) so they can't disagree. Resolves via `where.exe`. An ordinary executable is accepted from metadata alone (no spawn). A **zero-byte reparse point is ambiguous, not bad**: that shape is a Store App Execution Alias, and a *working* Store install of PowerShell 7 looks identical to a stub left by an uninstalled app — so only that case is settled by actually probing execution. Rejecting the shape outright silently downgraded Store-PS7 users to 5.1 on every session launch |
| `ToastHelper` | Tray balloon notifications |
| `SessionRunner` | Per-session owner of `RunInstance` dictionary (run commands runtime) |
| `RunInstance` | One headless PTY-backed run with ANSI-stripped output buffer |
| `RunCommandTemplatesService` | Detects project type (dotnet/cargo/node/python/make) → seed run-command list |
| `WindowsTerminalProfileService` | Reads Windows Terminal `settings.json` from all install variants |
| `BuiltInTerminalSchemes` | Lookup table of WT default color schemes not present in user `settings.json` |
| `SchemeMapper` | WT scheme JSON → xterm.js theme JSON (renames `purple` → `magenta`, rewrites background as `rgba()` when opacity < 1) |
| `CursorShapeMapper` | WT `cursorShape` → xterm.js `cursorStyle` (+ optional forced blink) |
| `PaddingParser` | WT `padding` shorthand (1/2/4 comma ints) → CSS `Npx` shorthand |
| `CommandLineSplitter` | Helper — quote-aware split of a Windows commandline into `(exe, args)` |
| `ShellIntegrationPayload` | WPF-free validation for the OSC 9001 channel: hex-colour check + `#rrggbbaa`→`#aarrggbb`, dirty-flag parse, title/branch sanitising (control chars stripped, 80-char cap). See "Shell Integration (OSC 9001)" |
| `WslDiscoveryService` | `wsl -l -v` parsing (UTF-16, header skipped, `*` default marker, names with spaces, Docker-internal distros filtered), `GetDistroHomeAsync` (cached `cd ~ && pwd` per distro+user), `GetLoginShellAsync` (cached `bash`-or-`sh` probe per distro+user, defaults to `bash` on any failure), `ToUncPath` / `TryParseUncPath` — the **only** UNC↔Linux path converters; GitService and NewSessionDialog delegate here |

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

**Session accent colors** — `ColorService.GetHexColor(key)` uses FNV-1a hash to deterministically assign one of 12 colors. For local sessions the key is `WorkingFolder`; for SSH sessions `user@host`; for WSL `wsl://<distro><linux-folder>`. Used as sidebar stripe + terminal toolbar top border.

**Active-terminal highlight** — every terminal pane is wrapped in an outer "active ring" Border (constant 2px thickness, transparent by default) so toggling it doesn't shift content. `UpdateActiveTerminalHighlight` (called from `UpdateSidebarActiveState`, which fires on every `MainViewModel.ActiveSession` change) paints the ring of the active session's pane in its accent color and clears all others.

The accent comes from the **live VM**, not the `Border.Tag` stashed at build time: `RepoRoot` is populated asynchronously by `GitService` and `AccentColor` changes when it lands, so a cached Tag goes stale and stops matching the sidebar ring. The Tag survives only as a fallback. `SetBorderColor` also assigns only when the colour actually differs — it previously allocated a fresh brush and reassigned every pane on every call, which was invisible at one call per switch and a visible flicker storm when something called it rapidly.

## What makes a session "active"

`MainViewModel.ActiveSession` drives the highlight, the dispatcher priority of terminal output (`TerminalBridge.IsForeground`, issue #70), and every `ActiveSession`-scoped command (`Ctrl+W`, `F5`, the run buttons). Three things set it:

1. **A sidebar row click** → `FocusSession`, which also calls `FocusTerminal()` when `AutoFocusTerminalOnSelect` is on.
2. **Typing in a pane** → the page posts `userkey` from xterm's `onKey`, throttled to one per 500ms.
3. **Clicking in a pane** → the page posts `activate` from a capture-phase `mousedown`, throttled to 300ms.

Both (2) and (3) **must come from the page**, and this is the part that is easy to get wrong twice:

- **WebView2 is an `HwndHost`.** Mouse input landing on hosted native content raises **no** WPF routed events, tunnelling `Preview*` ones included. A `PreviewMouseLeftButtonDown` on the host Border only ever fires for the thin ring around the terminal (#108). The same fact bites on the way out too — WPF content cannot be *drawn* over a pane either, whatever `Panel.ZIndex` says. See "Session Spinners".
- **xterm's `onData` is not "the user typed".** It also carries replies the terminal generates itself — device attributes (`ESC[?1;2c`), cursor-position reports, OSC colour replies, focus in/out (`ESC[I`/`ESC[O`) — plus mouse reports when the app enables tracking. Filtering those by inspecting the bytes cannot work; a device-attribute reply is not distinguishable from typing by shape. xterm knows internally (`triggerDataEvent`'s `wasUserInput`) but does not expose it on `onData`. `onKey` is the only honest source (#106).

The page-side `mousedown` handler also calls `fitAddon.fit()`, and the initial fit is re-run on `document.fonts.ready`. xterm derives its column count from the *measured advance width* of the font, so a fit that runs before the font loads computes the wrong `cols` and tells the PTY a width that doesn't match what is drawn — text then overlaps mid-line. The `ResizeObserver` cannot catch that, because the element size never changed, only the glyph metrics (#113).

## Session Lifecycle

1. User clicks **＋ New Session** → `NewSessionDialog` modal (Local, Remote SSH, or WSL)
2. `SessionManager.CreateSession()` creates `ShellSession` model; caller sets `Kind` and copies SSH or WSL fields; for WSL it also sets `WorkingFolder` to the `\\wsl$\<distro>\<linux path>` UNC mirror (see "WSL Sessions")
3. `LaunchSessionAsync()` creates: `SessionViewModel` → `WebView2` → `TerminalBridge` → `PseudoTerminal`
4. `OutputIndexer` indexes all output to SQLite; `AlertDetector` watches for prompts
5. Termination paths:
   - **Close** (`vm.CloseCommand`) → `MainViewModel.OnSessionCloseRequested` → `vm.Dispose()` + remove from `Sessions` + `SessionManager.RemoveSession()`. Session is gone from `state.json`.
   - **Sleep** (`SleepSession(vm)`) → `vm.Dispose()` + remove from `Sessions` but **keep** the `ShellSession` in `SessionManager` with `IsDormant = true`. A muted dormant sidebar entry replaces the active one.
   - **Wake** (`WakeSessionAsync(session)`) → re-runs `LaunchSessionAsync(session, restoring: true)` — same path as restore-on-startup.
   - **Restart** (`RestartSessionAsync(vm)`) → sleep-style teardown *without* the dormant bookkeeping, then `LaunchSessionAsync(session, restoring: true, removeOnFailure: false)`. The `ShellSession` stays in `SessionManager` (so Id, group, run commands and sidebar slot survive) and never enters the recently-closed ring. Used by "Edit session…" — see below. Both non-default arguments matter: `restoring: true` makes a Claude session resume rather than start a fresh conversation (matching Wake — a restart tears down the same way, so it must recover the same way), and `removeOnFailure: false` stops a bad edit from *deleting* the session, since `LaunchSessionAsync`'s PTY-failure path calls `SessionManager.RemoveSession` — correct for a session that never started, destructive for a relaunch. If the relaunch fails either way, the session falls back to dormant so the row stays visible and fixable instead of leaving a launching placeholder that never resolves.

     A Claude restart also **waits for the old process to actually exit** (`DisposeAndWaitForExitAsync` + a config quiesce) before relaunching. Without that it recreates the concurrent-config-writer race the launch stagger and the shutdown loop both exist to prevent, and `--resume` can read a session index the outgoing process hasn't finalised. Non-Claude sessions skip the wait — they don't touch that file.

**Waiting for a PTY to exit.** Check `PseudoTerminal.HasExited`, never `IsRunning`. `IsRunning` is `_hProcess != IntPtr.Zero` and the handle is only released in `Dispose`, so it stays true for a child that exited on its own — subscribing to `Exited` for one of those waits out the full timeout for an event that already fired. `HasExited` is latched immediately before `Exited` is raised. This is not academic: combined with `ClaudeShutdownBudgetMs`, two stale panes consumed the entire shutdown budget and every remaining *live* Claude session was then force-disposed with no exit wait — the exact opposite of what the budget was for.

**`ClaudeShutdownBudgetMs` is sized from measurement (30s).** The original 15s came from the only data available at the time — idle sessions exiting in 460–770ms. Real shutdowns of *busy* sessions measure **2.3–4.7s each**, so nine of them need roughly 30s, and 15s meant force-disposing more than half the fleet on an ordinary close. Waiting is the right trade: a clean exit lets Claude finish writing its config, and `ShutdownOverlay` is already on screen telling the user why. The budget exists to bound a genuinely wedged session, not to hurry a healthy one. If you shrink it, re-measure `exit=` in `crash.log` first — the summary line alone can't distinguish "slow exits" from "waits that aren't returning".
6. On app close: `_vm.SaveStateAsync()` flushes `_sessionManager.Sessions` (live + dormant) to `state.json` (unless `--clean`).

## Editing a Session's Configuration

Any existing session can be reconfigured through the **same form used to create one** —
`NewSessionDialog` has an edit mode rather than a second, drifting dialog.

**Entry points:** the ⚙ button on the terminal toolbar, **"Edit session…"** in the sidebar
right-click menu (single-target only), and **"Edit session…"** in the right-click menu on a
dormant row (edit a sleeping session without waking it).

**Dialog** — `NewSessionDialog.ForEdit(session, launchCommands, profiles)`:
- Title becomes "Edit Session", the primary button becomes "Save".
- "Recently closed" and the sibling-worktree checkbox list are hidden (both are
  create-only concepts); the worktree probe is skipped entirely (`IsEditMode` guard).
- Local/Remote/WSL radio, folder, SSH host/port/remote folder, WSL distro (pre-selected once
  the async list loads; a since-uninstalled distro is kept as a `(not installed)` entry so
  Save cannot wipe it), WSL user and Linux folder are all pre-filled, as is command (matched
  against the launch-command list, falling back to `[custom]`), and name.
- **Appearance combobox in edit mode:** when the session already carries profile overrides,
  the list gains a `— keep current appearance —` entry (tag `KeepCurrentAppearanceTag`,
  selected by default so saving never silently resets the look) and the old `— none —`
  entry is relabelled `— clear appearance overrides —`. The panel is shown when there are
  profiles to pick **or** overrides to clear, so overrides remain removable with Windows
  Terminal import switched off. The appearance panel is shown for Local and WSL, hidden only
  for SSH.

**Result plumbing** — the dialog's `ToDraft()` returns a `Models.SessionConfigDraft` (a flat,
UI-free snapshot of every field the form owns). `Services.SessionConfigEditor` then does the
work, and being WPF-free is what makes the rules unit-testable
(`tests/CodeShellManager.Tests/SessionConfigEditorTests.cs`):

- `Diff(session, draft)` → `SessionConfigChange(AnyChange, RequiresRelaunch, WorkingFolderChanged, AppearanceChanged)`.
  `WorkingFolderChanged` also fires for a WSL distro/Linux-folder change, not just a local
  folder edit.
- `Apply(session, draft)` writes every form-owned field verbatim (blanks included, so
  clearing in the dialog really clears), including `Kind` directly; for WSL it also
  **re-derives the UNC `WorkingFolder`** from the saved distro + Linux folder rather than
  trusting whatever the dialog had cached. Runtime state — `Id`, `GroupId`, `Status`,
  `RunCommands`, `IsDormant` — is untouched.

**What needs a restart.** `RequiresRelaunch` is true for: any `Kind` change, command or
args change, working-folder change (path-normalized compare), any SSH target field change,
any WSL field change (distro, user, Linux folder), crossing the transparency boundary
(opacity `< 1.0` picks a different xterm host page at navigation time), and *clearing* an
override (`TerminalBridge.ApplyProfileOverrides` only ever **sets** options, so it can't push
a value back to the global default). SSH and WSL fields and the working folder are only
compared while the session stays in the same kind, so leftovers from a previous mode don't
read as a change.

Everything else is applied live: `SessionViewModel.NotifyConfigChanged()` re-raises the
model-mirroring properties so the sidebar row and terminal toolbar repaint in place (no
rebuild → no orphaned `PropertyChanged` subscriptions), and added/changed overrides go out
via `ApplyFontSettings` + `ApplyProfileOverrides`. A folder change also calls
`SessionViewModel.ReloadGitInfoAsync()`, which clears the once-only `RepoRoot` cache before
re-probing.

When a restart *is* needed, the user is asked. **No** keeps the live terminal and defers the
change to the next launch (it's already persisted); **Yes** runs `RestartSessionAsync`.

Editing a **dormant** session skips all of that — `EditDormantSession` rewrites the model and
rebuilds the muted sidebar row, which renders name/folder/accent statically.

Run commands are *not* part of this form; they have their own editor
(`SessionRunCommandsDialog`, see below).

## SSH Remote Sessions

Remote sessions use the system `ssh` client as the PTY command — no extra library.

- `ShellSession.Kind == SessionKind.Ssh` distinguishes remote sessions from Local/WSL ones.
  `IsRemote` still exists as a `[JsonIgnore]`d two-way convenience view over `Kind` for the
  SSH case (not the persisted discriminator — see "WSL Sessions").
- SSH config fields on `ShellSession`: `SshUser`, `SshHost`, `SshPort` (default 22), `SshRemoteFolder`
- `ShellSession.BuildSshArgs()` (internal) produces: `-t [–p PORT] user@host "cd 'folder' && shell"`
- `LaunchSessionAsync()` branches on `Kind`: SSH sessions use `ssh` + `BuildSshArgs()`, skipping Claude auto-resume
- `PseudoTerminal.BuildCmdLine` passes `ssh` through directly (same as `cmd`/`pwsh`) — not wrapped in PowerShell
- `SessionViewModel.RefreshGitInfoAsync()` early-returns for remote sessions (no local working folder)
- SSH fields serialize to `state.json` automatically — sessions restore and relaunch on next startup

## WSL Sessions

`SessionKind.Wsl` launches `wsl.exe -d <distro> [-u <user>] [--cd <linux-folder>] -e <shell> -lc "<command args>"` (PR #65, hardened on `feat/wsl-sessions-v2`; `-e` replaced a bare `--` separator during the same hardening — see below).

- **`Kind` is the only persisted discriminator.** `IsRemote` is a `[JsonIgnore]`d two-way convenience over `Kind` (`false` on an SSH session makes it Local; a WSL session is untouched). The legacy `"IsRemote"` JSON key lands in `LegacyIsRemote` and `StateService.Normalize` folds it into `Kind` — migration lives in the loader, never in a setter. A promote-only setter was tried first and silently broke "Edit session" (SSH→Local could not demote) and then every save (`FullCommandLine` was serialised and threw); see `ShellSessionMigrationTests`.
- **UNC mirror invariant.** A WSL session stores `WorkingFolder = WslDiscoveryService.ToUncPath(WslDistro, WslWorkingFolder)` (`\\wsl$\Ubuntu\home\alice\proj`). Explorer, the dormant row, run-command template seeding and `GitService` all work off that path unchanged. The Linux-side path lives on `WslWorkingFolder` and is what `--cd` receives. Every path that creates or edits a WSL session must keep both in step: `MainWindow` session creation, `InheritSessionKindFrom` (duplicate / worktree), `SessionConfigEditor.Apply`, `ReopenClosedSessionAsync`.
- **`-e`, not `--`.** `wsl.exe <cmd> -- …` runs the trailing command through the distro's *default* login shell before our own `<shell> -lc "…"` ever sees it — a second, unwanted expansion pass in the wrong environment. Verified empirically: `wsl -d Ubuntu -- bash -lc 'for t in a b; do echo "L=$t"; done'` printed `L=` / `L=` (the loop variable never survived the first pass); the same command with `-e bash` printed `L=a` / `L=b`. `-e`/`--exec` runs the given program directly, skipping that pass, and composes fine with both `--cd` and `-u`. `--` looks more natural here — resist the urge to change it back. Any user command or run command containing `$var`, `` `…` ``, globs or `~` was silently mangled before this fix.
- **Login-shell fallback (bash → sh).** `BuildWslArgs` no longer hardcodes `bash` as the login shell — minimal distros (Alpine, BusyBox images, Docker Desktop's own `docker-desktop` distro) have no bash, so hardcoding it failed *every* session and run command there, no matter what the user typed. `WslDiscoveryService.GetLoginShellAsync(distro, user)` probes `-e sh -c "command -v bash >/dev/null 2>&1 && echo bash || echo sh"` (mirrors `GetDistroHomeAsync`: cached per `(distro, user)`, 3s timeout, drains both stdout and stderr, never throws) and returns `"bash"` on any failure — that preserves prior behaviour rather than silently downgrading a distro that actually works. `MainWindow.LaunchSessionAsync` resolves it into the runtime-only `ShellSession.ResolvedWslShell` (`[JsonIgnore] internal`, never persisted — a distro's shells can change between runs) before calling `BuildWslArgs`, which uses `ResolvedWslShell ?? "bash"` for both the `-e <shell>` login shell and the empty-`Command` fallback payload. Run commands share the resolved value for free since `RunInstance` reads the same `ShellSession` instance — no second probe.
- **Docker-internal distros are filtered from the picker.** `WslDiscoveryService.Parse` drops `docker-desktop` and `docker-desktop-data` (exact, case-insensitive match — a name that merely *contains* the phrase, e.g. `my-docker-desktop-clone`, is kept) before the listing reaches `NewSessionDialog`. Those are Docker's own BusyBox-based plumbing — root-only, rebuilt on Docker updates, not a user environment — and offering them (often as the *only* entry on a dev machine) is how a maintainer here hit the bash-not-found bug during manual testing. A listing containing only Docker distros now returns empty, so the dialog falls through to its existing "No WSL distros found" hint.
- **GitService routing.** `RunGitFullAsync` detects the UNC and runs `wsl.exe -d <distro> -- git -C <linux> …`, translating `\\wsl$` args to Linux (`TranslateUncArgsToLinux`, with a distro-name boundary so `Ubuntu` never matches `Ubuntu-22.04`) and Linux paths in stdout back to UNC. This path still uses `--` deliberately, not `-e`: `git` is invoked with a literal argv, not a shell-interpreted string payload, so there is no second expansion pass to avoid. `SessionViewModel.RefreshGitInfoAsync` runs the probe on the thread pool (a `Directory.Exists` on `\\wsl$` boots a stopped distro and used to freeze the UI), polls WSL sessions every **30 s** (10 s local) and caches a "not a repo" answer for WSL so it does not spawn `wsl.exe` for it forever.
- **Quoting.** Everything that reaches `wsl.exe` goes through `ShellSession.QuoteForCmd` — MSVCRT rules (backslashes before a quote doubled, trailing backslashes doubled), verified by round-tripping through `CommandLineToArgvW` in `Win32CommandLineTests`. There is one `BuildWslArgs` (`ShellSession.BuildWslArgs(string? inner)`); `RunInstance` delegates to it and turns a build failure into a failed run chip rather than a throw.
- **Validation before UI.** `ShellSession.LaunchValidationError` (blank distro / blank SSH host) is checked at the top of `LaunchSessionAsync` before any WebView2 exists. `state.json` and imports are untrusted; a bad entry used to leak a pane.
- **Relaunch paths.** `RecentlyClosedEntry` and the `session_history.snapshot_json` column carry `Kind` + WSL fields, so Ctrl+Shift+T, the "Recently closed" list and relaunch-from-search all restore the right kind.
- **Known gaps:** WSL Claude sessions do not auto-resume on restore (`--resume` id lookup reads the Windows `~/.claude`); a distro name beginning with `-` is not defended against in `wsl.exe` option parsing.

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

## Recently Closed Sessions

Closing a session (`Ctrl+W`, sidebar `✕`, or terminal-toolbar close) pushes a snapshot onto a ring buffer (`AppState.RecentlyClosed`, cap `MainViewModel.MaxRecentlyClosed = 10`, newest first). Two ways to reopen:

- **`Ctrl+Shift+T`** — pops the newest entry and re-launches it via `MainWindow.ReopenClosedSessionAsync`. The reopened session gets a **fresh Id** so it's independent of anything that may still reference the old one.
- **"Recently closed" list at the top of the New Session dialog** — click an entry to reopen it; that entry is removed from the ring.

Sleep/wake doesn't touch the ring (`SleepSession` bypasses `OnSessionCloseRequested`). `--clean` mode clears the ring at startup (full debug isolation) and never persists changes — `SaveStateAsync` is a no-op in clean mode.

The snapshot model is `Models/RecentlyClosedEntry.cs` — a separate POCO from `ShellSession` so PTY/runtime fields (`IsDormant`, `Status`, `LastActivityAt`) don't leak into the ring buffer. `RunCommands` are deep-copied with fresh Ids on both snapshot creation and session recreation, so edits to either side never alias the other. Entries carry `Kind` and the SSH/WSL fields; legacy entries are migrated by `StateService.Normalize` like sessions.

FTS5 scrollback retention is **out of scope** for v1 — restored sessions start with an empty xterm buffer.

## Shell Integration (OSC 9001)

Programs running inside a terminal can push session state up to CSM by emitting a custom OSC sequence — useful for SSH overlays (e.g. `nexus`) where CSM cannot inspect the remote repo locally.

> **Integrator-facing reference:** [`docs/shell-integration.md`](docs/shell-integration.md) (wire format + bash/PowerShell/Python/Node/Rust/Go snippets). The notes below are CSM-internal.

**Wire format:** `ESC ] 9001 ; key=value ; key=value … ST`

ST may be `BEL` (`\x07`) or `ESC \\` — xterm.js accepts both.

**Recognised keys:**

| Key | Effect |
|---|---|
| `color` | Override the session accent (`#rrggbb` / `#rgb` / `#rrggbbaa`). Repaints sidebar stripe + active ring. 8-digit values use alpha-last (`#rrggbbaa`); CSM converts to WPF's `#aarrggbb` internally. |
| `git-branch` | Set `SessionViewModel.GitBranch` directly, bypassing `GitService`. |
| `git-dirty` | `1`/`true` → dirty-marker shown; `0`/anything else → clean. |
| `title` | Renames the session (calls `vm.Rename`). Capped at `ShellIntegrationPayload.MaxTitleLength` (80), control chars stripped; empty-after-cleanup is ignored. |

Unknown keys are ignored. Multiple keys can be sent in a single sequence. Values cannot contain `;` (the field separator, no escaping) — documented as a limitation rather than solved.

**Every value is untrusted.** It comes from whatever is printing to the terminal — a remote host, a `cat` of some file, a hook — and `color`/`title` end up in `state.json`. All validation lives in the WPF-free `Services/ShellIntegrationPayload` (`TryNormalizeColor`, `ParseDirty`, `SanitizeTitle`, `SanitizeBranch`) so it is unit-tested (`ShellIntegrationPayloadTests`); `SessionViewModel.ApplyShellIntegration` only applies what that class accepts.

**Git poller stand-down.** Once a session has received `git-branch` or `git-dirty`, `RefreshGitInfoAsync` stops touching `GitBranch`/`GitIsDirty` (`_gitOverriddenByOsc`) — otherwise the local CWD's state would clobber the pushed value every 10s. The flag is per-session-lifetime, not persisted, and `ReloadGitInfoAsync` (folder edit) resets it, since the pushed info described the old folder.

**Colour is sticky, so it is resettable.** OSC 9001 is the only writer of `ShellSession.ColorOverride` and the override survives sleep/wake and restart. The sidebar right-click menu shows **Reset accent color** (→ `vm.ClearColorOverride()`) whenever an override exists.

**AlertDetector must strip both OSC terminators.** Its ANSI regex originally matched only BEL-terminated OSC; every example in `docs/shell-integration.md` uses `ESC \`, which either leaked the payload into prompt matching or lazily swallowed real output up to the next BEL. It now mirrors `OutputIndexer.AnsiPattern` — keep the two in step (`AlertDetectorStripAnsiTests`).

**Pipeline:** `terminal-init.js` registers an OSC handler via `term.parser.registerOscHandler(9001, …)` (requires `allowProposedApi: true`, already set). It posts `{type: "shellIntegration", fields: {…}}` to WPF. `TerminalBridge` parses it and raises `ShellIntegrationReceived`. `MainWindow.LaunchSessionAsync` subscribes and calls `vm.ApplyShellIntegration(fields)` on the dispatcher, then `MainViewModel.SaveStateDebounced()` (500ms idle coalescing — a prompt hook fires on every prompt, and a `state.json` write per emission would be silly). Repainting the stripe and ring is **not** done here: the existing `AccentColor` `PropertyChanged` subscriptions in `BuildSidebarItem` / `BuildTerminalWrapper` already handle it, exactly as they do when `RepoRoot` lands.

The OSC handler returns `true` so xterm consumes the sequence and it doesn't render.

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

**Data:** `ShellSession.RunCommands: List<RunCommandItem> { Id, Label, CommandLine, IsDefault, Mode, PostRunUrl }`. Exactly one item has `IsDefault=true`; see `RunCommandItem.EnsureSingleDefault`. Persisted to `state.json`.

- **`Mode`** (`RunMode.Process` default / `RunMode.PowerShell`) — `Process` runs through `cmd /c` as before; `PowerShell` wraps the command line in `pwsh.exe -NonInteractive -NoLogo -ExecutionPolicy Bypass -EncodedCommand <utf16le-b64>` (falls back to `powershell.exe` if `pwsh` isn't on PATH). SSH and WSL parents ignore `Mode` — those runs always go through bash (`ssh … bash -c` / `wsl.exe … bash -lc`). Use PowerShell when the command relies on pipes (`|`), redirection (`>`), `$env:` variables, or cmdlets.
- **`PostRunUrl`** (`string?`, default `null`) — when set and the run exits with code 0, `Process.Start` opens the URL via `UseShellExecute=true` (default browser). No health-check polling. The value is gated by `RunInstance.IsLaunchableUrl` first: **only absolute `http`/`https` URLs are launched.** ShellExecute would otherwise run a local exe, a `.ps1`, a UNC path or any registered protocol handler, and this fires automatically with no confirmation — and `ImportExportService` deserializes a whole `AppState` (run commands included) from any JSON file the user points at, so the stored value is not trusted. Rejections and launch failures both append to `crash.log`; neither pops UI, since this runs on the PTY-exit callback thread. Scheme-less input (`localhost:5173`) is rejected rather than guessed at.

**Templates:** `RunCommandTemplatesService.SeedFor(folder)` detects project type (top-level scan, first-match: dotnet → cargo → node → python → make) and returns a seed list with fresh Ids. Templates are *copied* onto new sessions at creation time; subsequent edits don't propagate back. SSH sessions skip detection (empty list).

**Runtime:** `SessionRunner` (one per `SessionViewModel`) owns a dictionary of `RunInstance` keyed by item Id. Each `RunInstance` wraps a `PseudoTerminal` started with `useJobObject: true` so the whole child tree dies when the PTY is disposed (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`). Output is captured to an ANSI-stripped string buffer (capped at 1MB). Not persisted.

**UI:**
- Toolbar `[▶][▼]` next to ⚙ / 💤. Hidden when `RunCommands` is empty.
- Chips strip between toolbar and terminal — one chip per active/finished run, color-coded (blue=running, green=ok, pink=failed). Click a chip to open the drawer; ✕ on a chip to dismiss it.
- Drawer (slide-down panel, like Notes) shows the selected run's output with `[⏹ Stop] [📋 Copy] [↗ Send to terminal]`.
- **Send to terminal:** for Claude parents (`ClaudeSessionService.IsClaudeCommand`), wraps in fenced preamble and writes to PTY (no trailing `\r`). For non-Claude shells, falls back to clipboard with a toast — auto-paste would risk executing pasted lines.

**Editor:** `SessionRunCommandsDialog` modal — reachable from right-click on ▶, the ▼ dropdown's "Edit commands…" entry, and the sidebar right-click "Session commands" submenu. Inline-edit rows with up/down reorder, default-radio column, +Add / 🗑 Delete, Cancel/Save.

**Keybindings:** `F5` runs the active session's default. `Shift+F5` stops it. Mirrors Visual Studio; deliberately not `Ctrl+R` (collides with shell history search).

**Lifecycle:** All runs are killed on session close, session sleep, and app exit. `SessionViewModel.Dispose()` calls `Runner.Dispose()` which iterates and disposes every instance. `SleepSession` also calls `vm.Runner.StopAll()` defensively before UI teardown.

## Per-Session Notes

Each session gets a collapsible 📝 notepad panel between the terminal toolbar and the terminal. Toggled by the 📝 button on the terminal toolbar; the panel is a docked 160px-high `TextBox` (`Visibility.Collapsed` by default).

**Storage:** notes are **not** on `ShellSession` and not in `state.json`. They live in the FTS5 SQLite DB owned by `SearchService` in a separate `project_notes` table keyed by `folder_path` (the session's `WorkingFolder`). Two sessions in the same folder share one note; SSH sessions and sessions with no working folder don't get a note (`vm.WorkingFolder` is empty → save is skipped).

- `SearchService.GetNoteAsync(folderPath)` — `SELECT content FROM project_notes WHERE folder_path = ?`
- `SearchService.SaveNoteAsync(folderPath, content)` — UPSERT on `folder_path`, stamps `updated_at` (ms since epoch)

**UI lifecycle:** content is lazy-loaded on the first time the panel is opened (`notesLoaded` flag in the toolbar build). Each keystroke restarts a 1-second `System.Threading.Timer` debounce; when it fires, `SaveNoteAsync` is called on the dispatcher thread. No explicit save action — closing the panel or the session just leaves the last debounce to flush. There's no save-on-exit hook, so a note edited in the final ~1s before app close can be lost.

**Search integration:** `SearchService.SearchAsync` queries notes alongside terminal output — notes use `LIKE %query%` (short free-text, FTS5 overkill) and are tagged `SearchResultType.Note` so the search panel can label them. The note's row in the search panel is keyed by folder, not session.

**Dormant sessions:** because notes are folder-keyed and live outside `state.json`, a dormant or reopened session in the same folder transparently picks up the existing note on next wake/restore.

`AlertDetector` fires `AlertRaised(AlertEvent)` after 1.5s idle when it detects:
- **ToolApproval**: Claude asking to run a tool (regex on approval phrases)
- **InputRequired**: Claude's `❯` prompt or generic `y/N` prompts

`SessionViewModel.RaiseAlert(message, alertType)` sets:
- `NeedsAttention` → shows pink badge + global alert count
- `IsWaitingForInput` → green dot in sidebar + terminal toolbar
- `IsWaitingForApproval` → orange dot in sidebar + terminal toolbar

`AlertDetector.NotifyUserInteracted()` clears alert state on user input.

## Session Spinners

Two overlays cover launch and shutdown so the user sees progress instead of a blank pane.

**Launch overlay (per session)** lives in `Assets/terminal.html` and `Assets/terminal-transparent.html` as a CSS-animated rotating SVG arc with a phase label. Visible by default; `TerminalBridge` posts `setBootState` after `NavigationCompleted` (label = `Starting {cmd}…` for local, `Connecting to {host}…` for SSH; accent = session color) and `bootDone` on the first PTY byte (via `OnPtyData → PostBootDoneIfNeeded`, race-safe via `Interlocked.CompareExchange`). An 8-second fallback timer scheduled in `NavCompleted` also calls `PostBootDoneIfNeeded` so silent sessions and slow SSH handshakes don't lock the user out of the pane.

**You cannot draw WPF content over a terminal pane.** WebView2 is an `HwndHost`, and a
native child window is composited by the OS *on top of* everything WPF renders —
`Panel.ZIndex` does not enter into it. This is the same `HwndHost` fact recorded under
"What makes a session active", but for **output** rather than input, and it is the more
expensive half to rediscover: the code looks correct, the overlay is genuinely in the tree
with `Panel.ZIndex="100"`, and it simply does not appear.

The original centred shutdown spinner was invisible for exactly this reason. All the user
ever saw was scrim leaking through the few-pixel gaps *between* panes — reported, fairly, as
"more like 1 line, hard to see, no spinner". Anything full-window must therefore either
collapse `TerminalGrid` first (what `OnClosing` does) or live in the toolbar/sidebar chrome,
which no `HwndHost` covers.

**Restore rail (startup, app-level)** — `RestoreRail` (a 2px `ProgressBar`, `FlatBar` style)
docked under the toolbar plus a `RestorePill` counter in the toolbar's right stack, both
driven by `SetRestoreProgress(done, total)` from the `OnLoaded` restore loop and hidden
outside it. Determinate on purpose: a 25-session restore runs ~131s with per-session cost
swinging 12×, so there is no rate to extrapolate and a spinner reads identically at session
2 and session 22. Placed in the toolbar because that is above the airspace problem.

The counter advances *after* the `try`/`catch` around `LaunchSessionAsync`, so a session that
fails to restore still moves the rail — otherwise one bad session strands it short of full,
which reads as a hang.

**Shutdown board (app-level)** — `ShutdownOverlay` is now a card listing every session with a
per-row glyph (`·` pending → `◐` closing → `✓` clean / `⨯` force-disposed), elapsed time, an
overall `k / N` bar, and a budget bar running against `ClaudeShutdownBudgetMs`. Built by
`BuildShutdownBoard`, updated in place by `MarkShutdownRow` / `SetShutdownProgress` /
`SetShutdownBudget`.

Force-disposed sessions are **marked, not hidden** — that is the case a user most wants to
see, and it used to happen silently. `ShutdownHint` escalates with elapsed time to explain
*why* the wait is long; keep it explanatory rather than jokey, since it has to still read
well on the four-hundredth shutdown. The board is skipped entirely when there are no
sessions, so `--clean` runs don't get a full-window flash of "0 / 0".

Full design: `docs/superpowers/specs/2026-05-16-session-spinners-design.md`. Option
comparison behind the current design: the "Waiting States" artifact (Quiet Rail for startup,
Restore Board for shutdown — the two paths deliberately differ, because restore does not
block the user and shutdown does).

## Search

- All PTY output is stripped of ANSI and indexed to SQLite FTS5 by `OutputIndexer`
- `SearchService.SearchAsync(query, limit)` uses FTS5 `snippet()` for result excerpts
- Clicking a result navigates to the matching session; panel auto-closes (configurable)

## Settings (AppSettings)

Persisted in `state.json`. Key settings:
- `AutoRestoreSessions` — restore open sessions on next launch
- `AutoResumeClaude` — when restoring, append `--resume <sessionId>` to claude commands so the prior conversation is picked up. Toggle off if you want fresh sessions on restart.
- `ClaudeLaunchStaggerMs` (default 2000) — flat delay between consecutive Claude launches, and the cap on the post-exit settle at shutdown. The Claude CLI rewrites its config unlocked on startup and exit, so two `claude.exe` doing it at once can lose each other's updates. Evidence this is real: a machine here had two orphaned `.claude.json.tmp.<pid>.<hash>` files with the same timestamp and different pids.

  **Do not replace this with an adaptive wait again.** That was tried (#96), watched the config file settle instead of sleeping a fixed 2s, and was reverted in #111 after three attempts to make it hold its cap. Measured on a real restore it produced gates of 12574ms, 22953ms and 31378ms against a 2000ms cap. Two follow-ups helped without bounding it: #107 moved it off the UI thread, #110 removed a thread-pool thread that `PseudoTerminal` was parking per PTY.

  The reason it could never work is worth recording: the restore loop periodically stalls for seconds at a time under load, and *any* timer's continuation absorbs that stall. After the revert, a plain `Task.Delay(2000)` still logged `gate=36339ms`. The gate was never slow — it was a stopwatch measuring someone else's freeze. The gate is now gone from BOTH paths. It was kept at shutdown on the reasoning that "the machine is quiet there, so polling is reliable" — measurement falsified that: a real run logged `cfgSettle=8731ms` against a 1000ms cap, 56% of the entire shutdown budget in one session, which is what forced the rest of the fleet to be killed without a wait. Same disease, same fix: flat delay.
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
| `Ctrl+Shift+T` | Reopen the most-recently-closed session (browser convention) |
| `Ctrl+Alt+T` | Duplicate active session (was `Ctrl+Shift+T` pre-bundle) |
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

**`IPseudoTerminal` testability seam.** `PseudoTerminal` implements `IPseudoTerminal` (in `Terminal/IPseudoTerminal.cs`), and `RunInstance` / `SessionRunner` both expose an `internal` constructor that accepts a `Func<IPseudoTerminal>` factory. Production code uses the parameterless public ctors which default to `() => new PseudoTerminal()`; tests pass a hand-rolled `FakePseudoTerminal` to exercise the run-command lifecycle (Run, Stop, Dismiss, kill-and-restart, 1MB output-buffer cap) without spawning a real ConPTY child. Keep the interface surface minimal — only what `RunInstance` actually calls (`DataReceived`, `Exited`, `ExitCode`, `Start`).

**SearchService tests** open a fresh file-backed SQLite at `Path.GetTempPath()` per test for isolation. The test class is `IDisposable` and clears the connection pool (`SqliteConnection.ClearAllPools()`) before deleting the file on Windows. Seed `session_history` rows with explicit timestamps rather than `Task.Delay` — Windows' 15.6ms timer granularity makes wall-clock-based ordering flaky on CI.

## Releases

CI/CD is in `.github/workflows/build.yml`. Releases are triggered by pushing a `v*.*.*` tag:

```bash
git tag v1.2.3 -m "v1.2.3 - description"
git push origin v1.2.3
```

The tag value overrides the csproj `<Version>` at publish time (`-p:Version=` flag). `AssemblyVersion` / `FileVersion` are deliberately **not** set in the csproj so they derive from `Version` — pinning them is what made every release through v0.5.0 ship binaries reporting `FileVersion 0.3.4.0`. CI produces a signed exe, MSI installer, and portable ZIP, then creates a GitHub Release automatically.

### Pushing the tag is only half the release

**The `release: released` triggers on `winget.yml` and `chocolatey.yml` have never fired.** Every run in the repo's history is a `workflow_dispatch`. This is GitHub behaving as designed: events raised by a workflow authenticated with the default `GITHUB_TOKEN` do not trigger other workflows, and `softprops/action-gh-release` creates the Release with exactly that token. Both workflows still *declare* the trigger, which makes it look automatic. It is not.

**Post-tag checklist — required, not optional:**

```bash
# 1. wait for CI / Release to finish and the GitHub Release to exist
# 2. then dispatch BOTH mirrors by hand
gh workflow run winget.yml     -f tag=vX.Y.Z
gh workflow run chocolatey.yml -f tag=vX.Y.Z
# 3. watch both — they fail independently of CI and nothing else will tell you
```

To make it genuinely automatic, CI / Release would have to create the Release with a PAT rather than `GITHUB_TOKEN`.

### winget: the `CreateRef` error names the wrong culprit

`winget.yml` submits the signed MSI to microsoft/winget-pkgs as `UmageAI.CodeShellManager` via [winget-releaser](https://github.com/vedantmgoyal9/winget-releaser). Needs `WINGET_TOKEN` — a **classic** PAT (fine-grained tokens are unsupported) with **both** `public_repo` and `workflow`.

When it fails you will see:

```
0: AThraen does not have the correct permissions to execute `CreateRef`
1: failed to create branch UmageAI.CodeShellManager-<version>-<hash>
```

**The message points at the wrong thing.** Two distinct causes produce it, and the token itself is the *second* one, not the first:

1. **The fork is stale.** komac creates its branch in `umage-ai/winget-pkgs`; upstream lands dozens of commits a day, so a fork untouched since the last release is always too far behind for GitHub to accept a new branch.
2. **`WINGET_TOKEN` is missing the `workflow` scope**, so the automatic sync that would have fixed (1) *cannot* run — `merge-upstream` returns HTTP 422 because upstream winget-pkgs contains `.github/workflows/*.yml` and syncing means writing them. The fork stays stale and you land back at (1).

Two traps that cost real time across v0.6.0 and v0.7.0:

- **Sync the fork under the org, `umage-ai/winget-pkgs`** — komac uses the fork owned by the same account as this repo. A maintainer's *personal* fork (`AThraen/winget-pkgs`) may also exist and is a red herring; syncing it changes nothing.
- **`public_repo` alone is not enough — the token also needs `workflow`.** This was recorded backwards here through v0.6.0 ("`public_repo` is sufficient"), which is why the same failure was rediagnosed three releases running. It is still true that widening to *full* `repo` is wrong and does not help: that grants CI write access to every private repo the owner can reach. `public_repo` + `workflow`, nothing more.

`winget.yml` syncs the org fork automatically before submitting, and that step is deliberately **not** `continue-on-error` — it used to be, which is exactly how a failing sync stayed invisible and only the misleading `CreateRef` error was ever seen. If the sync fails, fix the token scope; to unblock a release in the meantime, sync by hand and re-dispatch:

```bash
gh api -X POST repos/umage-ai/winget-pkgs/merge-upstream -f branch=master
```

A third workflow, `.github/workflows/chocolatey.yml`, also fires on `release: released` and publishes the signed MSI to community.chocolatey.org as `codeshellmanager`. It downloads the MSI from the GitHub Release, computes its SHA256, substitutes `__URL64__` / `__CHECKSUM64__` placeholders in `.chocolatey/tools/chocolateyinstall.ps1`, then runs `choco pack` and `choco push`. Needs the repo secret `CHOCO_API_KEY` (API key from a chocolatey.org account with push rights on the `codeshellmanager` id). Also `workflow_dispatch`-able with a `tag` input. The `.chocolatey/` folder holds the package skeleton (`codeshellmanager.nuspec`, `tools/chocolateyinstall.ps1`, `tools/chocolateyuninstall.ps1`); never commit a resolved URL or checksum into the install script — those placeholders are only substituted by the workflow at pack time. The nuspec's `licenseUrl` points at the GitHub `LICENSE` file, so no bundled `LICENSE.txt`/`VERIFICATION.txt` is needed (Chocolatey moderator feedback on the v0.5.0 submission asked these to be removed, along with setting `owners` to the maintainer account rather than the org).

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

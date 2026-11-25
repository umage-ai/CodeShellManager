# Releasing CodeShellManager 2 on Windows, as a drop-in replacement for the WPF app

When `v2` is merged into `main`, version 2 becomes **the** CodeShellManager. People who have the
WPF app (0.x, last release v0.11.0) installed by MSI, winget (`UmageAI.CodeShellManager`) or
Chocolatey (`codeshellmanager`) must get version 2 through the upgrade they already use, keep
their sessions, and never lose the WPF app's data.

This file is the audit behind that (every identity the two apps carry and what version 2 does
with it), the release checklist, and the decisions still open. Linux and macOS packaging lives
in its own workflow and is not covered here.

Sources compared: the WPF app at `main` (`installer/CodeShellManager.wxs`,
`.github/workflows/{build,winget,chocolatey}.yml`, `.chocolatey/`, `src/CodeShellManager`) and
this branch (`installer/CodeShellManager2.wxs`, `.github/workflows/{release-v2,winget,chocolatey}.yml`,
`.chocolatey/`, `apps/codeshell`, `crates/csm-engine/src/persist.rs`).

## Channels

`installer/CodeShellManager2.wxs` builds two products from one source, chosen with
`-d Channel=`:

| | **stable** (rc and final tags) | **beta** (`-beta.N` tags) |
|---|---|---|
| Product name (Add/Remove Programs) | CodeShellManager | CodeShellManager Beta (was "CodeShellManager 2 Beta"; same UpgradeCode, so older betas upgrade in place) |
| UpgradeCode | `3F7A9B1C-2D5E-4F8A-B0C3-6E1D9A4F2B7E` (the WPF app's) | `8B16A8B3-05F0-4048-B8DD-40B3AC84FF55` (its own) |
| What it replaces | the WPF app (any 0.x), earlier v2 stable builds, and any beta (found by the beta's UpgradeCode, not its name) | earlier betas only |
| Install folder | `C:\Program Files\umage.ai\CodeShellManager` (0.x and older 2.x: `C:\Program Files\umage\CodeShellManager`; see "The install folder moved") | `C:\Program Files\umage.ai\CodeShellManager Beta` (older betas: `...\umage\CodeShellManager Beta`) |
| Start menu / desktop / toast header | `CodeShellManager` | `CodeShellManager Beta` |
| AppUserModelID | `UmageAI.CodeShellManager` | `UmageAI.CodeShellManager.Beta` |
| HKCU key (shortcut key paths) | `Software\umage.ai\CodeShellManager` | `Software\umage.ai\CodeShellManager2Beta` (internal; the old `Software\umage\...` key is left behind in each user's hive, nothing reads it) |
| Compiled-in channel (`CSM_CHANNEL`, `csm_core::channel`) | `stable` (also every local and dev build: unset = stable) | `beta` (set by `release-v2.yml` from the tag) |
| Data folders | `%APPDATA%\CodeShellManager2`, index in `%LOCALAPPDATA%\CodeShellManager2` | `%APPDATA%\CodeShellManager2-Beta`, index in `%LOCALAPPDATA%\CodeShellManager2-Beta` |
| IPC app id (craft-ipc discovery records; Windows pipe `craft-<user>-<app id>-<pid>`) | `codeshell` | `codeshell-beta` |
| Per-session MCP configs when nothing is saved | `<local app data>\CodeShellManager2-mcp` (Linux: `$XDG_RUNTIME_DIR/CodeShellManager2-mcp`; never the shared temp folder) | `<local app data>\CodeShellManager2-Beta-mcp` |

**Beta data (Allan, 2026-11-22):** a beta build keeps its own data folders, so a beta installed
beside the stable app after 2.0 never shares its state. On its first start, when
`%APPDATA%\CodeShellManager2-Beta` does not exist yet, it copies the stable folders (roaming and
local) once per folder (`persist::seed_beta_in`: into a `.copying` folder, then renamed, with a
`.csm-seed-done` marker inside, so a copy cut short is redone, and a roaming copy that worked beside
a local one that did not is retried for the local one on the next start), so testers keep their
sessions, notes and add-ons. Left out of the copy: the stable app's remote-access identity and
enabled listener (`peer`), its paired hosts (`peer-client`) and its Claude config backups
(`claude-config-backups`), so a beta starts with remote access off and nobody paired; and a
database's `-wal`, `-shm` and `-journal` side files, which the stable app may be writing. Never the
other way: a stable build never reads or writes the beta folders on its own (see "Beta to final"
below for the one thing it offers). Not for `--clean`, `--config-dir`, `CSM_CONFIG_DIR`,
`CSM_STATE_PATH` or a portable install. A beta `csm` only finds a beta app (the app id), and
`csm --start` starts the `codeshell.exe` beside it before looking on PATH (with both installed,
PATH could hold the other channel's). The Linux and macOS packages have no separate beta
install, so `release-v2-unix.yml` builds the stable channel.

**Beta to final (COR-36).** A tester who moves from a beta to the final release would otherwise
start the stable app empty beside a full `CodeShellManager2-Beta` folder. So on the stable app's
first run (no `state.json` and no `.bak` of its own; not `--clean`, not a custom state location),
when `%APPDATA%\CodeShellManager2-Beta\state.json` exists, a strip under the title bar offers
*Import from the beta*. It runs the tool `state_import_beta` (the app and the CLI, `csm
state-import-beta`; MCP clients are refused): the beta's groups and sessions that are not here yet
arrive asleep, by id, and the beta's file is only read. Settings, notes and add-ons are not copied
(copy the folder by hand for those); *No thanks* hides the offer, and it is not made again once the
stable app has a `state.json` of its own. A 0.x import on the same first run is separate.

The beta keeps its own identity so testers can run it beside the WPF app; a beta never
upgrades or removes the WPF app. Release candidates use the stable channel so the real upgrade
path (WPF app to version 2) is what gets tested before the final release.

## Identity table

"Must match" means version 2 must carry the WPF app's value or the upgrade breaks. Values are
for the stable channel.

| Identity | WPF app (0.x) | Version 2 (stable) | Status |
|---|---|---|---|
| Product name (MSI `ProductName`, ARP DisplayName) | `CodeShellManager` | `CodeShellManager` | Must match (winget and Chocolatey find the installed app by it). Matches. |
| Manufacturer / ARP Publisher | `umage.ai` | `umage.ai` | Must match (winget correlation). Matches. |
| MSI UpgradeCode | `3F7A9B1C-2D5E-4F8A-B0C3-6E1D9A4F2B7E` | same | **Must match**: it is how MSI MajorUpgrade, and winget's installed-package correlation, find the WPF app. Changed from the beta's code for the stable channel. |
| MSI ProductCode | new per build (WiX `*`) | new per build | Must differ per version (major upgrade). OK. |
| MSI scope | `perMachine` | `perMachine` | **Must match**: a major upgrade only sees products of its own scope. Matches. |
| MajorUpgrade | default (no same-version upgrade) | `AllowSameVersionUpgrades="yes"` (pre-releases differ only in the 4th field, which MSI ignores) | Fine; WiX warns ICE61, expected. The old product is removed before the new one is installed (default schedule). |
| MSI ProductVersion | `0.11.0` (tag version) | final: `2.0.0` (tag version); rc: `2.0.0.(100+N)`; beta: `2.0.0.N`; dev: `2.0.0.0` | 2.x > 0.x, so every v2 build upgrades the WPF app. A final is exactly the tag version, as the WPF app's were, so ARP shows the version winget and Chocolatey publish. |
| Package version (winget, Chocolatey, tag) | `0.11.0` | `2.0.0` | Decided (Allan): version 2 starts at 2.0.0. Higher than 0.11.x for every package manager. |
| Install folder | `ProgramFiles64\umage\CodeShellManager` | `ProgramFiles64\umage.ai\CodeShellManager` | **Differs on purpose** (Allan, 2026-11-24: the company name reads "umage.ai" everywhere). A clean break: the upgrade removes the WPF app (its files and its folder, when empty) and installs into the new folder. See "The install folder moved". |
| Main exe | `CodeShellManager.exe` | `codeshell.exe` (GUI) + `csm.exe` (CLI, MCP) + `csm-session-summary.exe`, `csm-review.exe` | Differs. See Decisions. Start-menu and desktop shortcuts are re-created (pointing at `codeshell.exe`). No `CodeShellManager.exe` ships: the folder changed, so a pin to the old path breaks either way (re-pin). |
| `csm` CLI on PATH | none | install folder appended to the system PATH | New. `csm --start` finds `codeshell.exe` beside `csm.exe` (craft-ipc `resolve("codeshell")`), which is one reason not to rename the exe. |
| Start menu shortcut | `Programs\CodeShellManager\CodeShellManager.lnk` (+ Uninstall) | same names; now carries the AppUserModelID | Matches, so the upgrade replaces it rather than adding a second entry. |
| Desktop shortcut | `CodeShellManager.lnk` | same | Matches. |
| AppUserModelID / toasts | none (tray balloon tips) | `UmageAI.CodeShellManager`, on the Start-menu shortcut and in `codeshell.aumid` beside the exe | New. Toast header shows "CodeShellManager". |
| Registry (HKCU) | `Software\umage\CodeShellManager` (StartMenu, Desktop: shortcut key paths) | `Software\umage.ai\CodeShellManager` (same values) | Differs (company name). Only shortcut key paths; the old key is removed with the old product for the installing user. |
| Protocol handlers / file associations | none (`.csm` backups only through file dialogs) | none | Nothing to migrate. |
| Autostart | none | none | Nothing to migrate. |
| Single instance | none | none (the IPC endpoint is per channel: `codeshell` / `codeshell-beta`) | Running two copies is possible in both; see Risks. |
| Data folder | `%APPDATA%\CodeShellManager\` : `state.json`(+`.bak`), `output.db` (search index and notes), `crash.log`, `update-cache.json`, `webview2\` | `%APPDATA%\CodeShellManager2\` : `state.json`(+`.bak`), `addons\`, `addon-data\`; index in `%LOCALAPPDATA%\CodeShellManager2\` (beta: `CodeShellManager2-Beta`, see Channels) | Deliberately separate: the formats differ and the WPF app's folder stays as it was, so a rollback to 0.x finds its data. |
| First-run migration | n/a | On a true first run (no v2 `state.json` or `.bak`, default location, not `--clean`), `codeshell` imports the WPF app's sessions and groups (`state_import_wpf`, sessions arrive asleep), the settings that map onto version 2's (table "0.x settings" below), its Open with list **as a pending offer** (a one-time card, see below) and project notes (`notes_import_wpf`), on a background thread | Reads the WPF files only; never writes or deletes them. Not imported: search history (output.db's indexed output) and the settings marked "not carried". The empty screen and the palette still offer the import by hand, which does the same. |
| Uninstall | removes program files and shortcuts; leaves `%APPDATA%\CodeShellManager` | same; leaves both data folders; removes the PATH entry | OK. |
| Update check | GitHub `releases/latest` of `umage-ai/CodeShellManager`, compared as `Version` | the same endpoint (a pre-release build reads the release list), a title-bar badge, `update_check`, setting `CheckForUpdates` (see "Update check") | A v2 final published as "latest" makes every running WPF app say "Update available 2.0.0" (pre-releases do not). 0.x's releases (below 2.0.0) are never offered to version 2. |
| Authenticode signing | Azure Trusted Signing (`azure/trusted-signing-action@v2.0.0`) on `CodeShellManager.exe` and the MSI; secrets `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_SIGNING_PROFILE` | same action and secrets, on `codeshell.exe`, `csm.exe`, `csm-session-summary.exe`, `csm-review.exe` and the MSI (never Microsoft's ConPTY files) | Already ported. Added: signatures verified before publishing; a stable tag refuses to publish unsigned. |
| winget | `UmageAI.CodeShellManager`, MSI, `installers-regex '\.msi$'`, komac via `vedantmgoyal9/winget-releaser@v2`, secret `WINGET_TOKEN` (classic PAT, `public_repo` + `workflow`), fork sync of `umage-ai/winget-pkgs` | same workflow, same id and secret; refuses pre-release and pre-2.0 tags | komac reads ProductCode, UpgradeCode, scope and installer type from the new MSI. Same UpgradeCode keeps `winget upgrade` working. |
| Chocolatey | `codeshellmanager`, wraps the release MSI (`Install-ChocolateyPackage`, `/qn /norestart`), secret `CHOCO_API_KEY`, WebView2 check | same id, secret and MSI name pattern (`CodeShellManager-<ver>-Setup.msi`); WebView2 check dropped; uninstall matches exactly `CodeShellManager` (not a beta); package files taken from the tag; MSI signature checked | `choco upgrade` runs the new MSI, which major-upgrades the WPF app. |
| Release assets | `CodeShellManager-<ver>-Setup.msi`, `CodeShellManager-<ver>-win-x64.zip` | same names | Matches (Chocolatey and winget depend on the MSI name/extension). |
| Exe version resource | ProductName CodeShellManager (from .NET) | ProductName/FileDescription `CodeShellManager`, CompanyName `umage.ai` (set in `apps/codeshell/build.rs`) | Fixed on this branch (was the crate name). |
| Window title, About, tray | "CodeShellManager" | "CodeShellManager" (About said "CodeShellManager 2" and "beta" always) | Fixed: About says "pre-release" only for a pre-release version (the heading is renamed on the docs branch). The version shown is the workspace version, which the release workflow stamps from the tag; local builds say 0.0.1. |
| Import wording | n/a | "Import from CodeShellManager (WPF)" (button, palette) | Renamed to "Import from CodeShellManager 0.x" (and the notes import likewise): people know the old app by its version, not its UI toolkit. |
| Third-party licences | n/a | `about.toml` / `about.hbs` configured, never generated | Fixed: the release workflow runs cargo-about and ships `THIRD-PARTY-LICENSES.html` in the MSI and zip. |

## Release checklist (Windows)

Nothing here runs without Allan's say-so. Tags are created by hand.

1. **Before the first stable build:** merge `v2` into `main` (see "Merging into main" below).
2. Release candidate: tag `v2.0.0-rc.1`. `release-v2.yml` builds the **stable** channel, signs,
   and publishes a GitHub **pre-release**. Test on a machine (or VM) with the WPF app installed:
   - by MSI: install `CodeShellManager-2.0.0-rc.1-Setup.msi`. Expect one "CodeShellManager" in
     Add/Remove Programs (version 2.0.0.101), the WPF app gone from `C:\Program Files\umage\CodeShellManager`
     (and `C:\Program Files\umage` gone if nothing else was in it), version 2 in
     `C:\Program Files\umage.ai\CodeShellManager`, `%APPDATA%\CodeShellManager` untouched, and on first start the WPF sessions listed asleep
     and the notes present.
   - with a beta installed too: it is removed (by its UpgradeCode). A beta built before the
     beta data folder kept its state in `%APPDATA%\CodeShellManager2`, which the rc then uses (no
     import, not a first run); a newer beta kept its own (`CodeShellManager2-Beta`), which the rc
     does not read.
   - the 0.x settings: theme, default and launch commands, font size, as the table below says;
     and a card "Use these editors from CodeShellManager 0.x?" listing the 0.x Open with commands
     (Allow adds them, Don't allow discards them; it does not come back).
   - `codeshell --clean` on that machine: empty, no import, no card, nothing written.
   - `csm app-info` in a new terminal (PATH), a toast from a session, Start menu and desktop shortcuts.
   - a taskbar pin to the WPF app's `CodeShellManager.exe` made before the upgrade is broken
     (expected: re-pin the new app); the Start menu and desktop shortcuts work.
   - an agent registration made by `csm setup` (Claude Code, Claude Desktop, Codex or Cursor)
     before the upgrade names `...\umage\CodeShellManager\csm.exe`: start `codeshell` once and
     check it now names `...\umage.ai\CodeShellManager\csm.exe` (a `.csm-backup` was made), or run
     `csm doctor`. The sign-in (autostart) entry, if the person had turned it on, names the new exe.
   - `echo %PATH%` has `C:\Program Files\umage.ai\CodeShellManager` and no `...\umage\...` entry.
3. Final: tag `v2.0.0`. The release is a normal release, marked latest.
4. Dispatch the package workflows with the tag (they never run on their own; see the comments
   in each file for why the `release` trigger does not fire):
   - `gh workflow run winget.yml -f tag=v2.0.0`
   - `gh workflow run chocolatey.yml -f tag=v2.0.0`
5. After the winget PR is merged: `winget upgrade UmageAI.CodeShellManager` on a machine with
   0.11.0 installed; and `choco upgrade codeshellmanager` likewise.

Dry run at any time (signed like a release when the signing secrets are set; artifact only, nothing published): `gh workflow run release-v2.yml --ref v2 -f channel=stable`.

### Version numbers

Decided by Allan: version 2 starts at **2.0.0**. Tags and package versions are semver
(`2.0.0-beta.N`, `2.0.0-rc.N`, `2.0.0`); the workflow refuses tags below 2.0.0. MSI
ProductVersion must be numeric `major.minor.build[.revision]`, and MSI compares only the first
three fields:

| Tag | Channel | MSI ProductVersion | GitHub release |
|---|---|---|---|
| (dispatch) `2.0.0-dev.<run>` | input (`beta` default) | `2.0.0.0` | none (workflow artifact) |
| `v2.0.0-beta.N` | beta | `2.0.0.N` | pre-release |
| `v2.0.0-rc.N` | stable | `2.0.0.(100+N)` | pre-release |
| `v2.0.0` | stable | `2.0.0` | release, latest |

Because the fourth field is ignored, `AllowSameVersionUpgrades` lets each pre-release replace
the previous one of the same channel, and the final replace an rc.

## Merging into main

`main` today holds the WPF app's `build.yml`, `winget.yml`, `chocolatey.yml`, `.chocolatey/`,
`installer/CodeShellManager.wxs` and a copy of `release-v2.yml`. When `v2` is merged into it:

- **Delete `build.yml`.** It triggers on every `v*.*.*` tag and would try to build the .NET app
  for a `v2.0.0` tag on main.
- Take `v2`'s versions of `winget.yml`, `chocolatey.yml`, `.chocolatey/` and `release-v2.yml`
  (add/add conflicts; the WPF ones must not survive: the WPF `chocolateyinstall.ps1` matches
  `CodeShellManager*`, which would also match a beta, and its nuspec describes the WPF app).
- Delete `installer/CodeShellManager.wxs` (the WPF installer); `installer/CodeShellManager2.wxs` is the one.
- `ci.yml` runs only when asked, on a push to a `ci/**` branch (Linux) or `ci-full/**` (all three
  platforms), to keep Actions minutes down; after the merge, keep it that way (do not add `main`).
- A `v2.*` tag builds Windows (`release-v2.yml`) and Linux (`release-v2-unix.yml`). macOS is built
  only when asked for, on a push to `release-macos/<tag>` (see `docs/distribution.md`).
- Until the merge, a `v2.*` tag on a v2 commit runs only v2's workflows (a tag runs the files of
  the commit it points at), so betas are safe to tag now.

## 0.x settings

What `state_import_wpf` (the first-run import and "Import from CodeShellManager 0.x…") does with
each setting in the WPF app's `state.json` (`csm_core::wpf_settings`). The file is read value
by value: a value of the wrong type or out of range is skipped and logged, keys this version
does not know are logged and ignored, and a bad setting never stops the sessions importing. A
missing value keeps version 2's. Re-running the import takes the 0.x values again.

| 0.x setting | Version 2 | Notes |
|---|---|---|
| `Theme` | `Theme` | `dark`, `light` or `system` (any case); anything else skipped. Applied live. |
| `DefaultCommand` | `DefaultCommand` | Trimmed; blank, control characters or over 2000 characters skipped. |
| `DefaultWorkingFolder` | `DefaultWorkingFolder` | Trimmed; control characters skipped. |
| `LaunchCommands` | `LaunchCommands` | Trimmed; blanks, control characters and duplicates dropped; at most 64. The quick-open follows live. |
| `AutoRestoreSessions` | `AutoRestoreSessions` | |
| `AutoResumeClaude` | `AutoResumeClaude` | |
| `ClaudeLaunchStaggerMs` | `ClaudeLaunchStaggerMs` | 0 to 10000. |
| `WarnOnCloseWhenBusy` | `WarnOnCloseWhenBusy` | |
| `ShowToastNotifications`, `ShowNotificationSound` | same | |
| `ShowGitBranch` | `ShowGitBranch` | |
| `ImportWindowsTerminalProfiles` | `ImportWindowsTerminalProfiles` | |
| `GroupDisplayMode` (0 None, 1 FilterStrip, 2 InlineHeaders) | `GroupDisplayMode` (Flat, Rail, Headers) | Same numbers. |
| `ProgramStatusProbe` (0 Auto, 1 On, 2 Off) | `ProgramStatusProbe` | |
| `TerminalFontSize` (CSS px) | `TerminalFontPoints` | Version 2's points are logical pixels, as CSS px are: same number, 6 to 32. Left at 0.x's default (14) it is not carried, so version 2's own default (12) stays. |
| `OpenWith` + `OpenWithSeeded` | the Open with list, **only after consent** | Sanitised like a backup's (`sanitize_imported`: batch files dropped, programs checked at their real path); the entries not listed yet (same program and local arguments) wait in `PendingOpenWith` (state.json). A card "Use these editors from CodeShellManager 0.x?" lists each program and its arguments. Allow (`open_with_pending_answer`, `adopt_open_with: true`) adds them and makes 0.x's default the default; Don't allow discards them. Asked once; a manual re-import asks again only for entries still not listed. Never offered when `OpenWithSeeded` is false. |
| `AnthropicApiKey` | not carried | Nothing in version 2 uses it, and a secret is not copied without a need. |
| `TerminalFontFamily`, `TerminalFontWeight`, `TerminalFontLigatures`, `TerminalLetterSpacing`, `TerminalLineHeight` | not carried | xterm.js CSS values; version 2 draws its own font (built-in JetBrains Mono or `--font`), always with ligatures and its own metrics. |
| `UseBundledConPty` | not carried | Version 2 ships and chooses its own ConPTY; a 0.x workaround for 0.x's bundle should not switch version 2 to the inbox one. |
| `AutoFocusTerminalOnSelect`, `FilterGridByActiveGroup`, `PerGroupLayout`, `ShowGroupsTab`, `ShowWorktreeClusters`, `UngroupedSectionExpanded`, `SearchCollapseAfterNavigate`, `MaxSearchResults`, `ShowTerminalStatusDot`, `SidebarActionIconsMode` | not carried | No such setting in version 2 (it always focuses, keeps layouts per group, shows the status glyph, and so on). |
| `IndexTerminalOutput`, `OutputRetentionDays` | not carried | Version 2's index has no switch or retention setting. |
| `DebugTerminalTrace`, `LegacyDefaultGroupCleared` | not carried | 0.x diagnostics and a 0.x one-shot migration flag. |
| Window bounds, `LastLayout`, `GroupLayouts`, `RecentlyClosed` (not settings) | not carried | Version 2 places its window and lays out panes its own way; the import runs after the window is open. |

## The install folder moved (umage.ai) and the 0.x exe name

Allan, 2026-11-24: the company name must read **umage.ai** everywhere, to avoid brand confusion
(the folder was `umage`; the MSI Manufacturer, the ARP publisher and the winget publisher already
said `umage.ai`). Version 2 therefore installs to `C:\Program Files\umage.ai\<FolderName>`
(`CodeShellManager`, `CodeShellManager Beta`) and its HKCU key is `Software\umage.ai\...`. It is a
**clean break** from the 0.x folder: nothing of 0.x or of an older 2.x stays in
`C:\Program Files\umage\`.

How an upgrade behaves (MSI `MajorUpgrade`, UpgradeCodes unchanged, so the old product is found
whatever folder it is in; it is removed before the new one is installed):

| Coming from | What happens |
|---|---|
| 0.x (WPF), stable MSI | The WPF product is uninstalled by its own MSI: its files, shortcuts and (when empty) `umage\CodeShellManager` go. Version 2 is installed in `umage.ai\CodeShellManager`. Start menu and desktop shortcuts are re-created. **Taskbar pins and hand-made shortcuts to `...\umage\CodeShellManager\CodeShellManager.exe` break** (re-pin). `%APPDATA%\CodeShellManager` is untouched; the first-run import runs as before. |
| An older 2.x stable (installed under `umage`) | Same: the old product is removed, version 2 goes to the new folder. Its PATH entry goes with it and the new one is added. The sign-in entry (HKCU Run) is rewritten to the new `codeshell.exe` by the `AutostartRefresh` action (right after `InstallFiles`) and again by the app on every start; `csm setup` registrations are repaired by the app's start (below). A pin made to `codeshell.exe` breaks. |
| An older beta | The beta product (its own UpgradeCode) is replaced by the new beta, or removed by a stable install, exactly as before; the folder changes from `umage\CodeShellManager Beta` to `umage.ai\CodeShellManager Beta`. |

Leftovers: the new MSI carries two components (`RemoveFolder On="install"`, never
`RemoveFolderEx`) naming `C:\Program Files\umage\<FolderName>` and `C:\Program Files\umage`.
Windows Installer deletes a folder through `RemoveFolder` only when it is empty, so other
products' files, a user's files, or a 0.x copy installed beside a beta are never removed; the old
folders were emptied by the old product's uninstall, which runs first, so they go. (Not verified
on a machine: needs the rc test in the checklist. Removal order is child before parent: the
RemoveFile ids sort that way; if the parent were tried first it would stay until the next
upgrade or repair.) The old HKCU key `Software\umage\CodeShellManager` is removed by the old product for the
installing user; other users keep a stale, unread key.

What holds the old exe path, and what repairs it:

- **PATH** (`Environment`, System, `Permanent="no"`): removed by the old product's uninstall, the
  new folder appended by the new one.
- **Autostart (HKCU Run)**: see the table. Other users' entries on the machine stay pointing at a
  missing exe (Windows skips it) until they start the app.
- **`csm setup` registrations** (Claude Code, Claude Desktop, Codex, Cursor name `csm.exe` by full
  path): at every start the app (`codeshell`, a worker thread, Windows only, not under `--clean` or
  a custom config folder) calls `csm_setup::repair_moved`: an entry setup wrote (program `csm`,
  first argument `mcp`) whose program is in `...\umage\CodeShellManager[ Beta]\` is rewritten to
  the `csm.exe` beside the running app, through setup's own plan: comments kept, a `.csm-backup`
  made, verified, rolled back on failure. It never adds an entry, never touches one setup did not
  write, and a hand-made path (another folder, `csm` on PATH) is left alone. A beta repairs its
  own old folder's entries and any whose program is gone. `csm doctor` reports an entry that
  still names the old folder, and `csm setup` rewrites it (it already updated owned entries). The
  MSI does not do this itself: its custom actions run with the installer service's environment,
  not the person's `USERPROFILE`.
- **`csm --start`** looks for `codeshell.exe` beside `csm.exe` (craft-ipc), so it has no folder
  baked in. File associations, protocol handlers and AUMID shortcuts: none exist; the AUMID
  shortcut is re-created in the Start menu. The update check names a GitHub release, not a path.
  The winget and Chocolatey manifests name no install location (the MSI decides).

**The legacy launcher is gone.** Version 2.0 first shipped a `CodeShellManager.exe` launcher
(`apps/csm-legacy-launcher`) so a 0.x taskbar pin kept working in the *same* folder. With the move
to `umage.ai` a pin to the old path breaks regardless, and in the new folder nothing names
`CodeShellManager.exe`, so the launcher would help no one. It was removed from the MSI, the portable
zip, the release workflow's staging and signing lists, and the repository; one less signed exe.

Release notes text for the upgrade: "CSM 2 installs under `C:\Program Files\umage.ai\`; taskbar
pins to the old app need re-pinning."

## Update check

Version 2 ports 0.x's update check (`UpdateService.cs`: `releases/latest`, a 10 s timeout, a
badge "↑ vX available" in the title bar that opened the release page, a toast, a dismiss ×, a
24 h cache file; 0.x had no setting to turn it off).

- **Setting** `CheckForUpdates` (Settings › General › Updates, `settings_set`; on by default,
  written to `state.json` only when off). The same section has **Check now** (`update_check`).
- **When:** 5 s after start, then every 24 h while the app runs (1 h after a failed check). Never
  under `--clean` or an isolated run (no state file), and never for a local build (the workspace
  version `0.0.1`). `update_check` (app, CLI, MCP; effects `network`, no consent card) asks at once
  whatever the setting or the run says. Nothing is cached on disk.
- **How:** one HTTPS GET to `api.github.com/repos/umage-ai/CodeShellManager/…` on a worker thread
  (`csm_services::update::fetch`, ureq over rustls, already in the tree), 10 s deadline, HTTPS
  only, at most 3 redirects, body capped at 4 MB, the system proxy. The only identifying header is
  `User-Agent: CodeShellManager/<version>`.
- **What is offered** (`csm_core::update`): a final build reads `releases/latest` and is offered
  newer final releases only. A pre-release build reads the release list (`releases/latest` never
  names a pre-release) and is offered the newest of: newer finals, and newer pre-releases **of its
  own kind** (a beta sees betas, an rc sees rcs; a beta is never offered an rc, whose stable MSI
  would replace the beta install). Drafts and anything below 2.0.0 (0.x's releases) are never
  offered. The payload is untrusted: a bad tag, wrong types or garbage offer nothing; an
  unparsable answer is an error.
- **Shown as** a pill "Update available: 2.1.0" at the right of the title bar. A click opens the
  release page through `open_url` (the link is built from the validated tag, never taken from
  the payload); its × (`update_dismiss`) hides it for that version until the app restarts.
  Nothing is downloaded or installed. Unlike 0.x there is no OS toast: the badge stays until
  dismissed.

## Decisions for Allan

Decided by Allan on 2026-11-22 (12 on 2026-11-24): 1 (keep `codeshell.exe`), 12 (umage.ai folder), 3 and 8 (a beta data folder), 4
(import 0.x settings, Open with only after consent) and 7 (MIT).

1. **Exe name: decided, keep `codeshell.exe`** (Allan, 2026-11-22). Renaming would have needed
   `csm --start`'s lookup changed on Windows only, and bought one thing: taskbar pins and
   hand-made shortcuts to `CodeShellManager.exe` keep working. The Start-menu and desktop
   shortcuts, the only ones the WPF installer made, are re-created by the upgrade. (On
   2026-11-23 a `CodeShellManager.exe` launcher was added for the pins; on 2026-11-24 it was
   removed again with the folder move, see decision 12.)
2. **Release candidates on the stable channel.** Implemented: `-rc.N` builds the stable MSI
   (replaces the WPF app; published only as a GitHub pre-release). Recommendation: keep, so the
   upgrade is tested before the final. Alternative: rc on the beta channel, and test the stable
   MSI only through a `channel=stable` dry run.
3. **Stable removes a beta install, and betas have their own data: decided** (Allan,
   2026-11-22). The stable MSI still removes a beta, by its UpgradeCode. Betas keep their own data
   folders (`CodeShellManager2-Beta`) and IPC app id and pipe (`codeshell-beta`), and copy the
   stable folders once on their first start (see Channels). The beta product is now named
   "CodeShellManager Beta" (was "CodeShellManager 2 Beta"; same UpgradeCode).
4. **Which WPF data is imported: decided, settings too** (Allan, 2026-11-22). Sessions, groups,
   notes and the settings in "0.x settings" above, once automatically on first run and by hand
   the same way. The Open with list is only offered (the card), never applied silently. Not
   imported: the search index's past output.
5. **winget manifest extras.** komac copies the previous manifest's metadata. Recommendation:
   after the first 2.0.0 submission, add `Commands: [csm]` and refresh the description/tags in
   the winget-pkgs PR by hand (the WPF manifest says WebView2/WPF).
6. **Chocolatey icon.** The nuspec still points at the WPF app's `app.png` pinned to a commit
   (jsDelivr). Version 2's only icon is `apps/codeshell/assets/codeshell.ico`. Recommendation:
   add a PNG of the v2 icon to the repo after the merge and pin `iconUrl` to that commit.
7. **Licence: decided, MIT** (Allan, 2026-11-22). The workspace `Cargo.toml` says
   `license = "MIT"` (every crate inherits it) and the macOS `Info.plist` says MIT, matching
   `LICENSE`, the README, the nuspec and the WPF app. `deny.toml` and `about.toml` accept MIT. The
   remaining "MIT OR Apache-2.0" mentions are third-party code (craft-anything, craft-ipc, egui,
   two-face), not ours.
8. **Data folder name: decided.** `%APPDATA%\CodeShellManager2` stays for stable (Allan); the
   beta's is `CodeShellManager2-Beta`. Neither is user-facing.
9. **Workspace version.** `Cargo.toml` is `0.0.1`; only release builds carry the real version.
   Recommendation: set it to `2.0.0-dev` after the merge so local builds and `csm app-info`
   report a 2.x version (one test, `real_engine.rs` "CodeShellManager 0.", moves with it).
10. **Package descriptions.** The nuspec description was rewritten for version 2 from the code,
   not from the user docs (another agent owns those); please read it before the first push.
11. **(New) A beta tester's later changes stay in the beta.** The copy is one way, once: a tester
   who moves from a new beta to the stable rc or final finds the stable folder as it was (or, with
   none, gets the 0.x import). Betas built before this change used `CodeShellManager2`, so nothing
   changes for them. Recommendation: accept, and say so in the beta release notes.
12. **Install folder `C:\Program Files\umage.ai\`, a clean break, no legacy launcher: decided**
   (Allan, 2026-11-24). Brand: the company reads "umage.ai" everywhere. Both channels move
   (`umage.ai\CodeShellManager`, `umage.ai\CodeShellManager Beta`), UpgradeCodes unchanged so
   MajorUpgrade still removes 0.x and older 2.x/betas; nothing stays under `umage\`. Taskbar
   pins to the old app break (re-pin); the launcher `apps/csm-legacy-launcher` is deleted. See
   "The install folder moved".

## Risks for existing WPF users

- **Taskbar pins** to the WPF app's `...\umage\CodeShellManager\CodeShellManager.exe` (and to an
  older 2.x's `codeshell.exe` there) **break** after the upgrade: Windows says the item "has been
  changed or moved". Accepted (Allan, 2026-11-24): the install folder moved to `umage.ai` and a
  launcher in the new folder would not fix a pin to the old path (it was tried on 2026-11-23 and
  removed). People re-pin the new app. Start menu and desktop shortcuts are re-created and work.
  Release notes say so: "CSM 2 installs under `C:\Program Files\umage.ai\`; taskbar pins to the
  old app need re-pinning."
- **Scripts and agent configs that name the old path** (`C:\Program Files\umage\CodeShellManager\csm.exe`):
  the ones `csm setup` wrote are repaired when the app starts; a hand-written one is not. The PATH
  entry is replaced by the installer.
- **The WPF app running during the upgrade**: MSI asks to close it (files in use); a silent
  winget/choco upgrade may schedule a reboot (exit 3010) instead. Same as WPF-to-WPF upgrades.
- **Import is one-shot.** If someone starts version 2 before the WPF app has saved its last
  changes, or deletes v2's state later, the automatic import does not re-run while a v2 state
  file exists; the import is still available by hand (palette: "Import from CodeShellManager
  0.x…") and merges sessions by id, so running it again is safe (it takes the 0.x settings again).
- **Imported sessions arrive asleep**, not running, on the first start only. Waking one starts it.
- **Rollback**: reinstalling the WPF MSI over version 2 fails with "A newer version is already
  installed"; uninstall version 2 first. The WPF data is untouched, so the WPF app comes back as it was.
- **A beta's first copy** reads the stable index while the stable app may be running (SQLite);
  a torn copy only affects the beta's search index and notes, never the stable app's.
- **Two `csm` on PATH** with stable and beta both installed: the first on PATH wins, and it only
  talks to its own channel's app (`csm setup` also registers both under the MCP name `codeshell`).
- **Unsigned stable builds** can no longer be published (the workflow refuses). A dry-run
  stable MSI is signed too (since the signing secrets are set) and installing one on a real machine
  upgrades its WPF app.

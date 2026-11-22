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
| Product name (Add/Remove Programs) | CodeShellManager | CodeShellManager 2 Beta |
| UpgradeCode | `3F7A9B1C-2D5E-4F8A-B0C3-6E1D9A4F2B7E` (the WPF app's) | `8B16A8B3-05F0-4048-B8DD-40B3AC84FF55` (its own) |
| What it replaces | the WPF app (any 0.x), earlier v2 stable builds, and any CodeShellManager 2 Beta | earlier betas only |
| Install folder | `C:\Program Files\umage\CodeShellManager` | `C:\Program Files\umage\CodeShellManager 2 Beta` |
| Start menu / desktop | `CodeShellManager` | `CodeShellManager 2 Beta` |
| AppUserModelID | `UmageAI.CodeShellManager` | `UmageAI.CodeShellManager.Beta` |
| HKCU key (shortcut key paths) | `Software\umage\CodeShellManager` | `Software\umage\CodeShellManager2Beta` |

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
| Install folder | `ProgramFiles64\umage\CodeShellManager` | same | Matches. The WPF app's files (`CodeShellManager.exe`, `Assets\`, `conpty.dll`, `OpenConsole.exe`) are removed by the upgrade's uninstall step. |
| Main exe | `CodeShellManager.exe` | `codeshell.exe` (GUI) + `csm.exe` (CLI, MCP) + `csm-session-summary.exe`, `csm-review.exe` | Differs. See Decisions. Start-menu and desktop shortcuts are re-created; **taskbar pins and users' own shortcuts or scripts that name `CodeShellManager.exe` break**. |
| `csm` CLI on PATH | none | install folder appended to the system PATH | New. `csm --start` finds `codeshell.exe` beside `csm.exe` (craft-ipc `resolve("codeshell")`), which is one reason not to rename the exe. |
| Start menu shortcut | `Programs\CodeShellManager\CodeShellManager.lnk` (+ Uninstall) | same names; now carries the AppUserModelID | Matches, so the upgrade replaces it rather than adding a second entry. |
| Desktop shortcut | `CodeShellManager.lnk` | same | Matches. |
| AppUserModelID / toasts | none (tray balloon tips) | `UmageAI.CodeShellManager`, on the Start-menu shortcut and in `codeshell.aumid` beside the exe | New. Toast header shows "CodeShellManager". |
| Registry (HKCU) | `Software\umage\CodeShellManager` (StartMenu, Desktop: shortcut key paths) | same key and values | Matches. |
| Protocol handlers / file associations | none (`.csm` backups only through file dialogs) | none | Nothing to migrate. |
| Autostart | none | none | Nothing to migrate. |
| Single instance | none | none | Running two copies is possible in both; see Risks. |
| Data folder | `%APPDATA%\CodeShellManager\` : `state.json`(+`.bak`), `output.db` (search index and notes), `crash.log`, `update-cache.json`, `webview2\` | `%APPDATA%\CodeShellManager2\` : `state.json`(+`.bak`), `addons\`, `addon-data\`; index in `%LOCALAPPDATA%\CodeShellManager2\` | Deliberately separate: the formats differ and the WPF app's folder stays as it was, so a rollback to 0.x finds its data. |
| First-run migration | n/a | On a true first run (no v2 `state.json` or `.bak`, default location, not `--clean`), `codeshell` imports the WPF app's sessions and groups (`state_import_wpf`, sessions arrive asleep) and project notes (`notes_import_wpf`) on a background thread | **New on this branch.** Reads the WPF files only; never writes or deletes them. Not imported: WPF settings, search history (output.db's indexed output). The empty screen still offers the import by hand. |
| Uninstall | removes program files and shortcuts; leaves `%APPDATA%\CodeShellManager` | same; leaves both data folders; removes the PATH entry | OK. |
| Update check | GitHub `releases/latest` of `umage-ai/CodeShellManager`, compared as `Version` | none in v2 | A v2 final published as "latest" makes every running WPF app say "Update available 2.0.0" (pre-releases do not). |
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
     Add/Remove Programs (version 2.0.0.101), the WPF app gone from `C:\Program Files\umage\CodeShellManager`,
     `%APPDATA%\CodeShellManager` untouched, and on first start the WPF sessions listed asleep
     and the notes present.
   - with a CodeShellManager 2 Beta installed too: it is removed; its `%APPDATA%\CodeShellManager2` state is kept and used (no import then, because it is not a first run).
   - `csm app-info` in a new terminal (PATH), a toast from a session, Start menu and desktop shortcuts.
3. Final: tag `v2.0.0`. The release is a normal release, marked latest.
4. Dispatch the package workflows with the tag (they never run on their own; see the comments
   in each file for why the `release` trigger does not fire):
   - `gh workflow run winget.yml -f tag=v2.0.0`
   - `gh workflow run chocolatey.yml -f tag=v2.0.0`
5. After the winget PR is merged: `winget upgrade UmageAI.CodeShellManager` on a machine with
   0.11.0 installed; and `choco upgrade codeshellmanager` likewise.

Dry run at any time (unsigned, artifact only): `gh workflow run release-v2.yml --ref v2 -f channel=stable`.

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
- `ci.yml` runs on `v2` only; it needs `main` added to its triggers (not done here: it is not
  release-owned).
- Until the merge, a `v2.*` tag on a v2 commit runs only v2's workflows (a tag runs the files of
  the commit it points at), so betas are safe to tag now.

## Decisions for Allan

1. **Exe name** (`codeshell.exe` vs the WPF app's `CodeShellManager.exe`).
   Recommendation: keep `codeshell.exe`. Renaming would need `csm --start`'s lookup
   (craft-ipc `resolve("codeshell")`, `APP_ID` in csm-mcp) changed on Windows only, and buys one
   thing: taskbar pins and hand-made shortcuts to
   `CodeShellManager.exe` keep working. The Start-menu and desktop shortcuts, the only ones the
   WPF installer made, are re-created by the upgrade. Mention "re-pin to the taskbar" in the
   release notes.
2. **Release candidates on the stable channel.** Implemented: `-rc.N` builds the stable MSI
   (replaces the WPF app; published only as a GitHub pre-release). Recommendation: keep, so the
   upgrade is tested before the final. Alternative: rc on the beta channel, and test the stable
   MSI only through a `channel=stable` dry run.
3. **Stable removes CodeShellManager 2 Beta.** Implemented, because both use
   `%APPDATA%\CodeShellManager2` and a second launch only finds a running app through its IPC record (no lock on the state
   file). Recommendation:
   keep. After 2.0.0, a new beta tag would again install beside the stable app and share its
   state folder; if betas continue after 2.0, give them their own data folder first.
4. **Which WPF data is imported.** Implemented: sessions, groups, notes, once, automatically on
   first run. Not imported: WPF settings (theme, launch commands, Open with list), and the search
   index's past output. Recommendation: settings would be good to import too (the `.csm` import
   already maps settings via `adopt_settings`); it needs a decision because the WPF Open with
   list runs programs and the `.csm` import only adopts it after asking.
5. **winget manifest extras.** komac copies the previous manifest's metadata. Recommendation:
   after the first 2.0.0 submission, add `Commands: [csm]` and refresh the description/tags in
   the winget-pkgs PR by hand (the WPF manifest says WebView2/WPF).
6. **Chocolatey icon.** The nuspec still points at the WPF app's `app.png` pinned to a commit
   (jsDelivr). Version 2's only icon is `apps/codeshell/assets/codeshell.ico`. Recommendation:
   add a PNG of the v2 icon to the repo after the merge and pin `iconUrl` to that commit.
7. **Licence metadata disagrees.** `Cargo.toml` says `license = "MIT OR Apache-2.0"`; `LICENSE`,
   the README, the nuspec (`licenseUrl`) and the WPF app say MIT. Recommendation: make
   `Cargo.toml` `MIT` unless dual licensing is intended (then add `LICENSE-APACHE` and say so).
8. **Data folder name** `%APPDATA%\CodeShellManager2` stays. Recommendation: keep. Renaming it
   to the WPF app's `CodeShellManager` would mix two state formats in one folder and break
   rollback; renaming to anything else would need another migration for beta users (the
   `CodeShellManager-rs` move in `persist::migrate_legacy_data_dirs` shows the pattern). It is
   not user-facing.
9. **Workspace version.** `Cargo.toml` is `0.0.1`; only release builds carry the real version.
   Recommendation: set it to `2.0.0-dev` after the merge so local builds and `csm app-info`
   report a 2.x version (one test, `real_engine.rs` "CodeShellManager 0.", moves with it).
10. **Package descriptions.** The nuspec description was rewritten for version 2 from the code,
   not from the user docs (another agent owns those); please read it before the first push.

## Risks for existing WPF users

- **Taskbar pins** to `CodeShellManager.exe` stop working after the upgrade (Windows offers to
  remove the broken pin). Start menu and desktop shortcuts are fine.
- **The WPF app running during the upgrade**: MSI asks to close it (files in use); a silent
  winget/choco upgrade may schedule a reboot (exit 3010) instead. Same as WPF-to-WPF upgrades.
- **Import is one-shot.** If someone starts version 2 before the WPF app has saved its last
  changes, or deletes v2's state later, the automatic import does not re-run while a v2 state
  file exists; the import is still available by hand (palette: "Import from CodeShellManager
  0.x…") and merges by id, so running it again is safe.
- **Imported sessions arrive asleep**, not running, on the first start only. Waking one starts it.
- **Rollback**: reinstalling the WPF MSI over version 2 fails with "A newer version is already
  installed"; uninstall version 2 first. The WPF data is untouched, so the WPF app comes back as it was.
- **Two apps, one data folder**: a CodeShellManager 2 Beta installed after 2.0.0 shares
  `%APPDATA%\CodeShellManager2` with the stable app (Decision 3).
- **Unsigned stable builds** can no longer be published (the workflow refuses), but a dry-run
  stable MSI is unsigned; installing one on a real machine upgrades its WPF app.

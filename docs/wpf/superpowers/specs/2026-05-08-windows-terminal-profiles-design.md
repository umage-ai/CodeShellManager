# Windows Terminal Profile Import — Design Spec

**Date:** 2026-05-08
**Status:** Approved
**Scope:** Optional import of Windows Terminal profiles into the New Session dialog, with per-session theme/font overrides applied to the embedded xterm.js instance.
**Related issue:** [#6 — Support for Terminal Profiles](https://github.com/umage-ai/CodeShellManager/issues/6)

---

## Context

CodeShellManager hosts multiple ConPTY sessions in a tabbed/grid layout. Today, the New Session dialog only knows a hardcoded list of launch commands (`claude`, `pwsh`, `cmd`, ...) plus a free-text custom command. Users with curated Windows Terminal setups (custom WSL profiles, opinionated fonts, color schemes) cannot reuse that work — they must re-enter command, working folder, and accept the app's default xterm appearance.

This feature reads the user's Windows Terminal `settings.json` and offers each profile in the New Session dialog. Picking a profile pre-fills folder/command/name and stamps a per-session theme + font + cursor overlay onto the xterm.js instance. The app chrome (sidebar, toolbar, accent stripe, active-ring) keeps Catppuccin colors — only the terminal pane interior changes.

---

## Goals

- Read profiles from Windows Terminal's `settings.json` (Stable, Preview, and unpackaged install paths).
- Surface profiles in a new optional combobox at the top of the New Session dialog (Local mode only).
- Apply per-session overrides for: command, working folder, suggested name, color scheme, font (family/size/weight/ligatures), cursor (shape/blink), padding, transparency, and a best-effort retro overlay.
- Keep app chrome theming untouched — only the inner xterm instance is restyled.
- Gate the entire feature behind an explicit, off-by-default user setting.

## Non-goals

- Editing or creating Windows Terminal profiles inside CodeShellManager.
- Live-watching `settings.json` for changes (we re-read on every dialog open).
- True acrylic/Mica blur (WebView2 cannot render desktop-blur effects beneath its content).
- A WebGL CRT-effect renderer matching `experimental.retroTerminalEffect` — we do a CSS scanlines overlay only.
- Importing other Windows Terminal customizations: starting tab title color, bell sounds, scrollbar visibility, key bindings, snippets.

---

## User flow

1. User opens **Settings** and ticks **Show Windows Terminal profiles when creating sessions**. (Off by default.)
2. User clicks **＋ New Session**. The dialog now shows a **Profile (optional)** combobox above **Working Folder**.
3. The combobox is populated from `settings.json` profiles. The first entry is `— none —`. Hidden profiles are filtered out.
4. Selecting a profile:
   - Sets **Working Folder** to the profile's expanded `startingDirectory` (only if currently empty).
   - Adds the profile's `commandline` (split into exe + args) as a transient entry in the **Command** combobox and selects it.
   - Sets **Session Name** to the profile name (only if currently empty — same auto-fill rule the dialog already uses).
   - Stashes per-session theme/font/cursor/padding/opacity/retro overrides on the dialog's output, to be copied onto the new `ShellSession`.
5. User clicks **Start Session**. The session launches; the bridge applies the overrides to the xterm.js instance after navigation completes.
6. Re-selecting `— none —` clears all overrides and reverts the Command combobox to the default list.

When the setting is **off**, the Profile combobox is not added to the dialog at all — there is no disabled-state placeholder.

When the user is in **Remote (SSH)** mode, the Profile combobox is hidden even if the setting is on. (SSH sessions don't have a meaningful local "profile" — the remote shell decides its own appearance.)

---

## Architecture

### Components added

```
Models/
  WindowsTerminalProfile.cs         POCO returned by the service
Services/
  WindowsTerminalProfileService.cs  locates settings.json, parses, returns profiles
  BuiltInTerminalSchemes.cs         static lookup of WT-shipped schemes (Campbell, etc.)
  SchemeMapper.cs                   WT scheme JSON → xterm theme JSON
  CommandLineSplitter.cs            quote-aware (exe, args) split
Assets/
  terminal-init.js                  shared script extracted from terminal.html
  terminal-transparent.html         alternate entry constructing Terminal with allowTransparency
```

### Components changed

- `Terminal/TerminalBridge.cs` — adds `SetOptionsAsync(ShellSession)` that posts the override payload via `CoreWebView2.PostWebMessageAsString` after the existing `NavigationCompleted` handler has flushed.
- `Assets/terminal.html` — `setOptions` handler grows (theme, cursor, padding, retro); script body extracted to `terminal-init.js` so the new `terminal-transparent.html` can share it.
- `Views/NewSessionDialog.xaml/.cs` — adds Profile combobox row and dialog output fields.
- `Views/SettingsWindow.xaml/.cs` — adds `ImportWindowsTerminalProfiles` checkbox.
- `Models/ShellSession.cs` — adds nullable per-session override fields (see Data model).
- `Models/AppState.cs` — adds `AppSettings.ImportWindowsTerminalProfiles`.
- `MainWindow.xaml.cs` — when opening the New Session dialog, conditionally pass profiles in; in `LaunchSessionAsync`, after `NavigationCompleted` has fired, call `_bridge.SetOptionsAsync(session)` if any override fields are non-null.
- `StateService` — none required; new `ShellSession` fields serialize automatically (System.Text.Json with default options handles them).

### Data flow

```
settings.json (WT)
        │
        ▼
WindowsTerminalProfileService.GetProfiles()
        │  → IReadOnlyList<WindowsTerminalProfile>
        ▼
NewSessionDialog                                 user picks a profile
        │  → SelectedFolder/Command/Args/Name + override fields
        ▼
new ShellSession { ColorSchemeJson, FontFamily, ... }
        │  → SessionManager → state.json (persisted)
        ▼
LaunchSessionAsync → TerminalBridge attached → SetOptionsAsync(session)
        │  → postMessage { type: "setOptions", options: { theme, fontFamily, ... } }
        ▼
terminal.html → term.options.* + CSS padding + scanlines overlay class
```

---

## Data model

### `WindowsTerminalProfile`

```csharp
public sealed class WindowsTerminalProfile
{
    public string Guid { get; init; } = "";              // stable id; ComboBox Tag
    public string Name { get; init; } = "";              // display label
    public string Source { get; init; } = "";            // "Stable" | "Preview" | "Unpackaged"
    public string Commandline { get; init; } = "";       // raw, may contain quoted exe + args
    public string StartingDirectory { get; init; } = ""; // env-expanded, "~" → %USERPROFILE%

    // Resolved-and-mapped appearance overrides (null = WT didn't specify)
    public string? FontFamily { get; init; }
    public int? FontSize { get; init; }
    public string? FontWeight { get; init; }
    public bool? FontLigatures { get; init; }
    public string? CursorShape { get; init; }   // "block" | "underline" | "bar"
    public bool? CursorBlink { get; init; }
    public string? Padding { get; init; }        // CSS shorthand, e.g. "8px 8px"
    public double? BackgroundOpacity { get; init; } // 0.0–1.0
    public bool? RetroEffect { get; init; }
    public string? ColorSchemeJson { get; init; }   // pre-baked xterm theme JSON, or null
}
```

The "resolved-and-mapped" fields are filled by the service so callers never deal with WT-specific names like `cursorColor` or `purple`.

### `ShellSession` additions

The same nullable appearance fields plus `ColorSchemeJson` are added to `ShellSession`. They are persisted to `state.json` so a session relaunches with the same look. **No** `ProfileGuid` reference is stored — once a profile is "stamped" onto a session it is independent. This keeps sessions stable even if the user later edits or deletes the profile in Windows Terminal.

### `AppSettings` addition

```csharp
public bool ImportWindowsTerminalProfiles { get; set; } = false;
```

---

## settings.json discovery

Probe in order, keep the first match per source category:

| Source | Path |
|---|---|
| Stable (Store) | `%LOCALAPPDATA%\Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\settings.json` |
| Preview (Store) | `%LOCALAPPDATA%\Packages\Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe\LocalState\settings.json` |
| Unpackaged | `%LOCALAPPDATA%\Microsoft\Windows Terminal\settings.json` |

If multiple sources are present, profiles from each are returned and the source name is appended to the display label when there is a name collision (e.g. `"PowerShell"` and `"PowerShell (Preview)"`). When the setting is on but no `settings.json` is found, the combobox is empty (only `— none —` shown).

Read errors (file locked, JSON parse failure) are logged via the existing crash log helper and the service returns `[]`. They never throw.

---

## Profile JSON parsing

Top-level shape we consume:

```jsonc
{
  "profiles": {
    "defaults": { "fontFace": "...", "commandline": "...", ... },
    "list":    [ { "guid": "...", "name": "...", "commandline": "...", ... }, ... ]
  },
  "schemes": [ { "name": "Campbell", "background": "#0C0C0C", ... }, ... ]
}
```

Every profile inherits from `profiles.defaults`. The service flattens the merge so each emitted `WindowsTerminalProfile` has its effective settings. Profiles with `hidden: true` are skipped.

If a profile lacks `commandline`, fall back to `profiles.defaults.commandline`, then to `cmd.exe`.

If a profile lacks `startingDirectory`, omit it (the new-session dialog leaves the folder field empty and the user fills it in).

### Color scheme resolution

Profile's `colorScheme` field is a string. Resolution order:

1. Look up by name in the file's top-level `schemes[]`.
2. Fall back to `BuiltInTerminalSchemes.Lookup(name)` — a static dictionary of WT-shipped schemes (Campbell, Campbell Powershell, Vintage, One Half Dark, One Half Light, Solarized Dark, Solarized Light, Tango Dark, Tango Light).
3. Return `null` (xterm uses its own default theme).

`SchemeMapper.ToXtermTheme(WtScheme)` produces the JSON the bridge ships:

| WT key | xterm key |
|---|---|
| `background` | `background` |
| `foreground` | `foreground` |
| `cursorColor` | `cursor` |
| `selectionBackground` | `selectionBackground` |
| `black/red/green/yellow/blue/cyan/white` | `black/red/green/yellow/blue/cyan/white` |
| `purple` | `magenta` |
| `brightBlack/...../brightWhite` | `brightBlack/...../brightWhite` |
| `brightPurple` | `brightMagenta` |

When the scheme has no `cursorColor`, omit it (xterm picks one). When no selection color, omit it.

If the session has `BackgroundOpacity < 1.0`, `SchemeMapper` rewrites `background` from `#RRGGBB` to `rgba(r, g, b, opacity)` and emits `theme.background` accordingly.

### `commandline` splitting

`CommandLineSplitter.Split(string)` returns `(exe, args)` using a quote-aware single-pass scanner:

- `cmd.exe /k foo` → `("cmd.exe", "/k foo")`
- `"C:\Program Files\app.exe" -x` → `("C:\\Program Files\\app.exe", "-x")` (with quotes stripped from exe)
- `wsl.exe -d Ubuntu` → `("wsl.exe", "-d Ubuntu")`
- `pwsh` → `("pwsh", "")`

### Other field mappings (WT → us)

| WT field | Our field | Behavior |
|---|---|---|
| `font.face` | `FontFamily` | direct (single family is fine for xterm) |
| `font.size` | `FontSize` | int; if WT had a fractional size we round |
| `font.weight` (string or int) | `FontWeight` | "normal", "bold", or stringified number |
| `font.features.calt: 0` | `FontLigatures = false` | absence → leave null (xterm default = false anyway) |
| `cursorShape: bar` | `CursorShape = "bar"` | direct |
| `cursorShape: filledBox` / `vintage` | `CursorShape = "block"` | xterm has no `vintage` |
| `cursorShape: emptyBox` | `CursorShape = "block"`, `CursorBlink = false` | closest visual approximation |
| `cursorShape: underscore` / `doubleUnderscore` | `CursorShape = "underline"` | xterm has no double |
| `padding: "8"` / `"8, 12"` / `"4, 8, 4, 8"` | `Padding = "8px"` / `"8px 12px"` / `"4px 8px 4px 8px"` | parse 1/2/4 numbers |
| `useAcrylic: true` + `opacity: 0.8` | `BackgroundOpacity = 0.8` | acrylic blur not reproducible — flat alpha only |
| `experimental.retroTerminalEffect: true` | `RetroEffect = true` | CSS scanlines overlay (best-effort) |

WT fields we deliberately do **not** read: `bellStyle`, `closeOnExit`, `historySize`, `scrollbarState`, `tabTitle` (we only use `name`), `unfocusedAppearance`, `experimental.*` (other than the retro flag), key bindings.

---

## Bridge / xterm wiring

### `TerminalBridge.SetOptionsAsync(ShellSession)`

Composes the JSON payload from any non-null override fields on the session and posts it via `WebView2.CoreWebView2.PostWebMessageAsString(json)`. The bridge's existing init path already awaits `NavigationCompleted` before returning (see the `navDone` TCS in `TerminalBridge`), so `SetOptionsAsync` is a plain post that callers invoke once the bridge init has completed. No additional buffering is required.

Payload shape:

```jsonc
{
  "type": "setOptions",
  "options": {
    "theme": { /* xterm theme object, optional */ },
    "fontFamily": "Cascadia Code",
    "fontSize": 14,
    "fontWeight": "normal",
    "fontLigatures": true,
    "cursorStyle": "bar",
    "cursorBlink": true,
    "padding": "8px 8px",
    "retro": false
  }
}
```

Only the keys that are set on the session appear in `options` — the bridge does not send `null` placeholders.

### `terminal.html` — `setOptions` handler additions

The existing handler covers `fontFamily/fontSize/fontLigatures/fontWeight/letterSpacing/lineHeight`. Add:

```js
if (opts.theme        !== undefined) term.options.theme        = opts.theme;
if (opts.cursorStyle  !== undefined) term.options.cursorStyle  = opts.cursorStyle;
if (opts.cursorBlink  !== undefined) term.options.cursorBlink  = opts.cursorBlink;
if (opts.padding      !== undefined) document.getElementById('terminal').style.padding = opts.padding;
if (opts.retro        !== undefined) document.body.classList.toggle('retro', !!opts.retro);
fitAddon.fit();
```

### Transparency

xterm.js requires `allowTransparency: true` to be set in the constructor — it cannot be flipped at runtime. To support transparent sessions we host two near-identical entry points:

- `terminal.html` — current opaque init (default for sessions with no opacity override or `BackgroundOpacity == 1.0`).
- `terminal-transparent.html` — same source minus `<style>html, body { background: #1e1e1e }` and with `allowTransparency: true` in the `Terminal` constructor.

When the session has `BackgroundOpacity < 1.0`, the bridge navigates to `terminal-transparent.html` instead of `terminal.html`. The behind-the-WebView2 surface in WPF is the existing terminal wrapper Border, whose background is already a Catppuccin color — so the visible "showthrough" is the app's own panel color, not the desktop. (No acrylic.)

To avoid duplicating ~250 lines of script across two HTML files, the shared script is moved to a `terminal-init.js` resource and both HTML files include it; only the `<style>` block and the `Terminal` constructor `allowTransparency` flag differ between the two entry points.

### Retro overlay CSS

Added once to both HTML entry points:

```css
body.retro::before {
  content: "";
  position: fixed; inset: 0;
  pointer-events: none;
  background:
    repeating-linear-gradient(
      to bottom,
      rgba(0,0,0,0) 0,
      rgba(0,0,0,0) 2px,
      rgba(0,0,0,0.18) 3px
    );
  mix-blend-mode: multiply;
  z-index: 50;
}
```

Documented limitation: WT's actual retro effect is a WebGL shader with bloom and barrel distortion. We approximate with scanlines only.

---

## UI changes

### `NewSessionDialog.xaml`

Insert a new row at the top of the Local panel (above Working Folder), bound to a code-behind-controlled `Visibility`. The new row goes at `Grid.Row="1"` (between Session Type and Working Folder); existing rows for Folder/Ssh/Command/CustomArgs/Name all shift down by one. We re-number rather than using `Grid.Row="999"` tricks.

```xml
<StackPanel Grid.Row="1" x:Name="ProfilePanel" Visibility="Collapsed" Margin="0,0,0,12">
  <TextBlock Text="Profile (optional)" Style="{StaticResource Label}"/>
  <ComboBox x:Name="ProfileCombo"
            AutomationProperties.AutomationId="NewSessionProfileCombo"
            SelectionChanged="ProfileCombo_SelectionChanged"/>
</StackPanel>
```

### `NewSessionDialog.xaml.cs`

Constructor signature grows:

```csharp
public NewSessionDialog(
    string defaultFolder = "",
    IEnumerable<string>? launchCommands = null,
    IReadOnlyList<WindowsTerminalProfile>? profiles = null)
```

When `profiles` is non-null and non-empty, populate `ProfileCombo` (first item `— none —` with `Tag = null`, then each profile with `Tag = profile`) and set `ProfilePanel.Visibility = Visible` for Local mode. When the user toggles to Remote, the panel is hidden the same way `LocalPanel` is hidden.

Output API additions on the dialog:

```csharp
public string? ProfileFontFamily { get; private set; }
public int?    ProfileFontSize { get; private set; }
public string? ProfileFontWeight { get; private set; }
public bool?   ProfileFontLigatures { get; private set; }
public string? ProfileCursorShape { get; private set; }
public bool?   ProfileCursorBlink { get; private set; }
public string? ProfilePadding { get; private set; }
public double? ProfileBackgroundOpacity { get; private set; }
public bool?   ProfileRetroEffect { get; private set; }
public string? ProfileColorSchemeJson { get; private set; }
```

`Start_Click` reads the selected profile (if any) and copies these values onto the output properties. `MainWindow` then copies them onto the new `ShellSession`. `MainWindow` is the only consumer.

### `SettingsWindow`

New checkbox in a sensible group (alongside other "appearance" toggles like `ShowGitBranch`):

```
[ ] Show Windows Terminal profiles when creating sessions
    Reads %LOCALAPPDATA%\…\settings.json. No editing — read-only.
```

Bound directly to `AppSettings.ImportWindowsTerminalProfiles` via the existing settings-binding pattern.

---

## Edge cases and behavior

- **No `settings.json` found, setting is on** — Profile combobox shows only `— none —`. No error UI.
- **Malformed `settings.json`** — service logs to crash log, returns `[]`. Combobox shows only `— none —`.
- **Profile references a scheme name that doesn't exist anywhere** — `ColorSchemeJson` is `null`; xterm uses its default theme. Rest of the profile (font/cursor/etc.) still applies.
- **Profile's `commandline` starts a shell we don't passthrough (e.g. `nu.exe`)** — `PseudoTerminal.BuildCmdLine` already wraps unknown exes in PowerShell. This is unchanged.
- **WSL profiles** (`commandline: "wsl.exe -d Ubuntu"`) — `wsl` is in `BuildCmdLine`'s passthrough list already; works out of the box.
- **`startingDirectory` contains `~`** — replaced with `%USERPROFILE%` before `Environment.ExpandEnvironmentVariables`.
- **`startingDirectory` contains `%USERPROFILE%\Projects`** — expanded by `Environment.ExpandEnvironmentVariables`.
- **`startingDirectory` is a UNC path or non-existent** — left as-is; the user can edit before starting. We do not validate folder existence here.
- **User picks a profile, then types over the Folder field** — typed value wins (we only auto-fill empty fields). Same rule as today's Folder/Host hooks.
- **User picks a profile, then picks `— none —`** — Command combobox reverts; folder/name are NOT cleared (user might have edited them; we don't second-guess). All override fields are nulled out.
- **Setting toggled off mid-session** — already-running sessions keep their stamped overrides. New sessions don't see the combobox.
- **Setting on, but profile combobox not used** — no overrides set, session looks identical to one created today.
- **Settings.json read while WT has it open** — Windows allows shared read; no lock contention expected. If a read fails we silently fall back to `[]`.

---

## Testing

### Unit tests (`tests/CodeShellManager.Tests`)

| Test class | Coverage |
|---|---|
| `WindowsTerminalProfileServiceTests` | settings.json discovery (use a temp dir + injected probe path); defaults inheritance; hidden filter; missing file → `[]`; malformed JSON → `[]`; multi-source label disambiguation. |
| `SchemeMapperTests` | `purple` → `magenta`; missing `cursorColor` → omitted; opacity rewrites `background` to `rgba(...)`. |
| `BuiltInTerminalSchemesTests` | Campbell exists; unknown name → null. |
| `CursorShapeMapperTests` | All six WT shapes map correctly; `emptyBox` sets `CursorBlink = false`. |
| `PaddingParserTests` | `"8"`, `"8, 12"`, `"4, 8, 4, 8"`, `""`, `"  6  ,7  "` (whitespace tolerance). |
| `CommandLineSplitterTests` | Quoted exe with spaces, unquoted exe, exe-only, internal quotes within args. |

Fixtures: small `.json` files under `tests/CodeShellManager.Tests/Fixtures/wt/` covering a happy path, a hidden-profile path, a defaults-inheritance path, and a malformed path.

### UI tests (`tests/CodeShellManager.UITests`)

One new test in a new `ProfilesTests` class:

- Toggle the setting on (via the Settings window).
- Open the New Session dialog.
- Assert `ProfileCombo` is present with at least `— none —`.
- Toggle setting off.
- Open the dialog again — assert `ProfileCombo` is absent.

We **don't** integration-test theme application via FlaUI — xterm.js renders to canvas/WebGL and FlaUI cannot read its pixels meaningfully. Bridge unit tests on payload composition cover that path instead.

### Bridge payload tests

A small `TerminalBridgeTests.SetOptionsPayloadShape` test with a fake `IPostMessage` validates that:
- All-null session → no message sent.
- Session with `FontFamily` only → message contains `fontFamily`, no other keys.
- Session with full overrides → `theme`, `fontFamily`, `cursorStyle`, `padding`, `retro` all present.

This requires extracting the bridge's post target behind a tiny seam (already small — `WebView2.CoreWebView2.PostWebMessageAsString`).

---

## Risks and trade-offs

1. **`settings.json` shape can change between WT versions.** We code defensively (every field optional, malformed → `[]`), so a breaking change at worst gives an empty combobox, never a crash.
2. **Acrylic blur gap.** Users who pick an acrylic WT profile expect blur; we deliver flat transparency over our chrome. Documented in the settings checkbox tooltip and in user-facing release notes.
3. **Retro effect gap.** Our CSS overlay won't match WT's WebGL shader. Documented the same way.
4. **Two HTML entry points add maintenance.** Mitigated by extracting `terminal-init.js` so the duplicated script logic lives once.
5. **Per-session theme persists across version updates.** Once a user stamps a profile onto a session, subsequent edits in WT won't propagate. This is a deliberate trade — it keeps existing sessions stable when the user fiddles with WT.

---

## Out of scope (explicit)

- `iconPath` rendering in the dropdown. Skipped per brainstorming.
- Live-reload when `settings.json` changes.
- Editing profiles in CodeShellManager.
- Importing key bindings, snippets, or actions.
- True acrylic blur and WebGL retro shader.
- Per-session theme editor UI in CodeShellManager itself (only profile-driven for now).

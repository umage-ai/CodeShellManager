# Session Spinners Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a per-session "starting…" spinner that lives in the xterm host until the first PTY byte arrives, and a full-window "Shutting down…" overlay during app exit.

**Architecture:** The launch spinner is a CSS overlay inside `terminal.html` / `terminal-transparent.html`, visible by default and hidden via a WebView2 message (`bootDone`) on first PTY output. The shutdown overlay is a WPF `Grid` on `MainWindow`, made visible at the top of `OnClosing` followed by a single `Dispatcher.InvokeAsync(... Background)` yield so the overlay paints before disposal blocks the UI thread.

**Tech Stack:** WPF (.NET 10) + WebView2 + xterm.js + plain CSS/JS. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-05-16-session-spinners-design.md`

---

## File Structure

| File | Responsibility |
|---|---|
| `src/CodeShellManager/Assets/terminal.html` | Boot overlay markup + CSS (opaque variant) |
| `src/CodeShellManager/Assets/terminal-transparent.html` | Boot overlay markup + CSS (transparent variant — only difference: backdrop is `#1e1e2e` here too because the spinner is opaque even when xterm is transparent) |
| `src/CodeShellManager/Assets/terminal-init.js` | Add `setBootState` and `bootDone` handlers to the existing `message` event listener |
| `src/CodeShellManager/Terminal/TerminalBridge.cs` | New `SetBootContext(label, accentHex)` API; post `setBootState` after `NavigationCompleted`; post `bootDone` on first byte from `OnPtyData`; set `CoreWebView2Controller.DefaultBackgroundColor` |
| `src/CodeShellManager/MainWindow.xaml.cs` | In `LaunchSessionAsync`: call `SetBootContext` before `InitializeAsync`, move `terminalWrapper.Visibility = Visible` earlier. In `OnClosing`: show overlay + yield |
| `src/CodeShellManager/MainWindow.xaml` | Add `ShutdownOverlay` grid as last child of the root grid |

No new test files — the work is XAML / HTML / JS / WebView2 integration and is verified manually.

---

### Task 1: Add boot overlay markup + CSS to both terminal HTML files

**Files:**
- Modify: `src/CodeShellManager/Assets/terminal.html`
- Modify: `src/CodeShellManager/Assets/terminal-transparent.html`

- [ ] **Step 1: Add boot overlay CSS + markup to `terminal.html`**

In `src/CodeShellManager/Assets/terminal.html`, **inside the `<style>` block**, append these rules just before the closing `</style>` tag (after the `body.retro::before` rule):

```css
  /* Boot overlay — visible until terminal-init.js receives bootDone */
  #bootOverlay {
    position: fixed; inset: 0;
    background: #1e1e2e;
    z-index: 200;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-direction: column;
    gap: 14px;
    font-family: 'Segoe UI', sans-serif;
    color: #cdd6f4;
    transition: opacity 200ms ease-out;
  }
  #bootOverlay.hidden { opacity: 0; pointer-events: none; }
  #bootSpinner {
    width: 44px; height: 44px;
    --boot-accent: #89b4fa;
  }
  #bootSpinner circle {
    fill: none;
    stroke: var(--boot-accent);
    stroke-width: 4;
    stroke-linecap: round;
    stroke-dasharray: 90 150;
    transform-origin: center;
    animation: bootSpin 1.2s linear infinite;
  }
  @keyframes bootSpin { to { transform: rotate(360deg); } }
  #bootLabel { font-size: 13px; opacity: 0.85; }
```

Then **in the `<body>`**, immediately after `<div id="terminal"></div>` and before `<div id="dropOverlay">…`, insert:

```html
<div id="bootOverlay">
  <svg id="bootSpinner" viewBox="0 0 50 50"><circle cx="25" cy="25" r="20"/></svg>
  <div id="bootLabel">Initializing terminal…</div>
</div>
```

- [ ] **Step 2: Add the same overlay to `terminal-transparent.html`**

Make the identical CSS + markup edit in `src/CodeShellManager/Assets/terminal-transparent.html`. Same rules, same position. The overlay uses an opaque `#1e1e2e` background even on the transparent variant — that's intentional so the spinner is readable.

- [ ] **Step 3: Visually verify by opening one of the HTML files in a browser**

Open `src/CodeShellManager/Assets/terminal.html` directly in Chrome / Edge (drag onto the address bar). The xterm content area won't render (no JS bundling needed for this check) but the boot overlay should be visible: rotating arc + "Initializing terminal…" label, centered.

Expected: spinner spins, label visible, full-page dark background.

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/Assets/terminal.html src/CodeShellManager/Assets/terminal-transparent.html
git commit -m "feat(spinner): add boot overlay markup + CSS to xterm host pages"
```

---

### Task 2: Add `setBootState` / `bootDone` message handlers to `terminal-init.js`

**Files:**
- Modify: `src/CodeShellManager/Assets/terminal-init.js` (the existing `message` event listener block around lines 48–73)

- [ ] **Step 1: Add handlers inside the existing WebView2 message listener**

Find the `window.chrome.webview.addEventListener('message', e => { … })` block. Inside the `try { const msg = JSON.parse(e.data); … }` `if`/`else if` chain — after the existing `dropOverlayClear` handler and before the closing `} catch {}` — add two new `else if` branches:

```javascript
      else if (msg.type === 'setBootState') {
        const label = document.getElementById('bootLabel');
        const spinner = document.getElementById('bootSpinner');
        if (label && typeof msg.label === 'string') label.textContent = msg.label;
        if (spinner && typeof msg.accentHex === 'string') {
          spinner.style.setProperty('--boot-accent', msg.accentHex);
        }
      }
      else if (msg.type === 'bootDone') {
        const overlay = document.getElementById('bootOverlay');
        if (overlay && !overlay.classList.contains('hidden')) {
          overlay.classList.add('hidden');
          overlay.addEventListener('transitionend', () => {
            try { overlay.parentNode && overlay.parentNode.removeChild(overlay); } catch {}
          }, { once: true });
        }
      }
```

The `transitionend` listener uses `{ once: true }` so it auto-detaches after firing. The guard `!overlay.classList.contains('hidden')` makes `bootDone` idempotent — second invocation is a no-op.

- [ ] **Step 2: Verify the JS file is syntactically valid**

```bash
node --check src/CodeShellManager/Assets/terminal-init.js
```

Expected: no output (silent success). If Node isn't installed, skip this check — the next task's build step will catch syntax errors via the WebView2 console at runtime.

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/Assets/terminal-init.js
git commit -m "feat(spinner): add setBootState/bootDone handlers in terminal-init.js"
```

---

### Task 3: Bridge — add `SetBootContext`, post `setBootState`, set WebView2 default background

**Files:**
- Modify: `src/CodeShellManager/Terminal/TerminalBridge.cs`

This task adds the API and one of the two posting paths. The second (post `bootDone` on first byte) lands in Task 4 together with the MainWindow wiring so the feature ships end-to-end in one commit.

- [ ] **Step 1: Add boot context fields and `SetBootContext` method**

In `src/CodeShellManager/Terminal/TerminalBridge.cs`, find the field declarations near the top of the class (after `_lastSize` at ~line 25 and before `_outputBuffer` at ~line 28). Add:

```csharp
    // Boot overlay — set by MainWindow before InitializeAsync; posted as setBootState after
    // navigation completes, and hidden via bootDone on the first PTY byte (see OnPtyData).
    private string? _bootLabel;
    private string? _bootAccentHex;
    private int _bootDoneFlag; // 0 = overlay still visible, 1 = bootDone already posted
```

Then, **after** the `public TerminalBridge(WebView2 webView)` constructor (~line 80), add the public API:

```csharp
    /// <summary>
    /// Sets the boot-overlay label and accent color. Must be called before
    /// <see cref="InitializeAsync"/> — the bridge posts a setBootState message to the
    /// page as soon as navigation completes.
    /// </summary>
    public void SetBootContext(string label, string accentHex)
    {
        _bootLabel = label;
        _bootAccentHex = accentHex;
    }
```

- [ ] **Step 2: Set WebView2 default background color before navigation**

Inside `InitializeAsync`, **after** `await _webView.EnsureCoreWebView2Async(env);` (~line 96) and **before** `var settings = _webView.CoreWebView2.Settings;` (~line 99), add:

```csharp
        // Match the boot overlay background so the WebView2 init flicker (the gap between
        // the control becoming visible and terminal.html rendering) is invisible.
        try { _webView.DefaultBackgroundColor = System.Drawing.Color.FromArgb(0x1e, 0x1e, 0x2e); }
        catch { }
```

Note: `WebView2.DefaultBackgroundColor` is on the WPF `WebView2` control itself (not `CoreWebView2Controller`), which is what's referenced in the spec — the WPF wrapper exposes it directly. Wrapped in try/catch because the property's availability historically varied across WebView2 SDK versions.

- [ ] **Step 3: Post `setBootState` inside `NavCompleted`**

Inside the existing `NavCompleted` local function in `InitializeAsync`, **after** `_ready = true;` and **before** the `// Flush any PTY output…` block, add:

```csharp
            // Apply boot-overlay state if MainWindow called SetBootContext before init.
            if (_bootLabel != null && _bootAccentHex != null)
            {
                var bootJson = JsonSerializer.Serialize(new
                {
                    type = "setBootState",
                    label = _bootLabel,
                    accentHex = _bootAccentHex
                });
                try { _webView.CoreWebView2?.PostWebMessageAsString(bootJson); }
                catch { }
            }
```

- [ ] **Step 4: Build the project**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
```

Expected: build succeeds with the pre-existing CS8123 warnings only. No new errors.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Terminal/TerminalBridge.cs
git commit -m "feat(spinner): add SetBootContext + post setBootState in TerminalBridge"
```

---

### Task 4: Wire `bootDone` on first PTY byte + MainWindow visibility + SetBootContext call

**Files:**
- Modify: `src/CodeShellManager/Terminal/TerminalBridge.cs`
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

This is the task that makes the launching spinner work end-to-end.

- [ ] **Step 1: Add a `PostBootDoneIfNeeded` helper in TerminalBridge**

In `src/CodeShellManager/Terminal/TerminalBridge.cs`, near the `Trace` / `Log` helpers (around line 60), add:

```csharp
    // Posts a one-shot bootDone message to the WebView2. Safe to call from any thread.
    private void PostBootDoneIfNeeded()
    {
        if (System.Threading.Interlocked.CompareExchange(ref _bootDoneFlag, 1, 0) != 0) return;
        WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
        {
            try { _webView.CoreWebView2?.PostWebMessageAsString("{\"type\":\"bootDone\"}"); }
            catch { }
        });
    }
```

- [ ] **Step 2: Call `PostBootDoneIfNeeded` from `OnPtyData`**

In the existing `OnPtyData` method (~line 180), find the line `RawOutputReceived?.Invoke(rawData);`. Immediately after it, add:

```csharp
        PostBootDoneIfNeeded();
```

Place it AFTER `RawOutputReceived` (so listeners see the raw bytes) and BEFORE the `!_ready` check (so even buffered output triggers the dismiss — once the page navigates the overlay will fade). The helper is idempotent so calling it repeatedly is safe.

- [ ] **Step 3: Call `PostBootDoneIfNeeded` defensively from `Dispose`**

In `TerminalBridge.Dispose` (~line 404), at the very start of the method body (before the `if (_pty != null)` line), add:

```csharp
        PostBootDoneIfNeeded();
```

This covers the case where a bridge is being torn down mid-launch — the page might still be alive momentarily before the WebView2 is reclaimed, and we don't want it to ship with a half-faded overlay if it somehow revives.

- [ ] **Step 4: Build to verify the bridge changes compile**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
```

Expected: build succeeds, no new errors.

- [ ] **Step 5: Call `SetBootContext` from `LaunchSessionAsync`**

Open `src/CodeShellManager/MainWindow.xaml.cs`. Find `LaunchSessionAsync` — the method that creates `SessionViewModel`, `WebView2`, `TerminalBridge` and calls `bridge.InitializeAsync(htmlPath)`.

Locate the line that calls `bridge.InitializeAsync(...)`. **Immediately before** that call, add:

```csharp
            string bootLabel = session.IsRemote
                ? $"Connecting to {session.SshHost}…"
                : $"Starting {(string.IsNullOrWhiteSpace(session.Command) ? "session" : session.Command)}…";
            bridge.SetBootContext(bootLabel, GetAccentForSession(session));
```

`GetAccentForSession` already exists in `MainWindow.xaml.cs` (used by `BuildLaunchingSidebarItem` at line 4101 — same accessor pattern).

If you cannot locate the call to `bridge.InitializeAsync` quickly: search for `InitializeAsync(htmlPath` or `InitializeAsync(html` across `MainWindow.xaml.cs`. There should be one call site in `LaunchSessionAsync`.

- [ ] **Step 6: Move `terminalWrapper.Visibility = Visibility.Visible` earlier**

Still in `LaunchSessionAsync`. Currently the wrapper is made visible at MainWindow.xaml.cs:1083 — after the PTY is attached. Cut that line (`terminalWrapper.Visibility = Visibility.Visible;` and its trailing `Log(...)` line if there is one) from its current location, and paste it immediately after the line that adds the wrapper to `TerminalGrid.Children`.

The exact target location: search for `TerminalGrid.Children.Add(terminalWrapper)` — set visibility on the next line. This makes the spinner visible from the moment the WebView2 host is in the layout, instead of after PTY spawn.

If there's both a `Log("terminalWrapper visible, …")` line and the `Visibility = Visible` assignment, keep the Log near the original position (it'll fire after a successful PTY attach, which is still a meaningful event) but ensure the `Visibility = Visible` itself is moved to right after the `Children.Add`.

- [ ] **Step 7: Build + run the app**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
dotnet run --project src/CodeShellManager/CodeShellManager.csproj
```

In the app:
1. Click ＋ New Session. Pick any local command (e.g. `pwsh`).
2. Observe: the new terminal pane should briefly show a rotating arc + "Starting pwsh…" label, then fade out as the prompt appears.
3. Try creating an SSH session if you have access to one — label should read "Connecting to {host}…".
4. Close the app for now (don't worry about the shutdown overlay yet — that lands in Task 6).

If the spinner never appears: the wrapper visibility move didn't take. Re-check Step 6.
If the spinner never disappears: `bootDone` isn't being posted. Check `OnPtyData` for the new line + verify `terminal-init.js` was saved with the handler.
If the label is always "Initializing terminal…": `SetBootContext` isn't being called or the page navigates faster than the message ships — verify the call site in `LaunchSessionAsync` is before `InitializeAsync`.

- [ ] **Step 8: Commit**

```bash
git add src/CodeShellManager/Terminal/TerminalBridge.cs src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat(spinner): show launching overlay until first PTY byte"
```

---

### Task 5: Add `ShutdownOverlay` grid to `MainWindow.xaml`

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml`

- [ ] **Step 1: Find the root Grid's closing tag**

Open `src/CodeShellManager/MainWindow.xaml`. The root content of the `<Window>` is a Grid (or DockPanel containing a Grid). Find the **last** `</Grid>` before `</Window>` — the outermost layout container's closing tag.

If the layout is nested (e.g. DockPanel wrapping a Grid wrapping more grids), the shutdown overlay should be a sibling at the topmost level so it can cover the entire window. The simplest target: if the root is a `<Grid>` containing all UI, add this as the last child of that Grid (siblings render on top in Z-order).

If you can't find a single root Grid, the safest fallback is to wrap the existing root in a new Grid with two children: the existing content and the `ShutdownOverlay`. Do this only if no clearer target exists — the file is unfamiliar.

- [ ] **Step 2: Insert the overlay XAML**

As the last child of the root Grid (or new outer Grid per fallback in Step 1), add:

```xml
        <Grid x:Name="ShutdownOverlay" Visibility="Collapsed" Background="#cc1e1e2e" Panel.ZIndex="100">
            <StackPanel HorizontalAlignment="Center" VerticalAlignment="Center" Orientation="Vertical">
                <Grid Width="48" Height="48">
                    <Path Stroke="#89b4fa" StrokeThickness="4" StrokeStartLineCap="Round" StrokeEndLineCap="Round"
                          Data="M 24 4 A 20 20 0 1 1 4 24"
                          RenderTransformOrigin="0.5,0.5">
                        <Path.RenderTransform>
                            <RotateTransform x:Name="ShutdownSpinnerRotate" Angle="0"/>
                        </Path.RenderTransform>
                        <Path.Triggers>
                            <EventTrigger RoutedEvent="Path.Loaded">
                                <BeginStoryboard>
                                    <Storyboard RepeatBehavior="Forever">
                                        <DoubleAnimation Storyboard.TargetName="ShutdownSpinnerRotate"
                                                         Storyboard.TargetProperty="Angle"
                                                         From="0" To="360" Duration="0:0:1.2"/>
                                    </Storyboard>
                                </BeginStoryboard>
                            </EventTrigger>
                        </Path.Triggers>
                    </Path>
                </Grid>
                <TextBlock Text="Shutting down…" Foreground="#cdd6f4" FontFamily="Segoe UI" FontSize="13"
                           Margin="0,14,0,0" HorizontalAlignment="Center"/>
            </StackPanel>
        </Grid>
```

Notes on the XAML:
- `Background="#cc1e1e2e"` is `#1e1e2e` at 80% alpha — the existing UI shows through at low contrast.
- `Panel.ZIndex="100"` is belt-and-braces; the overlay being the last child of the root Grid already puts it on top.
- The `Storyboard` starts once when the Path is first loaded into the visual tree (at window startup) and runs for the lifetime of the window. A single `DoubleAnimation` on a rotation transform is cheap (<0.1% CPU) so we don't bother gating it on visibility — the simpler XAML is worth more than the marginal saving.

- [ ] **Step 3: Build to confirm the XAML is valid**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
```

Expected: build succeeds. If XAML parsing fails, the compiler will pinpoint the line.

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml
git commit -m "feat(spinner): add ShutdownOverlay grid to MainWindow.xaml"
```

---

### Task 6: Wire shutdown overlay into `OnClosing`

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml.cs` (the existing `OnClosing` override at ~line 4789)

- [ ] **Step 1: Show overlay and yield once**

Open `src/CodeShellManager/MainWindow.xaml.cs`. Find `protected override async void OnClosing(...)` (~line 4789).

After the existing `_isShuttingDown = true;` line (~line 4804) and **before** `_windowStateTimer.Stop();` (~line 4806), insert:

```csharp
        // Show the shutdown overlay so the user sees progress while sessions tear down.
        // The yield lets WPF render the overlay before the synchronous disposal below blocks
        // the UI thread; without it, the overlay would only paint after Close() is reached.
        ShutdownOverlay.Visibility = Visibility.Visible;
        await Dispatcher.InvokeAsync(() => { },
            System.Windows.Threading.DispatcherPriority.Background);
```

The yield uses `DispatcherPriority.Background` (lower than `Render`), which guarantees a render pass completes before the continuation runs.

- [ ] **Step 2: Build**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
```

Expected: build succeeds.

- [ ] **Step 3: Manual verification**

```bash
dotnet run --project src/CodeShellManager/CodeShellManager.csproj
```

1. Open the app, create at least one session (more sessions = longer shutdown, more visible overlay).
2. If you have Claude installed, open a Claude session too — it has a 10-second-per-session dispose path which makes the overlay shine.
3. Close the window.
4. Expected: the existing UI dims behind the `Shutting down…` overlay, the arc spins, and after sessions finish disposing the window actually closes.

If the overlay never appears: the yield didn't fire before disposal started. Try increasing the priority to `ContextIdle` (lowest) or verify the XAML naming.
If the overlay flashes too briefly to see: that's actually fine — it means shutdown was fast. Re-test with more sessions or a Claude session.

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat(spinner): show ShutdownOverlay during OnClosing teardown"
```

---

### Task 7: Final smoke test + run full test suite

**Files:** None modified.

- [ ] **Step 1: Run the full unit test suite**

```bash
dotnet test tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj
```

Expected: all 206 tests pass (same as before the feature — no test changes were made).

- [ ] **Step 2: End-to-end visual verification**

```bash
dotnet run --project src/CodeShellManager/CodeShellManager.csproj
```

Checklist:
- [ ] New local session shows "Starting {command}…" spinner that fades out as the prompt arrives
- [ ] New SSH session (if available) shows "Connecting to {host}…" spinner
- [ ] Restored sessions on app launch each show their own spinner briefly
- [ ] Failed launch (e.g. fake a typo'd command) removes the wrapper cleanly — no orphan spinner
- [ ] Closing the app with sessions live shows "Shutting down…" overlay
- [ ] Spinner arc color matches the session's accent stripe

- [ ] **Step 3: If everything checks out, no commit needed**

This task is verification only. If any item fails, return to the relevant earlier task and adjust.

---

## Out of scope (per spec)

- Per-session sleep / wake / close spinners (only app-exit gets a spinner)
- UI tests for the spinner (XAML/WebView2 makes this flaky; spec accepts manual verification)
- Configurable spinner appearance (one accent-colored arc style everywhere)
- Telemetry on how long launches actually take

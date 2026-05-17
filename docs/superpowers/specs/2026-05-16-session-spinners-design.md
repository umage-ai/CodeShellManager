# Session Spinners — Design Spec

**Date:** 2026-05-16
**Status:** Approved
**Scope:** Per-session "starting" spinner inside the xterm host, and an app-exit "shutting down" overlay on the main window. No per-session close/sleep spinner.

---

## Context

When a session launches today, the terminal pane is invisible until the PTY has been spawned and `terminalWrapper.Visibility = Visible` is set at `MainWindow.xaml.cs:1083`. The sidebar shows a "launching…" placeholder (`_launchingSidebarItems`, `BuildLaunchingSidebarItem` at `MainWindow.xaml.cs:4099`), but the main terminal area gives no feedback. WebView2 init, navigation to `Assets/terminal.html`, xterm.js mount, and PTY spawn collectively take long enough — especially for SSH sessions waiting on host handshake — that users wonder whether anything is happening.

App shutdown has a similar gap. With many live sessions, the WebView2 and PTY disposals serialize and can take a couple of seconds. The window currently freezes during that window with no indication of progress.

This spec adds two small visual layers to cover both cases.

---

## Goals

- Show a centered spinner + phase-appropriate text in the terminal pane from the moment a session begins launching until the first byte of PTY output arrives.
- Show a full-window "Shutting down…" overlay during app exit while sessions are being torn down.
- Style: accent-colored rotating arc, matches the per-session accent color from `ColorService`.
- Hide automatically — no manual dismiss.

## Non-goals

- Sleep / wake / per-session close spinners. The dispose paths for those are fast enough that adding feedback would be more code than benefit.
- A spinner on the sidebar entry. The existing `_launchingSidebarItems` placeholder already covers that surface and is not changing.
- Progress *quantification* (e.g. "step 2 of 4"). The spinner is qualitative; only the label string changes between phases.
- Surfacing the spinner inside run-command chips. Those have their own status UI.

---

## User flow

**Launching a session:**

1. User picks ＋ New Session, fills in the dialog, hits OK. (Or session is restored on startup.)
2. The terminal wrapper becomes visible immediately (no longer waiting until after PTY spawn).
3. The wrapper shows a centered rotating arc in the session's accent color, with a phase label below:
   - Local sessions: `Starting {command}…`
   - SSH sessions: `Connecting to {host}…`
   - During WebView2 init before the bridge has wired up: `Initializing terminal…` (default in HTML)
4. As soon as the first byte of PTY output arrives, the overlay fades out (200ms) and the terminal content takes over.
5. On launch failure, the existing catch block in `LaunchSessionAsync` already removes the terminal wrapper and shows a modal `MessageBox`. The spinner simply disappears with the wrapper — no separate error UI in the overlay. (Trade-off: a hung WebView2 init or SSH connect just spins forever; user must use the toolbar ✕ to bail out. Acceptable for v1; can be revisited if it happens in practice.)

**App exit:**

1. User clicks the window close button (or Alt+F4, etc.).
2. A full-window overlay fades in (semi-transparent dark backdrop over the existing UI, centered spinner + `Shutting down…` label).
3. Session disposals run on a background task.
4. Once teardown completes, the window actually closes.

---

## Architecture

### Per-session launch spinner (HTML + JS)

The spinner lives inside the xterm host HTML, not as a WPF overlay. This means it disappears the instant `terminal-init.js` sees the first PTY data, with no cross-tier coordination needed.

**`Assets/terminal.html` and `Assets/terminal-transparent.html`:**

Both files get a sibling div alongside the existing xterm container:

```html
<div id="boot-overlay" class="boot-overlay">
  <svg class="boot-spinner" viewBox="0 0 50 50">
    <circle class="boot-arc" cx="25" cy="25" r="20" />
  </svg>
  <div id="boot-label" class="boot-label">Initializing terminal…</div>
</div>
```

CSS lives in the same files (small enough not to warrant a separate stylesheet):

- `.boot-overlay` — absolutely positioned, fills the WebView2 viewport, `background: #1e1e2e`, centered flex column, transitions `opacity 200ms`.
- `.boot-spinner` — 48px square, rotates via `@keyframes spin` (1.2s linear infinite).
- `.boot-arc` — stroked SVG arc (`stroke-dasharray`, `stroke-linecap: round`), default `stroke: #89b4fa`; overridable from JS by setting a CSS variable.
- `.boot-label` — Catppuccin foreground (`#cdd6f4`), muted weight.
- A `.boot-overlay.hidden` modifier sets `opacity: 0; pointer-events: none`, and a `transitionend` handler removes the node from the DOM.
- `.boot-overlay.error` swaps the spinning arc for a static "!" glyph and stops the animation.

**`Assets/terminal-init.js`:**

Add three handlers on the existing WebView2 message channel:

| Message | Payload | Effect |
|---|---|---|
| `setBootState` | `{ label: string, accentHex: string }` | Updates `#boot-label` text and the spinner CSS variable. |
| `bootDone` | (none) | Adds `.hidden`; on `transitionend` removes the node. Idempotent — calling twice is safe. |

### Per-session launch spinner (C# bridge)

**`Terminal/TerminalBridge.cs`:**

- Right after the WebView2 navigates to `terminal.html` and the bridge's `WebMessageReceived` is wired, post a `setBootState` message with the session's accent + phase label. The label is computed from `ShellSession.IsRemote` and the command line.
- On the first invocation of `RawOutputReceived` for that session — track via a `bool _bootDone` field on the bridge — post `bootDone`.
- Also post `bootDone` from `Dispose` defensively so a wrapper that's torn down mid-launch doesn't ship a half-faded overlay if it's somehow revived.
- Ensure the WebView2's default background color is set to `#1e1e2e` before navigation (set on the `CoreWebView2Controller.DefaultBackgroundColor` in the existing WebView2 init code). This hides the gap between WebView2 becoming visible and `terminal.html` rendering.

**`MainWindow.xaml.cs`:**

- Move `terminalWrapper.Visibility = Visibility.Visible` from after the PTY-attach (~line 1083) to immediately after the wrapper is built. This makes the spinner visible during the full launch window. The existing catch block continues to remove the wrapper on PTY-start failure — no new error-UI logic needed.

### App-exit overlay (WPF)

**`MainWindow.xaml`:**

A new `Grid x:Name="ShutdownOverlay"` at the end of the root grid (z-order on top), default `Visibility="Collapsed"`. Contents:

- Full-bleed `Rectangle` with `Fill="#cc1e1e2e"` (80% alpha over existing UI).
- Centered `StackPanel` with:
  - A `Path` drawing the same arc shape as the HTML spinner, with a `Storyboard` rotating it 360° linear infinite. The storyboard is started in `Loaded` and stopped on `Unloaded` to avoid CPU when not visible.
  - A `TextBlock` with `Shutting down…` in Catppuccin foreground.

**`MainWindow.xaml.cs`:**

Override `OnClosing`. WebView2 and PTY disposal must run on the UI thread, so we don't `Task.Run` the teardown — we yield once at `Background` priority to let the overlay paint, then dispose synchronously. The UI is still frozen during disposal, but the user now sees a "Shutting down…" overlay instead of a hung window.

```
private bool _shutdownInProgress;

protected override async void OnClosing(CancelEventArgs e)
{
    if (_shutdownInProgress) return; // second pass: let base.OnClosing fall through naturally
    e.Cancel = true;
    _shutdownInProgress = true;

    ShutdownOverlay.Visibility = Visibility.Visible;

    // Yield once so the overlay actually paints before disposal blocks the UI thread.
    await Dispatcher.InvokeAsync(() => { }, DispatcherPriority.Background);

    DisposeAllSessions(); // existing teardown extracted; runs on the UI thread.
    Close();              // re-enters OnClosing with _shutdownInProgress=true.
}
```

`DisposeAllSessions` is the existing teardown logic extracted from current shutdown paths — no behavior change. The yield-then-dispose pattern is enough to guarantee the overlay paints; we don't need true async disposal.

---

## Edge cases

- **PTY launch fails:** Existing catch block removes the wrapper (and shows a `MessageBox`); the spinner disappears with the wrapper. No new error UI.
- **WebView2 init flicker:** ~50–200ms before `terminal.html` renders. We set `CoreWebView2Controller.DefaultBackgroundColor` to `#1e1e2e` before navigation so the gap blends with the spinner overlay.
- **Session restored on startup, multiple panes:** Each pane has its own independent overlay since each has its own bridge + WebView2. No coordination needed.
- **SSH connection hangs forever:** Spinner spins forever. Existing close ✕ on the terminal toolbar still works since it's outside the WebView2.
- **First PTY byte is ANSI clear-screen:** Still counts as "first output" — overlay hides. Acceptable; matches the user's mental model of "something happened."
- **Re-entrant `OnClosing`:** Guarded by `_shutdownInProgress` flag; second pass falls through to base.
- **App-exit while a session is mid-launch:** The launching wrapper's spinner becomes irrelevant once the shutdown overlay is on top. Disposal of the half-launched session works the same as today.

---

## Testing

Most of the new code is XAML / HTML / JS and is not unit-testable headless. We rely on existing UI tests in `tests/CodeShellManager.UITests/` for smoke coverage:

- A UI test in `SessionTests.cs` can assert the boot overlay element is visible briefly after creating a new session, then disappears once the prompt is on screen. FlaUI cannot directly query the WebView2 DOM, but it can assert the terminal wrapper is visible from t=0 (current behavior would have it invisible).
- Shutdown overlay: add a UI test that closes the window with N sessions live and verifies the overlay element is present during teardown. May be flaky if teardown is faster than the polling interval — keep the assertion loose.

No new unit tests planned. The C# changes in `TerminalBridge` are too tightly coupled to WebView2 messaging to test headlessly without a refactor we don't otherwise need.

---

## Out of scope (revisit later if asked)

- A WPF-spinner pre-stage that hands off to the HTML spinner. Would eliminate the WebView2 init flicker, but the matched background color makes the gap invisible and it's not worth the extra coordination code.
- Per-session sleep/close spinners.
- Progress text more granular than the four phases listed above.
- Spinner shape variations (we use one rotating arc everywhere).

---

## File touch list

- `src/CodeShellManager/Assets/terminal.html` — overlay markup + CSS.
- `src/CodeShellManager/Assets/terminal-transparent.html` — same.
- `src/CodeShellManager/Assets/terminal-init.js` — `setBootState` / `bootDone` / `bootError` handlers.
- `src/CodeShellManager/Terminal/TerminalBridge.cs` — post `setBootState` after navigation, `bootDone` on first PTY byte + defensively from `Dispose`; set WebView2 default background color before navigation.
- `src/CodeShellManager/MainWindow.xaml` — `ShutdownOverlay` grid.
- `src/CodeShellManager/MainWindow.xaml.cs` — `OnClosing` override + `DisposeAllSessions` extraction; move `terminalWrapper.Visibility = Visible` earlier in `LaunchSessionAsync`.
- `tests/CodeShellManager.UITests/SessionTests.cs` — smoke tests for both overlays (best-effort).

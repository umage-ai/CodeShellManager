# Cross-Platform Spike

Throwaway proof for [issue #32](https://github.com/umage-ai/CodeShellManager/issues/32) Phase 1:
**Avalonia 12 + NativeWebView (xterm.js) + Porta.Pty** as the cross-platform stack.
Spec: `docs/superpowers/specs/2026-06-10-cross-platform-spike-design.md`.

Run: `dotnet run --project spikes/CrossPlatformSpike/CrossPlatformSpike.csproj`

Linux needs WPE WebKit runtime libs: `libwpewebkit-2.0`, `libwpe-1.0`, `libWPEBackend-fdo-1.0`
(Debian/Ubuntu: `sudo apt install libwpewebkit-2.0-1 libwpe-1.0-1 libwpebackend-fdo-1.0-1` — exact
package names vary by distro/version; record what actually worked below.)

## Success criteria

| # | Criterion | Windows | WSLg (Linux) | macOS |
|---|---|---|---|---|
| 1 | Window opens, xterm.js renders | ✅ | ☐ | deferred |
| 2 | Interactive shell (typing, arrows, Ctrl+C) | ✅ (typing; arrows/Ctrl+C untested) | ☐ | deferred |
| 3 | ANSI colors + cursor addressing (TUI) | ✅ (colored dir listing; full TUI untested) | ☐ | deferred |
| 4 | Resize propagates to the PTY | ☐ (not tested) | ☐ | deferred |
| 5 | CI build green | ✅ | ✅ | ✅ |
| 6 | Type-echo latency acceptable | ✅ (no perceptible latency at interactive volume) | ☐ | deferred |

**Footnotes:**
- **#2:** Keyboard input via `SendKeys` was validated through a full `dir` round-trip. Arrow keys and Ctrl+C were not explicitly tested; their behaviour depends on PTY raw-mode pass-through which was not exercised.
- **#3:** ANSI color sequences rendered correctly in a PowerShell directory listing. No full TUI application (e.g., `htop`, `vim`, or `btm`) was run; cursor-addressing beyond a normal prompt has not been verified on Windows.
- **#4:** Initial terminal size was correctly reported as 119x38 (fit-on-open worked), but no deliberate window resize was performed during validation. Resize propagation to the PTY is therefore untested.
- **#6:** No latency was perceptible at interactive/dir-burst volume. High-throughput streaming (e.g., `cat` of a large file) was not benchmarked.

## Findings

### 2026-06-11 — Windows validation (Tasks 1–3)

**Porta.Pty 1.0.7 API surface.** The library's public API matched its README exactly: `PtyProvider.SpawnAsync`, `IPtyConnection` with `ReaderStream`/`WriterStream`/`Pid`/`ExitCode`/`ProcessExited`/`Kill`/`Resize`. One gotcha: `PtyOptions.Cwd` must be a non-empty string — passing `null` or `""` causes `SpawnAsync` to throw at runtime.

**NativeWebView integration.** Package `Avalonia.Controls.WebView 12.0.1` (MIT). The bare `<NativeWebView/>` tag resolves via its `XmlnsDefinition`; no extra namespace import needed. `Navigate(Uri)` is safe to call before the platform adapter is ready — it queues internally and fires once the adapter attaches (call from `Window.Opened` is fine). `InvokeScript(string)` returns `Task<string?>`. `WebMessageReceivedEventArgs.Body` is `string?`.

**WebMessageReceived is adapter-lazy.** Handlers subscribed before the adapter exists are re-attached automatically. The page's early `'ready'` postMessage is not dropped even if the subscription was set up before navigation completed.

**WebView2 adapter script injection.** On Windows, the WebView2 adapter injects `invokeCSharpAction` via `AddScriptToExecuteOnDocumentCreated`, wired to `window.chrome.webview.postMessage`. The WPE (Linux) and WKWebView (macOS) equivalents have not yet been confirmed — this needs verification in Task 6 and any macOS follow-up.

**CRITICAL — app.manifest required.** An `app.manifest` containing the Windows 10/11 `supportedOS` GUID is **mandatory**. Without it, `Win32NativeControlHost` throws `"Unable to create child window for native control host"` when `NativeWebView` tries to attach its HWND. Avalonia scaffold templates do not include this file; it must be added manually. This is the most likely blocker for anyone trying to reproduce the spike from scratch.

**Initial resize race did not occur.** The 0×0 initial-size race that the main CodeShellManager app guards against (with a delayed re-fit) was not observed here — `NativeWebView` reported 119×38 correctly on first fit. This may be app-manifest or DPI-awareness related; worth noting in case it resurfaces on Linux.

**Automation focus quirk (not user-facing).** `SendKeys` only reaches the xterm.js content after a mouse click inside the WebView child HWND. Calling `SetForegroundWindow` on the top-level Avalonia window alone does not give keyboard focus to the embedded WebView. This is irrelevant for real users but needs attention if automated UI testing is planned in Phase 2.

**Phase 2 must-fix patterns (identified in code review; acceptable for the spike — do not copy into production code):**
- `async void` ready handler: a page reload would spawn a second PTY with no re-entry guard.
- Concurrent `WriterStream.WriteAsync` calls are not serialized — interleaving is possible under rapid input.
- Disposal relies on a swallow-all `catch` around `InvokeScript` rather than a proper shutdown gate.
- `InvokeScript` is called once per 4 KB output chunk; heavy output should batch/coalesce calls to reduce round-trip overhead.

### 2026-06-11 — CI matrix

All three matrix legs built green on the first attempt with no source changes: ubuntu-latest (24s), macos-latest (35s), windows-latest (1m10s). Run: https://github.com/umage-ai/CodeShellManager/actions/runs/27333797882. The only notable CI annotation was an informational notice that `windows-latest` requests will be redirected to `windows-2025-vs2026` by 15 June 2026 — this is a GitHub runner housekeeping change, not a build issue. No restore warnings on Linux; all three platforms resolved NuGet packages and compiled without modification.

## Verdict

<!-- Go / no-go recommendation for Phase 2, written when validation is done.
     Mirror it as a comment on issue #32. -->

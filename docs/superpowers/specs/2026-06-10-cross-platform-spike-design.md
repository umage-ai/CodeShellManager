# Cross-Platform Spike — Design

**Date:** 2026-06-10
**Issue:** [#32 — Cross-platform support: target macOS and Linux](https://github.com/umage-ai/CodeShellManager/issues/32)
**Branch:** `feat/cross-platform-spike`
**Phase:** 1 of 5 (spike) per the issue's suggested phasing.

## Goal

Answer one question: **does Avalonia 11 + NativeWebView + Porta.Pty give us a working
interactive terminal on non-Windows platforms?**

No feature parity. No search, alerts, persistence, groups, or run commands. The spike is
throwaway by design, but the PTY wrapper and JS bridge are structured so they can seed the
real port (Phase 2) if the stack proves out.

## Research findings that update issue #32

The issue was written 2026-05-12; two assumptions have changed:

1. **Avalonia now ships a first-party, free WebView** — the `Avalonia.Controls.WebView`
   NuGet package provides `NativeWebView`: WebView2 on Windows, WKWebView on macOS,
   WPE WebKit on Linux (WebKitGTK fallback). This supersedes the issue's recommendation
   to spike the third-party community `Avalonia.WebView` wrapper, which is now the
   *fallback*, not the primary candidate.
2. **microsoft/vs-pty.net (Pty.Net) is not on nuget.org** — it is source-only. The
   primary PTY candidate is **Porta.Pty** (nuget.org, ~41k downloads), which bundles a
   native `forkpty()`+`execvp()` shim per platform (linux-x64/arm64, osx-x64/arm64).
   The native shim matters: .NET 7+ cannot safely call `forkpty` from managed code
   (fork in a threaded runtime is hazardous), so pure-P/Invoke approaches are out.
3. **Avalonia is now at v12** (12.0.x on nuget.org); the spike targets 12, not the 11.x
   the issue mentions.

## Licensing constraint (hard requirement)

The product must stay **100% free and open-source, including all dependencies**.
"Free to use" is not sufficient — every package must carry an OSI-approved license.
Verified against nuget.org license metadata (2026-06-10):

| Package | License |
|---|---|
| `Avalonia` 12.0.x | MIT |
| `Avalonia.Controls.WebView` 12.0.x | MIT (confirmed — *not* an Accelerate/EULA license) |
| `Porta.Pty` 1.0.x | MIT |
| xterm.js + fit addon (vendored) | MIT |
| Linux runtime: WPE WebKit / WebKitGTK | BSD/LGPL (system packages, not shipped) |

Any dependency added later (spike or port) must be license-checked against this bar
before adoption. Fallback candidates are pre-checked in the fallback ladder below.

## Alternatives considered (UI layer)

- **Uno Platform** (Apache 2.0) — disqualified: no embedded WebView support on its
  macOS/Linux Skia desktop targets, and this app is "N web views in a grid".
- **.NET MAUI** (MIT) — disqualified: no Linux target.
- **Photino.Blazor** (Apache 2.0) — credible architectural alternative: whole UI as web
  content in a single native web view, terminal panes as xterm.js divs (the VS Code
  model). Rejected as primary because it means a full UI rewrite in Blazor/HTML rather
  than a WPF→Avalonia port, and the project's maintainer investment is smaller — but it
  is the recognized fallback **if Avalonia's multi-WebView embedding fails the spike**.
- **Eto.Forms** (BSD) — aging, dated web view support; not competitive.

## Validation constraints

Development happens on Windows. For this phase:

- **Windows** — full manual validation locally.
- **Linux** — manual validation via WSLg.
- **macOS** — CI build only; manual smoke test deferred until Mac hardware is available.
- **CI** — build-only matrix across `windows-latest` / `macos-latest` / `ubuntu-latest`.

## Project structure

```
spikes/
└── CrossPlatformSpike/
    ├── CrossPlatformSpike.csproj   # net10.0 (NOT net10.0-windows), self-contained
    ├── Program.cs                  # Avalonia bootstrap
    ├── App.axaml / App.axaml.cs
    ├── MainWindow.axaml / .cs      # NativeWebView + status bar
    ├── SpikePty.cs                 # Porta.Pty wrapper
    ├── SpikeBridge.cs              # PTY ↔ xterm.js message routing
    ├── Assets/
    │   ├── terminal.html           # stripped-down host page (no spinner/profile machinery)
    │   ├── xterm.js / xterm.css    # copied from src/CodeShellManager/Assets
    │   └── xterm-addon-fit.js
    └── README.md                   # success-criteria checklist, findings log
```

Not referenced by `CodeShellManager.slnx`. Easy to delete or mine later.

**Packages:** `Avalonia` 12.x, `Avalonia.Desktop`, `Avalonia.Themes.Fluent`,
`Avalonia.Controls.WebView`, `Porta.Pty` — all MIT (see licensing constraint).

## Components

### MainWindow

A single `NativeWebView` filling the window, plus a thin status bar showing PTY state
(shell path, pid, running/exited + exit code). The status bar exists so failures are
visible without attaching a debugger — if the web view renders but the PTY died, or
vice versa, the screen says so.

### SpikePty

Thin wrapper over Porta.Pty exposing the minimum surface:

- `Start(cols, rows)` — spawns the platform default shell:
  - Unix: `$SHELL`, fallback `bash`
  - Windows: `pwsh` if on PATH, fallback `cmd`
- `DataReceived` event (raw bytes/strings from the PTY)
- `Write(string data)` — user keystrokes to the PTY
- `Resize(int cols, int rows)`
- `Exited` event with exit code

Mirrors the shape of the existing `IPseudoTerminal` so a Phase-2 port can slot it in
behind the same interface.

### SpikeBridge

The `TerminalBridge` equivalent for NativeWebView:

- PTY `DataReceived` → JS `term.write(...)`
- xterm `onData` (keystrokes) → `SpikePty.Write`
- xterm `onResize` (fit addon) → `SpikePty.Resize`

**The message channel is itself a spike subject.** NativeWebView's JS interop API is not
WebView2's `PostWebMessage`/`WebMessageReceived`. Part of the spike's job is finding the
supported bidirectional channel (script invocation, navigation interception, custom
scheme, or whatever the component offers) and recording how it works — and whether it is
fast enough for terminal traffic — in the README findings log.

## Success criteria

Recorded as a checklist in `spikes/CrossPlatformSpike/README.md`, each marked
pass/fail per platform as validation happens:

1. Window opens and xterm.js renders — Windows & WSLg.
2. Interactive shell works: typing, arrow keys (history), Ctrl+C reach the PTY.
3. ANSI colors and cursor addressing render correctly (`ls --color`, a TUI such as
   `claude` or `htop`).
4. Resize propagates: dragging the window changes the shell's reported `COLUMNS`/`LINES`.
5. CI build matrix is green on all three OSes.
6. Type-to-echo latency feels acceptable (subjective judgment, noted in findings).

The spike *succeeds* when criteria 1–5 pass on Windows + WSLg and the findings log has
enough detail to commit to the Phase-2 stack. It *fails usefully* if a layer is
unfixable — in which case the findings log says which fallback to try next.

## Fallback ladder

Each layer swaps independently; the rest of the spike survives:

| Layer | Primary | Fallback 1 | Fallback 2 |
|---|---|---|---|
| Web view | `Avalonia.Controls.WebView` (NativeWebView, MIT) | community `WebView.Avalonia` (MIT) | CefGlue-based `WebViewControl-Avalonia` (heavy, ~100MB) |
| PTY | `Porta.Pty` (MIT) | `Quick.PtyNet` | vendor `microsoft/vs-pty.net` source (MIT) |
| UI shell | Avalonia 12 (MIT) | Photino.Blazor (Apache 2.0) — architecture change, see alternatives | — |

Fallback licenses must be re-verified at adoption time (licensing constraint above).

## CI

New workflow `.github/workflows/spike-crossplatform.yml`:

- Triggers: `workflow_dispatch` + pushes touching `spikes/CrossPlatformSpike/**`.
- Matrix: `windows-latest`, `macos-latest`, `ubuntu-latest`.
- Steps: checkout, setup .NET 10, `dotnet build spikes/CrossPlatformSpike/`.
- Build-only — no packaging, no signing, no release-pipeline changes.

## Out of scope

- macOS manual validation (hardware not available; CI build only).
- Transparency, fonts, Windows Terminal profiles, theming.
- Porting any real app code (Phase 2).
- Packaging/installers (Phase 5).
- Performance benchmarking beyond the subjective latency check.

## Exit

The spike ends with a findings log in the README and a go/no-go recommendation for
Phase 2 (full Avalonia port on Windows) added as a comment on issue #32.

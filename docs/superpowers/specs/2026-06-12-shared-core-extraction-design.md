# Shared Core Extraction (Phase 2a) — Design

**Date:** 2026-06-12
**Issue:** [#32 — Cross-platform support](https://github.com/umage-ai/CodeShellManager/issues/32)
**Branch:** `cross-platform` (work on `feat/2a-shared-core` off it, merge back when green)
**Phase:** 2a — first sub-project of Phase 2 (Avalonia port with feature parity, Windows-only target).

## Context

Phase 1 (spike, `spikes/CrossPlatformSpike/`) proved Avalonia 12 + NativeWebView + a PTY
behind `IPseudoTerminal` works on Windows. Phase 2 ports the app side-by-side: a new
Avalonia app will coexist with the shipping WPF app, both consuming one shared core, with
cutover only when parity is proven. The WPF app and its release pipeline stay shippable
throughout.

Phase 2 decomposition (each sub-project gets its own spec → plan → implementation):

| Sub-project | Scope |
|---|---|
| **2a (this spec)** | Extract `CodeShellManager.Core` class library |
| 2b | Avalonia skeleton + terminal stack (one working session) |
| 2c | Session management UI (sidebar, lifecycle, groups, sleep/wake, layouts) |
| 2d | Dialogs + features (NewSession/Settings/RunCommands, search, notes, alerts, tray, shortcuts) |
| 2e | Parity audit against CLAUDE.md's feature list |

## Goal

A `net10.0` class library `src/CodeShellManager.Core/` holding everything
platform-agnostic. The WPF app references it and behaves identically; all 206 unit tests
pass. **Zero behavior change** — this is a pure extraction with three small seams.

A coupling audit (2026-06-12) classified the existing code: 25 of 32 files under
Models/Services/ViewModels/Terminal are already WPF-free; 4 need small abstractions;
3 are platform-bound and stay behind.

## What moves to Core (verbatim, namespaces unchanged)

Keeping `CodeShellManager.*` namespaces makes the WPF-side diff just file deletions plus
a project reference.

- **Models/** — all 7 files (AlertEvent, AppState, RecentlyClosedEntry, RunCommandItem,
  SessionGroup, ShellSession, WindowsTerminalProfile).
- **Services/** — all except `ToastHelper.cs`: SessionManager, StateService,
  SearchService, GitService, AlertDetector, CommandPresetsService, ClaudeSessionService,
  UpdateService, ImportExportService, SessionRunner, RunInstance,
  RunCommandTemplatesService, WindowsTerminalProfileService, BuiltInTerminalSchemes,
  SchemeMapper, CursorShapeMapper, PaddingParser, CommandLineSplitter — plus
  `ColorService` minus its WPF brush members (see seams).
- **Terminal/** — `IPseudoTerminal.cs`, `OutputIndexer.cs`, and `PseudoTerminal.cs`.
  PseudoTerminal is Windows-*runtime*-bound (ConPTY P/Invoke) but pure BCL — no WPF.
  The Avalonia app needs it on Windows too, so Core is its home. It compiles on all
  platforms; callers only instantiate it on Windows.
- **ViewModels/** — `MainViewModel.cs`, `SessionViewModel.cs` (with the seams below).

**Core package references:** `CommunityToolkit.Mvvm`, `Microsoft.Data.Sqlite` — both
already used, both MIT (per the OSS-only dependency constraint; any new dependency must
be license-checked).

## The three seams (new code, all in Core)

1. **`IDispatcher`** — `void Post(Action action)` (and, only if call sites require a
   synchronous variant, `void Invoke(Action action)`). Replaces `MainViewModel`'s four
   `App.Current.Dispatcher.Invoke(...)` calls; injected via constructor. The WPF app
   provides `WpfDispatcher` (a ~5-line adapter over `Application.Current.Dispatcher`).
2. **`ITerminalBridge`** — minimal interface covering exactly what `SessionViewModel`
   calls on `TerminalBridge` (enumerated at plan time from actual usage; expected to be
   `Dispose` plus a small number of members). `TerminalBridge` stays in the WPF app and
   implements it. `SessionViewModel.Bridge` becomes `ITerminalBridge?`.
3. **`ColorService` split** — the FNV-1a hash → hex-string logic (including the 12-color
   palette as hex strings) lives in Core. The `Color`/`SolidColorBrush` conversions move
   to a small static helper in the WPF app (e.g. `Util/ColorBrushes.cs`) that the
   existing call sites use instead.

`RunInstance`'s `Process.Start(UseShellExecute = true)` post-run-URL call moves as-is —
it is BCL-only. Making URL-opening work on Linux is a later phase's concern.

## What stays in the WPF app

`TerminalBridge` (WebView2-coupled), `ToastHelper` (WinForms tray), all Views,
`MainWindow.*`, `App.*`, and the new `WpfDispatcher` + `ColorBrushes` helpers.

## Tests

- `CodeShellManager.Tests` adds a reference to Core (keeping the WPF project reference
  for anything WPF-side it touches).
- `[assembly: InternalsVisibleTo("CodeShellManager.Tests")]` moves/extends to Core's
  assembly (covers `ShellSession.BuildSshArgs`, `RunInstance`/`SessionRunner` internal
  ctors, `PseudoTerminal.BuildCmdLine`).
- The `Func<IPseudoTerminal>` test seams move with their classes, unchanged.
- Acceptance: `dotnet test tests/CodeShellManager.Tests/` — all 206 tests pass.

## Solution / CI

- Add `src/CodeShellManager.Core/CodeShellManager.Core.csproj` to `CodeShellManager.slnx`.
- `.github/workflows/build.yml` unchanged — it builds the app project, which now pulls
  Core in via the project reference.

## Risks

- **Hidden WPF touches** in "clean" files that the audit's grep missed — surfaced
  immediately by the Core build failing; resolved per-case (move the member to the WPF
  side or add to a seam).
- **`SessionViewModel`'s bridge surface** turns out larger than expected — the
  `ITerminalBridge` interface grows accordingly; it must not absorb WebView2 types
  (that's the line that may not be crossed).
- **Test project entanglement** with WPF types — tests that exercise WPF-side helpers
  keep doing so via the retained WPF project reference.

## Out of scope

- Any Avalonia code (2b).
- Behavior changes, refactors beyond the three seams, fixing the spike's "must-fix"
  bridge patterns (2b, where the Avalonia bridge is written).
- Porta.Pty / cross-platform PTY (later phases; `IPseudoTerminal` is the seam).
- Cross-platform URL opening, toast abstraction (later phases).

## Acceptance criteria

1. `dotnet build src/CodeShellManager/CodeShellManager.csproj` — clean.
2. `dotnet test tests/CodeShellManager.Tests/` — 206/206 pass.
3. WPF app launches and a manual smoke test (new session, type, close) behaves
   identically.
4. Core has zero references to WPF/WinForms/WebView2 assemblies (verifiable from the
   csproj: no `UseWPF`/`UseWindowsForms`, TFM `net10.0`).
5. No namespace changes; git history shows moves, not rewrites, wherever possible
   (`git mv`).

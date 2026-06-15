# Shared Core Extraction (Phase 2a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract all platform-agnostic code from the WPF app into a new `net10.0` class library `CodeShellManager.Core`, so a future Avalonia app can share it — with zero behavior change and all 206 unit tests still passing.

**Architecture:** A pure-move refactor. Files keep their `CodeShellManager.*` namespaces (so the WPF-side diff is just deletions + a project reference). The WPF app references Core. A handful of host-boundary seams (`IDispatcher`, `IToastNotifier`, `ITerminalBridge`, a `CleanStart` flag, and a `UpdateWindowState` signature change) decouple `MainViewModel`/`SessionViewModel` from WPF; the WPF app supplies thin adapters. `TerminalBridge` and `ToastHelper` (WebView2/WinForms-bound) stay in the WPF app.

**Tech Stack:** .NET 10, C#, CommunityToolkit.Mvvm (MIT), Microsoft.Data.Sqlite (MIT). No new dependencies (OSS-only constraint — any addition must be license-checked).

**Spec:** `docs/superpowers/specs/2026-06-12-shared-core-extraction-design.md`

**Working branch:** `feat/2a-shared-core` off `cross-platform`. Merge back to `cross-platform` when green.

**Verification model:** This is a mechanical extraction, not feature work — the existing test suite is the regression gate, not new TDD. Every task ends by building the WPF app and running all 206 tests; they must stay green. Two tiny new tests are added (Tasks 4 and 6) only where a seam introduces genuinely new Core logic worth pinning.

**Key facts established by the pre-plan audit (2026-06-15):**
- `ColorService.GetColor`/`GetBrush` are **dead code** — never called. MainWindow does its own inline `(Color)ColorConverter.ConvertFromString(...)`. Only `GetHexColor` is used (SessionViewModel:43, MainWindow:4361). So the "ColorService split" is just deleting the two WPF members.
- `MainViewModel` WPF touch points: `App.Current.Dispatcher.Invoke` ×4 (lines 78, 273, 281, 291); `App.CleanStart` ×2 (231, 338); `ToastHelper.Show` ×1 (286); `System.Windows.WindowState` (244–247).
- `UpdateWindowState` callers: `MainWindow.xaml.cs:199` and `:4846`, both `_vm.UpdateWindowState(WindowState, Left, Top, Width, Height)`.
- `vm.Bridge` (the typed property) is used for these members only: `UserInput` event, `SendToTerminal`, `FitTerminal`, `FocusTerminal`, `ApplyFontSettings`, `Dispose`. (All other `TerminalBridge` calls in MainWindow go through a local concrete `bridge` variable during setup, not `vm.Bridge`.)
- `TerminalBridge` is `public sealed class TerminalBridge : IDisposable` (Terminal/TerminalBridge.cs:18) and already has all six members above.
- `InternalsVisibleTo("CodeShellManager.Tests")` lives at `src/CodeShellManager/AssemblyInfo.cs:5`.

---

### Task 1: Create the empty Core project

**Files:**
- Create: `src/CodeShellManager.Core/CodeShellManager.Core.csproj`
- Modify: `CodeShellManager.slnx`

- [ ] **Step 1: Create the branch**

```bash
git switch cross-platform
git pull
git switch -c feat/2a-shared-core
```

- [ ] **Step 2: Write the Core csproj**

`src/CodeShellManager.Core/CodeShellManager.Core.csproj`:

```xml
<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <Nullable>enable</Nullable>
    <ImplicitUsings>enable</ImplicitUsings>
    <RootNamespace>CodeShellManager</RootNamespace>
  </PropertyGroup>

  <ItemGroup>
    <PackageReference Include="CommunityToolkit.Mvvm" Version="8.4.2" />
    <PackageReference Include="Microsoft.Data.Sqlite" Version="10.0.8" />
  </ItemGroup>

  <ItemGroup>
    <InternalsVisibleTo Include="CodeShellManager.Tests" />
  </ItemGroup>

</Project>
```

(Versions match the WPF csproj: CommunityToolkit.Mvvm 8.4.2, Microsoft.Data.Sqlite 10.0.8. `RootNamespace` is `CodeShellManager` so moved files' `CodeShellManager.Models` etc. namespaces are unaffected. `<InternalsVisibleTo>` is the SDK-native form — no AssemblyInfo file needed.)

- [ ] **Step 3: Add Core to the solution**

Read `CodeShellManager.slnx` first to match its element style, then add a `<Project Path="src/CodeShellManager.Core/CodeShellManager.Core.csproj" />` entry alongside the existing projects.

Run: `dotnet build src/CodeShellManager.Core/CodeShellManager.Core.csproj`
Expected: build succeeds (empty project, 0 files, 0 errors).

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager.Core/ CodeShellManager.slnx
git commit -m "build(core): add empty CodeShellManager.Core class library"
```

---

### Task 2: Move Models + reference Core from the app

**Files:**
- Move: all 7 files `src/CodeShellManager/Models/*.cs` → `src/CodeShellManager.Core/Models/`
- Modify: `src/CodeShellManager/CodeShellManager.csproj` (add ProjectReference)
- Modify: `tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj` (add ProjectReference)

- [ ] **Step 1: Move the Models files with git mv**

```bash
mkdir src/CodeShellManager.Core/Models
git mv src/CodeShellManager/Models/AlertEvent.cs src/CodeShellManager.Core/Models/
git mv src/CodeShellManager/Models/AppState.cs src/CodeShellManager.Core/Models/
git mv src/CodeShellManager/Models/RecentlyClosedEntry.cs src/CodeShellManager.Core/Models/
git mv src/CodeShellManager/Models/RunCommandItem.cs src/CodeShellManager.Core/Models/
git mv src/CodeShellManager/Models/SessionGroup.cs src/CodeShellManager.Core/Models/
git mv src/CodeShellManager/Models/ShellSession.cs src/CodeShellManager.Core/Models/
git mv src/CodeShellManager/Models/WindowsTerminalProfile.cs src/CodeShellManager.Core/Models/
```

(If the `Models/` directory contains other `.cs` files not in this list, move them too — the audit listed exactly these 7. Verify with `git status` that `src/CodeShellManager/Models/` is now empty.)

- [ ] **Step 2: Add the project reference from the WPF app to Core**

In `src/CodeShellManager/CodeShellManager.csproj`, add to the existing `<ItemGroup>` that holds `<PackageReference>`s (or a new `<ItemGroup>`):

```xml
    <ProjectReference Include="..\CodeShellManager.Core\CodeShellManager.Core.csproj" />
```

- [ ] **Step 3: Add the project reference from Tests to Core**

Read `tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj`. It already references the WPF app project. Add alongside it:

```xml
    <ProjectReference Include="..\..\src\CodeShellManager.Core\CodeShellManager.Core.csproj" />
```

(Keep the existing reference to the WPF app project — tests may touch WPF-side helpers. The moved types live only in Core now, so there's no ambiguity.)

- [ ] **Step 4: Build the app**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: success. The WPF app's auto-globbing no longer picks up the moved files (they're under a sibling project dir now); it resolves them from Core via the project reference. If any "type or namespace not found" errors appear, they indicate a file that wasn't moved or a missing reference — fix before continuing.

- [ ] **Step 5: Run the tests**

Run: `dotnet test tests/CodeShellManager.Tests/`
Expected: `Passed! - Failed: 0, Passed: 206`. This proves `ShellSession.BuildSshArgs` (internal) is still visible to tests via Core's `InternalsVisibleTo`.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(core): move Models to CodeShellManager.Core"
```

---

### Task 3: Move WPF-free Services + Terminal to Core

**Files:**
- Move: 18 Service files + 3 Terminal files (listed below) → Core
- These have no WPF/WinForms/WebView2 references (audit-confirmed). `ColorService` is NOT moved here (Task 4). `ToastHelper` is NOT moved (stays WPF). `TerminalBridge` is NOT moved (stays WPF).

- [ ] **Step 1: Move the Services**

```bash
mkdir src/CodeShellManager.Core/Services
git mv src/CodeShellManager/Services/AlertDetector.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/BuiltInTerminalSchemes.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/ClaudeSessionService.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/CommandLineSplitter.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/CommandPresetsService.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/CursorShapeMapper.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/GitService.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/ImportExportService.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/PaddingParser.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/RunCommandTemplatesService.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/RunInstance.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/SchemeMapper.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/SearchService.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/SessionManager.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/SessionRunner.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/StateService.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/UpdateService.cs src/CodeShellManager.Core/Services/
git mv src/CodeShellManager/Services/WindowsTerminalProfileService.cs src/CodeShellManager.Core/Services/
```

After this, `src/CodeShellManager/Services/` should contain only `ColorService.cs` and `ToastHelper.cs`. Verify with `git status`.

- [ ] **Step 2: Move the Terminal files**

```bash
mkdir src/CodeShellManager.Core/Terminal
git mv src/CodeShellManager/Terminal/IPseudoTerminal.cs src/CodeShellManager.Core/Terminal/
git mv src/CodeShellManager/Terminal/OutputIndexer.cs src/CodeShellManager.Core/Terminal/
git mv src/CodeShellManager/Terminal/PseudoTerminal.cs src/CodeShellManager.Core/Terminal/
```

After this, `src/CodeShellManager/Terminal/` should contain only `TerminalBridge.cs`. (`PseudoTerminal` is ConPTY P/Invoke — Windows-runtime-bound but pure BCL, no WPF; it compiles in a `net10.0` lib and is only instantiated on Windows.) Verify with `git status`.

- [ ] **Step 3: Build the app**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: success. Likely errors to watch for and fix:
- `TerminalBridge` references `PseudoTerminal` (it has `AttachPty(PseudoTerminal)`) — now resolved via Core's project reference; should just work since the WPF app references Core.
- Any `internal` member of a moved Service used by another moved Service is fine (same assembly now). Any `internal` member used *across* the Core/WPF boundary would error — none expected per the audit, but if one appears, surface it (don't widen visibility without noting it).

- [ ] **Step 4: Run the tests**

Run: `dotnet test tests/CodeShellManager.Tests/`
Expected: `Passed! - Failed: 0, Passed: 206`. Proves `RunInstance`/`SessionRunner` internal ctors and `PseudoTerminal.BuildCmdLine` (all internal, all now in Core) are still test-visible.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(core): move WPF-free Services and Terminal types to Core"
```

---

### Task 4: ColorService — drop dead WPF members, move to Core

**Files:**
- Modify then move: `src/CodeShellManager/Services/ColorService.cs` → `src/CodeShellManager.Core/Services/ColorService.cs`
- Test: `tests/CodeShellManager.Tests/ColorServiceTests.cs` (create)

`GetColor` and `GetBrush` are dead code (audit: zero callers). Remove them and the `System.Windows.Media` dependency, leaving the pure hash/hex logic.

- [ ] **Step 1: Write a failing test pinning GetHexColor**

Create `tests/CodeShellManager.Tests/ColorServiceTests.cs`:

```csharp
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class ColorServiceTests
{
    [Fact]
    public void GetHexColor_IsDeterministic_ForSameInput()
    {
        var a = ColorService.GetHexColor(@"C:\repos\project-x");
        var b = ColorService.GetHexColor(@"C:\repos\project-x");
        Assert.Equal(a, b);
    }

    [Fact]
    public void GetHexColor_IgnoresTrailingSlashAndCase()
    {
        Assert.Equal(
            ColorService.GetHexColor(@"C:\Repos\Project-X"),
            ColorService.GetHexColor(@"c:\repos\project-x\"));
    }

    [Fact]
    public void GetHexColor_ReturnsAPaletteHexString()
    {
        var hex = ColorService.GetHexColor("anything");
        Assert.Matches("^#[0-9A-Fa-f]{6}$", hex);
    }
}
```

- [ ] **Step 2: Run the test to verify it passes against the current (pre-move) ColorService**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter ColorServiceTests`
Expected: PASS (3 tests). This pins current behavior *before* the edit so any regression in Step 3 is caught. (The test exercises only `GetHexColor`, which the edit leaves untouched — confirming the case/slash normalization in `Fnv1a(folderPath.ToLowerInvariant().TrimEnd('/', '\\'))`.)

- [ ] **Step 3: Edit ColorService to remove the WPF members, then move it**

Replace the entire contents of `src/CodeShellManager/Services/ColorService.cs` with (note: `using System.Windows.Media;` removed, `GetColor`/`GetBrush` deleted):

```csharp
namespace CodeShellManager.Services;

public static class ColorService
{
    // 12 colours spaced ~30° apart on the HSL wheel, all at similar saturation/lightness
    // so every project folder gets a clearly distinct accent colour.
    private static readonly string[] Palette =
    [
        "#FF6B6B",  // red
        "#FF9E42",  // orange
        "#FFD166",  // yellow
        "#AEDE68",  // lime
        "#51CF66",  // green
        "#38D9A9",  // emerald
        "#66D9E8",  // cyan
        "#4DABF7",  // blue
        "#748FFC",  // indigo
        "#9775FA",  // violet
        "#F783AC",  // pink
        "#FF6B95",  // rose
    ];

    public static string GetHexColor(string folderPath)
    {
        uint hash = Fnv1a(folderPath.ToLowerInvariant().TrimEnd('/', '\\'));
        return Palette[hash % (uint)Palette.Length];
    }

    private static uint Fnv1a(string s)
    {
        uint hash = 2166136261u;
        foreach (char c in s) { hash ^= (byte)c; hash *= 16777619u; }
        return hash;
    }
}
```

Then move it:

```bash
git mv src/CodeShellManager/Services/ColorService.cs src/CodeShellManager.Core/Services/ColorService.cs
```

After this, `src/CodeShellManager/Services/` contains only `ToastHelper.cs`.

- [ ] **Step 4: Build the app and run the full suite**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: success. (MainWindow's inline `ColorConverter.ConvertFromString` conversions are untouched — they never used `ColorService.GetColor`/`GetBrush`.)

Run: `dotnet test tests/CodeShellManager.Tests/`
Expected: `Passed! - Failed: 0, Passed: 209` (206 + 3 new).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(core): drop dead ColorService WPF members and move to Core"
```

---

### Task 5: ITerminalBridge seam + move SessionViewModel

**Files:**
- Create: `src/CodeShellManager.Core/Terminal/ITerminalBridge.cs`
- Modify: `src/CodeShellManager/Terminal/TerminalBridge.cs` (implement the interface)
- Modify then move: `src/CodeShellManager/ViewModels/SessionViewModel.cs` → Core (change `Bridge` type)

- [ ] **Step 1: Create the ITerminalBridge interface in Core**

`src/CodeShellManager.Core/Terminal/ITerminalBridge.cs`:

```csharp
using CodeShellManager.Models;

namespace CodeShellManager.Terminal;

/// <summary>
/// The slice of the terminal bridge that platform-agnostic view-models depend on.
/// The WPF app's WebView2-backed <c>TerminalBridge</c> implements it; a future
/// Avalonia bridge will implement the same surface. Members here are exactly those
/// called through the typed <c>SessionViewModel.Bridge</c> property — nothing more.
/// </summary>
public interface ITerminalBridge : IDisposable
{
    event Action? UserInput;
    void SendToTerminal(string text);
    void FitTerminal();
    void FocusTerminal();
    void ApplyFontSettings(AppSettings settings);
}
```

(Every member is confirmed present on `TerminalBridge`: `UserInput`:47, `SendToTerminal`:427, `FitTerminal`:429, `FocusTerminal`:443, `ApplyFontSettings`:363, `Dispose`:461. All signatures are WPF-free — `AppSettings` is in Core.)

- [ ] **Step 2: Make TerminalBridge implement it**

In `src/CodeShellManager/Terminal/TerminalBridge.cs:18`, change:

```csharp
public sealed class TerminalBridge : IDisposable
```

to:

```csharp
public sealed class TerminalBridge : ITerminalBridge
```

(`ITerminalBridge` extends `IDisposable`, so the existing `Dispose` still satisfies it. `TerminalBridge` is in namespace `CodeShellManager.Terminal` — same as the interface — so no using needed. No member changes: the class already matches the interface.)

- [ ] **Step 3: Change SessionViewModel.Bridge to the interface type, then move it**

In `src/CodeShellManager/ViewModels/SessionViewModel.cs`, change line 30:

```csharp
    public TerminalBridge? Bridge { get; set; }
```

to:

```csharp
    public ITerminalBridge? Bridge { get; set; }
```

The `using CodeShellManager.Terminal;` at the top already covers `ITerminalBridge`. Leave `Pty` as `PseudoTerminal?` (that type is in Core). No other change — `Bridge?.Dispose()` in `Dispose()` still works.

Then move the file:

```bash
mkdir src/CodeShellManager.Core/ViewModels
git mv src/CodeShellManager/ViewModels/SessionViewModel.cs src/CodeShellManager.Core/ViewModels/
```

- [ ] **Step 4: Build the app**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: success. Watch for: MainWindow assigns `vm.Bridge = bridge;` (line 975) where `bridge` is a concrete `TerminalBridge` — assigning to an `ITerminalBridge?` property is valid. MainWindow's `vm.Bridge?.SendToTerminal/FitTerminal/FocusTerminal/ApplyFontSettings` calls all resolve through the interface. If MainWindow calls any *other* member via `vm.Bridge`, the build will flag it — if so, that member must be added to `ITerminalBridge` (it must stay WPF-free; if it isn't, stop and report).

- [ ] **Step 5: Run the tests**

Run: `dotnet test tests/CodeShellManager.Tests/`
Expected: `Passed! - Failed: 0, Passed: 209`.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(core): add ITerminalBridge seam and move SessionViewModel to Core"
```

---

### Task 6: MainViewModel host seams + move to Core

**Files:**
- Create: `src/CodeShellManager.Core/Services/IDispatcher.cs`
- Create: `src/CodeShellManager.Core/Services/IToastNotifier.cs`
- Create: `src/CodeShellManager/Services/WpfDispatcher.cs`
- Create: `src/CodeShellManager/Services/WpfToastNotifier.cs`
- Modify then move: `src/CodeShellManager/ViewModels/MainViewModel.cs` → Core
- Modify: `src/CodeShellManager/MainWindow.xaml.cs` (2 `UpdateWindowState` call sites)
- Modify: the MainViewModel construction site + `CleanStart` wiring (located in Step 5)
- Test: `tests/CodeShellManager.Tests/MainViewModelSeamTests.cs` (create)

This task removes MainViewModel's four WPF couplings: `App.Current.Dispatcher`, `App.CleanStart`, `ToastHelper.Show`, `System.Windows.WindowState`.

- [ ] **Step 1: Create the two Core interfaces**

`src/CodeShellManager.Core/Services/IDispatcher.cs`:

```csharp
namespace CodeShellManager.Services;

/// <summary>Marshals an action onto the UI thread. Implemented per-host (WPF, Avalonia).</summary>
public interface IDispatcher
{
    void Post(Action action);
}
```

`src/CodeShellManager.Core/Services/IToastNotifier.cs`:

```csharp
namespace CodeShellManager.Services;

/// <summary>Shows a host notification (tray balloon on WPF). Implemented per-host.</summary>
public interface IToastNotifier
{
    void Show(string title, string message, bool playSound);
}
```

- [ ] **Step 2: Create the WPF adapters**

`src/CodeShellManager/Services/WpfDispatcher.cs`:

```csharp
using System;
using System.Windows;
using CodeShellManager.Services;

namespace CodeShellManager.Services;

/// <summary>WPF <see cref="IDispatcher"/> — posts onto the application dispatcher.</summary>
public sealed class WpfDispatcher : IDispatcher
{
    public void Post(Action action) => Application.Current.Dispatcher.Invoke(action);
}
```

`src/CodeShellManager/Services/WpfToastNotifier.cs`:

```csharp
using CodeShellManager.Services;

namespace CodeShellManager.Services;

/// <summary>WPF <see cref="IToastNotifier"/> — delegates to the existing tray helper.</summary>
public sealed class WpfToastNotifier : IToastNotifier
{
    public void Show(string title, string message, bool playSound)
        => ToastHelper.Show(title, message, playSound);
}
```

(`WpfDispatcher.Post` uses `Dispatcher.Invoke` — synchronous, matching MainViewModel's current `App.Current.Dispatcher.Invoke` behavior exactly, so no marshaling-timing change.)

- [ ] **Step 3: Edit MainViewModel — inject seams, replace couplings**

In `src/CodeShellManager/ViewModels/MainViewModel.cs`:

(a) Add fields and a `CleanStart` flag near the other private fields (after line 27 `private AppState _appState = new();`):

```csharp
    private readonly IDispatcher _dispatcher;
    private readonly IToastNotifier _toast;

    /// <summary>When true, state is never persisted (debug isolation). Set by the host at startup.</summary>
    public bool CleanStart { get; set; }
```

(b) Change the constructor (lines 73–79) to accept the two seams:

```csharp
    public MainViewModel(SessionManager sessionManager, StateService stateService,
        IDispatcher dispatcher, IToastNotifier toast)
    {
        _sessionManager = sessionManager;
        _stateService = stateService;
        _dispatcher = dispatcher;
        _toast = toast;
        _sessionManager.GroupsChanged += () =>
            _dispatcher.Post(() => GroupsChanged?.Invoke());
    }
```

(c) Replace the three remaining `App.Current.Dispatcher.Invoke(` calls (lines 273, 281, 291) with `_dispatcher.Post(`. The blocks are:

Line 273:
```csharp
                _dispatcher.Post(() => OnPropertyChanged(nameof(AlertCount)));
```
Line 281 (opening of the AlertRaised block):
```csharp
                _dispatcher.Post(() =>
```
Line 291 (opening of the AlertCleared block):
```csharp
                _dispatcher.Post(() =>
```

(d) Replace the `ToastHelper.Show(...)` call (line 286) with:

```csharp
                        _toast.Show(vm.DisplayName, alert.Message, Settings.ShowNotificationSound);
```

(e) Replace both `App.CleanStart` reads (lines 231, 338) with `CleanStart`:

```csharp
        if (CleanStart) return;
```

(f) Change `UpdateWindowState` (lines 244–257) to take bools instead of `System.Windows.WindowState`:

```csharp
    /// <summary>Saves window position/size. Only updates NormalBounds when not maximized.</summary>
    public void UpdateWindowState(bool isMaximized, bool isNormal, double left, double top, double width, double height)
    {
        _appState.WindowMaximized = isMaximized;
        if (isNormal)
        {
            _appState.LastNormalBounds = new Models.WindowBounds
            {
                Left = left,
                Top = top,
                Width = width,
                Height = height
            };
        }
    }
```

After these edits, MainViewModel has no `App.`, `ToastHelper`, or `System.Windows` references. (It still uses `CommunityToolkit.Mvvm`, `System.*` BCL — all Core-safe.)

- [ ] **Step 4: Move MainViewModel to Core**

```bash
git mv src/CodeShellManager/ViewModels/MainViewModel.cs src/CodeShellManager.Core/ViewModels/
```

After this, `src/CodeShellManager/ViewModels/` is empty (both VMs now in Core).

- [ ] **Step 5: Update the construction site and CleanStart wiring**

Find where MainViewModel is constructed and where `App.CleanStart` is set:

```bash
grep -rn "new MainViewModel(" src/CodeShellManager/
grep -rn "CleanStart" src/CodeShellManager/App.xaml.cs src/CodeShellManager/MainWindow.xaml.cs
```

At the `new MainViewModel(...)` call site, pass the adapters and set `CleanStart`. Expected shape (adapt to the actual surrounding code — the constructor args `sessionManager`/`stateService` already exist there):

```csharp
        _vm = new MainViewModel(sessionManager, stateService, new WpfDispatcher(), new WpfToastNotifier())
        {
            CleanStart = App.CleanStart,
        };
```

(`App.CleanStart` remains the source of truth on the WPF `App` class — it's read once here and copied into the VM. Do not remove `App.CleanStart` itself; `App`/`MainWindow` may still use it directly.)

- [ ] **Step 6: Fix the two UpdateWindowState call sites**

In `src/CodeShellManager/MainWindow.xaml.cs` at lines 199 and 4846, change:

```csharp
        _vm.UpdateWindowState(WindowState, Left, Top, Width, Height);
```

to:

```csharp
        _vm.UpdateWindowState(
            WindowState == System.Windows.WindowState.Maximized,
            WindowState == System.Windows.WindowState.Normal,
            Left, Top, Width, Height);
```

- [ ] **Step 7: Write a test pinning the CleanStart no-op and window-state logic**

Create `tests/CodeShellManager.Tests/MainViewModelSeamTests.cs`:

```csharp
using CodeShellManager.Services;
using CodeShellManager.ViewModels;
using Xunit;

namespace CodeShellManager.Tests;

public class MainViewModelSeamTests
{
    private sealed class ImmediateDispatcher : IDispatcher
    {
        public void Post(System.Action action) => action();
    }

    private sealed class NoopToast : IToastNotifier
    {
        public int Calls;
        public void Show(string title, string message, bool playSound) => Calls++;
    }

    private static MainViewModel NewVm()
    {
        var sm = new SessionManager();
        var ss = new StateService();
        return new MainViewModel(sm, ss, new ImmediateDispatcher(), new NoopToast());
    }

    [Fact]
    public void UpdateWindowState_Maximized_DoesNotOverwriteNormalBounds()
    {
        var vm = NewVm();
        vm.UpdateWindowState(isMaximized: false, isNormal: true, 10, 20, 800, 600);
        vm.UpdateWindowState(isMaximized: true, isNormal: false, 0, 0, 1920, 1040);

        Assert.True(vm.IsWindowMaximized());
        var bounds = vm.GetSavedWindowBounds();
        Assert.NotNull(bounds);
        Assert.Equal(10, bounds!.Left);
        Assert.Equal(800, bounds.Width);
    }

    [Fact]
    public void CleanStart_SuppressesRecentlyClosedPush()
    {
        var vm = NewVm();
        vm.CleanStart = true;
        vm.PushRecentlyClosed(new CodeShellManager.Models.ShellSession { Name = "x" });
        Assert.Empty(vm.RecentlyClosed);
    }
}
```

(Verify `SessionManager` and `StateService` have parameterless constructors when writing this — if `StateService` needs a path argument, construct it the way existing tests do; check an existing test under `tests/CodeShellManager.Tests/` for the established pattern and match it. Also confirm `ShellSession` has a settable `Name` — adjust the initializer to match its actual surface.)

- [ ] **Step 8: Build and run the full suite**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: success.

Run: `dotnet test tests/CodeShellManager.Tests/`
Expected: `Passed! - Failed: 0, Passed: 211` (209 + 2 new).

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(core): decouple MainViewModel from WPF host and move to Core"
```

---

### Task 7: Verify Core isolation and finish

**Files:** none modified (verification + merge).

- [ ] **Step 1: Confirm Core has zero WPF/WinForms/WebView2 references**

```bash
grep -rn "System.Windows\|System.Drawing\|Microsoft.Web.WebView2\|using System.Windows.Forms\|App.Current\|App.CleanStart\|ToastHelper" src/CodeShellManager.Core/
```
Expected: **no matches.** (A match means a coupling slipped through — fix it before finishing.)

Also confirm the Core csproj has no `UseWPF`/`UseWindowsForms` and targets `net10.0`:

```bash
grep -n "UseWPF\|UseWindowsForms\|TargetFramework" src/CodeShellManager.Core/CodeShellManager.Core.csproj
```
Expected: only `<TargetFramework>net10.0</TargetFramework>`.

- [ ] **Step 2: Confirm Core builds standalone on a non-Windows TFM path**

Run: `dotnet build src/CodeShellManager.Core/CodeShellManager.Core.csproj`
Expected: success with 0 warnings about WPF/Windows-only APIs. (PseudoTerminal's ConPTY P/Invoke compiles fine on `net10.0`; it's only a *runtime* Windows dependency.)

- [ ] **Step 3: Full solution build + full test run**

Run: `dotnet build CodeShellManager.slnx`
Expected: success across Core, the WPF app, and both test projects.

Run: `dotnet test tests/CodeShellManager.Tests/`
Expected: `Passed! - Failed: 0, Passed: 211`.

- [ ] **Step 4: Manual smoke test**

Run: `dotnet run --project src/CodeShellManager/CodeShellManager.csproj`
Confirm: app launches; create a new local session; type a command and see output; close the session; close the app. Behavior identical to before the extraction. (If a Windows desktop isn't available in the executing environment, note this step as skipped and flag it for the user to run.)

- [ ] **Step 5: Merge the branch back to cross-platform**

```bash
git switch cross-platform
git pull
git merge --no-ff feat/2a-shared-core -m "Merge feat/2a-shared-core: extract CodeShellManager.Core (Phase 2a, issue #32)"
git push
```

---

## Self-review notes

- **Spec coverage:** Core library (T1); Models move (T2); Services+Terminal move (T3); ColorService split — simplified to dead-member deletion, documented (T4); ITerminalBridge + SessionViewModel (T5); IDispatcher + the additional MainViewModel seams the audit under-counted (ToastHelper→IToastNotifier, App.CleanStart→CleanStart flag, WindowState→bool signature) (T6); tests reference Core + InternalsVisibleTo (T1/T2); CI unchanged (no workflow edits needed — build.yml builds the app which pulls Core transitively); acceptance criteria 1–5 verified (T7). ✓
- **Deviation from spec, flagged:** the spec named "three seams" and classified MainViewModel as needing only the Dispatcher seam. The audit for this plan found three more MainViewModel couplings (CleanStart, ToastHelper, WindowState). They're the same category (MainViewModel→host) and are required to meet acceptance criterion 4 ("Core has zero WPF references"), so the plan handles them in Task 6 rather than expanding scope elsewhere. The ColorService seam turned out simpler than specced (dead code, not a live split). Both are noted for the user's spec review.
- **Type consistency:** `IDispatcher.Post(Action)`, `IToastNotifier.Show(string,string,bool)`, `ITerminalBridge` (5 members + IDisposable), `UpdateWindowState(bool,bool,double,double,double,double)`, `MainViewModel.CleanStart` — used consistently across tasks and call-site fixes.
- **No new dependencies** — Core uses only the two MIT packages the app already had (OSS-only constraint upheld).

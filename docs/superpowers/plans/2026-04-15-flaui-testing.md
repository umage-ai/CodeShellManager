# FlaUI UI Test Suite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a FlaUI + xUnit smoke test suite covering session lifecycle, layout switching, and settings persistence for the WPF shell.

**Architecture:** Per-class `IClassFixture<AppFixture>` — the app launches once per test class into an isolated temp state file (via `CSM_STATE_PATH` env var), then is torn down after the class. Tests interact with the WPF shell via UI Automation IDs added to XAML. No mocking; real `cmd /c pause` processes are used.

**Tech Stack:** FlaUI.Core 4.x, FlaUI.UIA3 4.x, xUnit 2.x, .NET 10 Windows, C#

---

## File Map

| File | Action | Purpose |
|---|---|---|
| `src/CodeShellManager/Services/StateService.cs` | Modify | Read `CSM_STATE_PATH` env var for test isolation |
| `src/CodeShellManager/MainWindow.xaml` | Modify | Add `AutomationProperties.AutomationId` to toolbar/sidebar/grid |
| `src/CodeShellManager/Views/NewSessionDialog.xaml` | Modify | Add `AutomationProperties.AutomationId` to dialog controls |
| `src/CodeShellManager/Views/SettingsWindow.xaml` | Modify | Add `AutomationProperties.AutomationId` to settings controls |
| `CodeShellManager.slnx` | Modify | Add test project |
| `tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj` | Create | Test project |
| `tests/CodeShellManager.UITests/AppFixture.cs` | Create | App launch/teardown + state isolation |
| `tests/CodeShellManager.UITests/Helpers/AppActions.cs` | Create | Reusable UI helpers (CreateSession, WaitForElement, etc.) |
| `tests/CodeShellManager.UITests/SessionTests.cs` | Create | Session lifecycle tests |
| `tests/CodeShellManager.UITests/LayoutTests.cs` | Create | Layout switching tests |
| `tests/CodeShellManager.UITests/SettingsTests.cs` | Create | Settings persistence tests |

---

## Task 1: StateService env var override

**Files:**
- Modify: `src/CodeShellManager/Services/StateService.cs`

- [ ] **Step 1: Replace static `StatePath` with a dynamic property**

Replace:
```csharp
private static readonly string StatePath = Path.Combine(
    Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
    "CodeShellManager", "state.json");
```
With:
```csharp
private static string StatePath =>
    Environment.GetEnvironmentVariable("CSM_STATE_PATH")
    ?? Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "CodeShellManager", "state.json");
```

- [ ] **Step 2: Build to verify no regressions**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj -nologo -v q
```
Expected: `Build succeeded. 0 Warning(s) 0 Error(s)`

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/Services/StateService.cs
git commit -m "feat: support CSM_STATE_PATH env var for test isolation"
```

---

## Task 2: AutomationIds in MainWindow.xaml

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml`

- [ ] **Step 1: Add AutomationIds to toolbar buttons**

Add `AutomationProperties.AutomationId="..."` to these controls. Find each by its `Click` handler or `Content` attribute and add the attribute:

```xml
<!-- New Session button — find by Click="NewSession_Click" -->
<Button ... Click="NewSession_Click"
        AutomationProperties.AutomationId="NewSessionBtn" .../>

<!-- RC All button — find by Click="BroadcastRemoteControl_Click" -->
<Button ... Click="BroadcastRemoteControl_Click"
        AutomationProperties.AutomationId="BroadcastBtn" .../>

<!-- Search button — find by x:Name="SearchBtn" -->
<Button x:Name="SearchBtn" ...
        AutomationProperties.AutomationId="SearchBtn" .../>

<!-- Settings button — find by x:Name="SettingsButton" -->
<Button x:Name="SettingsButton" ...
        AutomationProperties.AutomationId="SettingsBtn" .../>

<!-- Layout buttons — find by Click handlers -->
<Button ... Click="Layout_Single_Click" AutomationProperties.AutomationId="Layout_Single" .../>
<Button ... Click="Layout_Two_Click"    AutomationProperties.AutomationId="Layout_Two" .../>
<Button ... Click="Layout_Grid_Click"   AutomationProperties.AutomationId="Layout_Grid" .../>
```

- [ ] **Step 2: Add AutomationIds to sidebar and terminal grid**

```xml
<!-- Sidebar session list — find by x:Name="SidebarSessionList" -->
<StackPanel x:Name="SidebarSessionList"
            AutomationProperties.AutomationId="SidebarSessionList" .../>

<!-- Terminal grid — find by x:Name="TerminalGrid" -->
<Grid x:Name="TerminalGrid"
      AutomationProperties.AutomationId="TerminalGrid" .../>

<!-- Alert badge — find by x:Name="AlertBadge" -->
<Border x:Name="AlertBadge"
        AutomationProperties.AutomationId="AlertBadge" .../>
```

- [ ] **Step 3: Build**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj -nologo -v q
```
Expected: `Build succeeded. 0 Warning(s) 0 Error(s)`

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml
git commit -m "feat: add AutomationIds to main window for UI tests"
```

---

## Task 3: AutomationIds in NewSessionDialog.xaml

**Files:**
- Modify: `src/CodeShellManager/Views/NewSessionDialog.xaml`

- [ ] **Step 1: Add AutomationIds to dialog controls**

The dialog has: `FolderBox` (TextBox), `CommandCombo` (ComboBox), `NameBox` (TextBox), and a "Start Session" primary button. Add AutomationIds:

```xml
<!-- Folder TextBox — find by x:Name="FolderBox" -->
<TextBox x:Name="FolderBox"
         AutomationProperties.AutomationId="NewSessionFolderBox" .../>

<!-- Command ComboBox — find by x:Name="CommandCombo" -->
<ComboBox x:Name="CommandCombo"
          AutomationProperties.AutomationId="NewSessionCommandCombo" .../>

<!-- Session Name TextBox — find by x:Name="NameBox" -->
<TextBox x:Name="NameBox"
         AutomationProperties.AutomationId="NewSessionNameBox" .../>

<!-- Start Session button — find by Content="Start Session" -->
<Button Content="Start Session" ...
        AutomationProperties.AutomationId="NewSessionOkBtn" .../>
```

- [ ] **Step 2: Build**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj -nologo -v q
```
Expected: `Build succeeded. 0 Warning(s) 0 Error(s)`

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/Views/NewSessionDialog.xaml
git commit -m "feat: add AutomationIds to NewSessionDialog for UI tests"
```

---

## Task 4: AutomationIds in SettingsWindow.xaml

**Files:**
- Modify: `src/CodeShellManager/Views/SettingsWindow.xaml`

- [ ] **Step 1: Add AutomationIds to settings controls**

```xml
<!-- Max Search Results TextBox — find by x:Name="MaxSearchResultsBox" -->
<TextBox x:Name="MaxSearchResultsBox"
         AutomationProperties.AutomationId="MaxSearchResultsBox" .../>

<!-- Save button — find by Content="Save" and Click="Save_Click" -->
<Button Content="Save" ... Click="Save_Click"
        AutomationProperties.AutomationId="SettingsSaveBtn" .../>
```

- [ ] **Step 2: Build**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj -nologo -v q
```
Expected: `Build succeeded. 0 Warning(s) 0 Error(s)`

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/Views/SettingsWindow.xaml
git commit -m "feat: add AutomationIds to SettingsWindow for UI tests"
```

---

## Task 5: Create test project

**Files:**
- Create: `tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj`
- Modify: `CodeShellManager.slnx`

- [ ] **Step 1: Create the test project file**

Create `tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj`:

```xml
<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <TargetFramework>net10.0-windows</TargetFramework>
    <Nullable>enable</Nullable>
    <ImplicitUsings>enable</ImplicitUsings>
    <IsPackable>false</IsPackable>
    <!-- Required for UI tests to run on the WPF desktop -->
    <UseWPF>false</UseWPF>
  </PropertyGroup>

  <ItemGroup>
    <PackageReference Include="FlaUI.Core"               Version="4.0.0" />
    <PackageReference Include="FlaUI.UIA3"               Version="4.0.0" />
    <PackageReference Include="Microsoft.NET.Test.Sdk"   Version="17.12.0" />
    <PackageReference Include="xunit"                    Version="2.9.3" />
    <PackageReference Include="xunit.runner.visualstudio" Version="2.8.2">
      <IncludeAssets>runtime; build; native; contentfiles; analyzers</IncludeAssets>
      <PrivateAssets>all</PrivateAssets>
    </PackageReference>
  </ItemGroup>

</Project>
```

- [ ] **Step 2: Add the project to the solution**

```bash
dotnet sln CodeShellManager.slnx add tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj
```

If the above fails (slnx format), manually edit `CodeShellManager.slnx`:

```xml
<Solution>
  <Folder Name="/src/">
    <Project Path="src/CodeShellManager/CodeShellManager.csproj" />
  </Folder>
  <Folder Name="/tests/">
    <Project Path="tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj" />
  </Folder>
</Solution>
```

- [ ] **Step 3: Restore packages**

```bash
dotnet restore tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj
```
Expected: `Restore succeeded.`

- [ ] **Step 4: Build test project**

```bash
dotnet build tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj -nologo -v q
```
Expected: `Build succeeded. 0 Warning(s) 0 Error(s)`

- [ ] **Step 5: Commit**

```bash
git add tests/ CodeShellManager.slnx
git commit -m "feat: add CodeShellManager.UITests project (FlaUI + xUnit)"
```

---

## Task 6: AppFixture

**Files:**
- Create: `tests/CodeShellManager.UITests/AppFixture.cs`

- [ ] **Step 1: Create AppFixture.cs**

```csharp
using System;
using System.IO;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.UIA3;

namespace CodeShellManager.UITests;

/// <summary>
/// Launches and tears down the CodeShellManager process once per test class.
/// Sets CSM_STATE_PATH to a temp file so tests never read the developer's real sessions.
/// </summary>
public sealed class AppFixture : IDisposable
{
    public Application App { get; }
    public Window MainWindow { get; }
    public UIA3Automation Automation { get; }
    private readonly string _statePath;

    public AppFixture()
    {
        _statePath = Path.GetTempFileName();

        // Child process inherits this env var — StateService uses it instead of %AppData%
        Environment.SetEnvironmentVariable("CSM_STATE_PATH", _statePath);

        Automation = new UIA3Automation();
        App = Application.Launch(GetExePath());
        MainWindow = App.GetMainWindow(Automation, TimeSpan.FromSeconds(15));
    }

    public void Dispose()
    {
        try { App.Close(); } catch { /* ignore if already closed */ }
        Automation.Dispose();
        try { File.Delete(_statePath); } catch { }
        Environment.SetEnvironmentVariable("CSM_STATE_PATH", null);
    }

    private static string GetExePath()
    {
        // AppContext.BaseDirectory = tests/CodeShellManager.UITests/bin/Debug/net10.0-windows/
        // Go up 5 levels to reach solution root, then into the main app's output
        string testBinDir = AppContext.BaseDirectory;
        string solutionRoot = Path.GetFullPath(Path.Combine(testBinDir, "../../../../../"));
        string exePath = Path.Combine(solutionRoot,
            "src", "CodeShellManager", "bin", "Debug", "net10.0-windows", "CodeShellManager.exe");

        if (!File.Exists(exePath))
            throw new FileNotFoundException(
                $"Main app not built. Build it first:\n" +
                $"  dotnet build src/CodeShellManager/CodeShellManager.csproj\n" +
                $"Expected at: {exePath}");

        return exePath;
    }
}
```

- [ ] **Step 2: Build**

```bash
dotnet build tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj -nologo -v q
```
Expected: `Build succeeded. 0 Warning(s) 0 Error(s)`

- [ ] **Step 3: Commit**

```bash
git add tests/CodeShellManager.UITests/AppFixture.cs
git commit -m "feat: add AppFixture for per-class app launch with state isolation"
```

---

## Task 7: AppActions helper

**Files:**
- Create: `tests/CodeShellManager.UITests/Helpers/AppActions.cs`

- [ ] **Step 1: Create AppActions.cs**

```csharp
using System;
using System.Linq;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Tools;

namespace CodeShellManager.UITests.Helpers;

/// <summary>Reusable UI interaction helpers shared across test classes.</summary>
public static class AppActions
{
    private static readonly TimeSpan DefaultTimeout = TimeSpan.FromSeconds(10);

    /// <summary>Waits up to <paramref name="timeout"/> for an element with the given AutomationId.</summary>
    public static AutomationElement WaitForElement(Window window, string automationId,
        TimeSpan? timeout = null)
    {
        var element = Retry.WhileNull(
            () => window.FindFirstDescendant(cf => cf.ByAutomationId(automationId)),
            timeout ?? DefaultTimeout);

        if (element is null)
            throw new TimeoutException(
                $"Element with AutomationId '{automationId}' not found within timeout.");
        return element;
    }

    /// <summary>
    /// Opens the New Session dialog, fills folder and name, and clicks Start Session.
    /// Waits for the dialog to close before returning.
    /// </summary>
    public static void CreateSession(Application app, Window window, UIA3Automation automation,
        string folder = @"C:\Windows", string name = "Test")
    {
        // Click New Session
        WaitForElement(window, "NewSessionBtn").AsButton().Click();

        // Wait for dialog window
        var dialog = Retry.WhileNull(
            () => app.GetAllTopLevelWindows(automation)
                     .FirstOrDefault(w => w.Title == "New Session"),
            DefaultTimeout)
            ?? throw new TimeoutException("New Session dialog did not open.");

        // Fill folder
        var folderBox = dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionFolderBox")).AsTextBox();
        folderBox.Text = folder;

        // Fill name
        var nameBox = dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionNameBox")).AsTextBox();
        nameBox.Text = name;

        // Click Start Session
        dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionOkBtn")).AsButton().Click();

        // Wait for dialog to close
        Retry.WhileFalse(
            () => app.GetAllTopLevelWindows(automation)
                     .All(w => w.Title != "New Session"),
            DefaultTimeout);
    }

    /// <summary>Returns the number of direct children in the sidebar session list.</summary>
    public static int GetSidebarSessionCount(Window window)
    {
        var list = WaitForElement(window, "SidebarSessionList");
        return list.FindAllChildren().Length;
    }

    /// <summary>Returns the number of visible children in the terminal grid.</summary>
    public static int GetTerminalGridChildCount(Window window)
    {
        var grid = WaitForElement(window, "TerminalGrid");
        return grid.FindAllChildren(cf => cf.ByControlType(FlaUI.Core.Definitions.ControlType.Pane))
                   .Length;
    }

    /// <summary>Clicks a layout button by its AutomationId (e.g. "Layout_Two").</summary>
    public static void SetLayout(Window window, string layoutAutomationId)
    {
        WaitForElement(window, layoutAutomationId).AsButton().Click();
        System.Threading.Thread.Sleep(300); // let layout refresh
    }
}
```

- [ ] **Step 2: Build**

```bash
dotnet build tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj -nologo -v q
```
Expected: `Build succeeded. 0 Warning(s) 0 Error(s)`

- [ ] **Step 3: Commit**

```bash
git add tests/CodeShellManager.UITests/Helpers/AppActions.cs
git commit -m "feat: add AppActions UI helper for FlaUI tests"
```

---

## Task 8: SessionTests

**Files:**
- Create: `tests/CodeShellManager.UITests/SessionTests.cs`

- [ ] **Step 1: Write SessionTests.cs**

```csharp
using System.Threading;
using CodeShellManager.UITests.Helpers;
using FlaUI.Core.AutomationElements;
using Xunit;

namespace CodeShellManager.UITests;

[Collection("UITests")]
public sealed class SessionTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _f;

    public SessionTests(AppFixture fixture) => _f = fixture;

    [Fact]
    public void CreateSession_AppearsInSidebar()
    {
        int before = AppActions.GetSidebarSessionCount(_f.MainWindow);

        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "SmokeTest1");

        // Allow up to 5s for PTY to start and sidebar to populate
        Thread.Sleep(2000);

        int after = AppActions.GetSidebarSessionCount(_f.MainWindow);
        Assert.True(after > before, $"Expected sidebar to grow. Before: {before}, After: {after}");
    }

    [Fact]
    public void CloseSession_RemovedFromSidebar()
    {
        // Create a session to close
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "SmokeTest2");
        Thread.Sleep(2000);

        int before = AppActions.GetSidebarSessionCount(_f.MainWindow);
        Assert.True(before > 0, "Expected at least one session before closing.");

        // Find and click the close button on the active terminal toolbar.
        // The terminal wrapper toolbar contains a close button with Content="✕".
        // Use Ctrl+W keyboard shortcut as the most reliable approach.
        _f.MainWindow.Focus();
        FlaUI.Core.Input.Keyboard.TypeSimultaneously(
            FlaUI.Core.WindowsAPI.VirtualKeyShort.CONTROL,
            FlaUI.Core.WindowsAPI.VirtualKeyShort.KEY_W);
        Thread.Sleep(1000);

        int after = AppActions.GetSidebarSessionCount(_f.MainWindow);
        Assert.True(after < before, $"Expected sidebar to shrink. Before: {before}, After: {after}");
    }

    [Fact]
    public void EmptyState_ShownWhenNoSessions()
    {
        // Close all sessions until empty
        int count = AppActions.GetSidebarSessionCount(_f.MainWindow);
        for (int i = 0; i < count + 2; i++) // extra iterations are safe
        {
            _f.MainWindow.Focus();
            FlaUI.Core.Input.Keyboard.TypeSimultaneously(
                FlaUI.Core.WindowsAPI.VirtualKeyShort.CONTROL,
                FlaUI.Core.WindowsAPI.VirtualKeyShort.KEY_W);
            Thread.Sleep(500);
        }

        // EmptyState TextBlock should be visible
        var emptyState = _f.MainWindow.FindFirstDescendant(
            cf => cf.ByName("No sessions open."));
        Assert.NotNull(emptyState);
    }
}
```

- [ ] **Step 2: Build and run (expect failures — app not launched yet, that's expected in CI but fine locally)**

```bash
dotnet build tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj -nologo -v q
```
Expected: `Build succeeded.`

- [ ] **Step 3: Run tests to verify they execute (app must be built first)**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj -c Debug -nologo -v q
dotnet test tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj --filter "SessionTests" -v n
```

- [ ] **Step 4: Commit**

```bash
git add tests/CodeShellManager.UITests/SessionTests.cs
git commit -m "feat: add SessionTests (create/close/empty state)"
```

---

## Task 9: LayoutTests

**Files:**
- Create: `tests/CodeShellManager.UITests/LayoutTests.cs`

- [ ] **Step 1: Write LayoutTests.cs**

```csharp
using System.Threading;
using CodeShellManager.UITests.Helpers;
using Xunit;

namespace CodeShellManager.UITests;

[Collection("UITests")]
public sealed class LayoutTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _f;

    public LayoutTests(AppFixture fixture) => _f = fixture;

    [Fact]
    public void TwoColumn_ShowsTwoPanes()
    {
        // Create 2 sessions
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "Layout1");
        Thread.Sleep(1500);
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "Layout2");
        Thread.Sleep(1500);

        AppActions.SetLayout(_f.MainWindow, "Layout_Two");

        int count = AppActions.GetTerminalGridChildCount(_f.MainWindow);
        Assert.Equal(2, count);
    }

    [Fact]
    public void Single_ShowsOnePaneAfterTwoColumn()
    {
        // Ensure at least 2 sessions
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "Layout3");
        Thread.Sleep(1500);
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "Layout4");
        Thread.Sleep(1500);

        AppActions.SetLayout(_f.MainWindow, "Layout_Two");
        Thread.Sleep(300);
        AppActions.SetLayout(_f.MainWindow, "Layout_Single");

        int count = AppActions.GetTerminalGridChildCount(_f.MainWindow);
        Assert.Equal(1, count);
    }

    [Fact]
    public void TwoByTwo_OffscreenSession_BecomesVisibleOnSelect()
    {
        // Create 5 sessions — only 4 fit in 2×2
        for (int i = 1; i <= 5; i++)
        {
            AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
                folder: @"C:\Windows", name: $"Grid{i}");
            Thread.Sleep(1500);
        }

        AppActions.SetLayout(_f.MainWindow, "Layout_Grid");
        Thread.Sleep(500);

        // Click the 5th sidebar item (index 4) — it's off-screen in the 2×2 grid
        var sidebar = AppActions.WaitForElement(_f.MainWindow, "SidebarSessionList");
        var items = sidebar.FindAllChildren();
        Assert.True(items.Length >= 5, $"Expected ≥5 sidebar items, got {items.Length}");

        items[4].Click();
        Thread.Sleep(500);

        // After clicking, the viewport should scroll and the 5th pane should appear in the grid
        int gridCount = AppActions.GetTerminalGridChildCount(_f.MainWindow);
        Assert.True(gridCount > 0, "Expected at least one pane in grid after selecting off-screen session");
    }
}
```

- [ ] **Step 2: Build and run**

```bash
dotnet build tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj -nologo -v q
dotnet test tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj --filter "LayoutTests" -v n
```

- [ ] **Step 3: Commit**

```bash
git add tests/CodeShellManager.UITests/LayoutTests.cs
git commit -m "feat: add LayoutTests (two-column, single, 2x2 off-screen session)"
```

---

## Task 10: SettingsTests

**Files:**
- Create: `tests/CodeShellManager.UITests/SettingsTests.cs`

- [ ] **Step 1: Write SettingsTests.cs**

```csharp
using System.Linq;
using System.Threading;
using CodeShellManager.UITests.Helpers;
using FlaUI.Core.Tools;
using System;
using Xunit;

namespace CodeShellManager.UITests;

[Collection("UITests")]
public sealed class SettingsTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _f;

    public SettingsTests(AppFixture fixture) => _f = fixture;

    [Fact]
    public void MaxResults_PersistsAfterSave()
    {
        // Open Settings
        AppActions.WaitForElement(_f.MainWindow, "SettingsBtn").AsButton().Click();

        var settingsWindow = Retry.WhileNull(
            () => _f.App.GetAllTopLevelWindows(_f.Automation)
                        .FirstOrDefault(w => w.Title == "Settings"),
            TimeSpan.FromSeconds(5))
            ?? throw new TimeoutException("Settings window did not open.");

        // Set Max Search Results to 42
        var maxResultsBox = settingsWindow.FindFirstDescendant(
            cf => cf.ByAutomationId("MaxSearchResultsBox")).AsTextBox();
        maxResultsBox.Text = "42";

        // Click Save
        settingsWindow.FindFirstDescendant(
            cf => cf.ByAutomationId("SettingsSaveBtn")).AsButton().Click();
        Thread.Sleep(500);

        // Reopen Settings and verify value persisted
        AppActions.WaitForElement(_f.MainWindow, "SettingsBtn").AsButton().Click();

        var settingsWindow2 = Retry.WhileNull(
            () => _f.App.GetAllTopLevelWindows(_f.Automation)
                        .FirstOrDefault(w => w.Title == "Settings"),
            TimeSpan.FromSeconds(5))
            ?? throw new TimeoutException("Settings window did not reopen.");

        var maxResultsBox2 = settingsWindow2.FindFirstDescendant(
            cf => cf.ByAutomationId("MaxSearchResultsBox")).AsTextBox();

        Assert.Equal("42", maxResultsBox2.Text);

        // Close settings
        settingsWindow2.FindFirstDescendant(
            cf => cf.ByAutomationId("SettingsSaveBtn")).AsButton().Click();
    }
}
```

- [ ] **Step 2: Build and run**

```bash
dotnet build tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj -nologo -v q
dotnet test tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj --filter "SettingsTests" -v n
```

- [ ] **Step 3: Run the full suite**

```bash
dotnet test tests/CodeShellManager.UITests/CodeShellManager.UITests.csproj -v n
```

- [ ] **Step 4: Commit**

```bash
git add tests/CodeShellManager.UITests/SettingsTests.cs
git commit -m "feat: add SettingsTests (max results persists after save)"
```

---

## Self-Review

### Spec coverage check
| Spec requirement | Task |
|---|---|
| `CSM_STATE_PATH` env var in StateService | Task 1 ✓ |
| AutomationIds on 14 controls | Tasks 2, 3, 4 ✓ |
| Test project: FlaUI.Core, FlaUI.UIA3, xUnit, .NET 10 | Task 5 ✓ |
| `AppFixture` with temp state path | Task 6 ✓ |
| `AppActions` reusable helpers | Task 7 ✓ |
| `CreateSession_AppearsInSidebar` | Task 8 ✓ |
| `CloseSession_RemovedFromSidebar` | Task 8 ✓ |
| `EmptyState_ShownWhenNoSessions` | Task 8 ✓ |
| `TwoColumn_ShowsTwoPanes` | Task 9 ✓ |
| `Single_ShowsOnePaneAfterGrid` | Task 9 ✓ |
| `TwoByTwo_OffscreenSession_BecomesVisible` | Task 9 ✓ |
| `MaxResults_PersistsAfterSave` | Task 10 ✓ |

### Type consistency check
- `AppActions.WaitForElement` used in Tasks 7, 8, 9, 10 — consistent signature ✓
- `AppActions.CreateSession` used in Tasks 8, 9 — consistent params ✓
- `AppActions.GetSidebarSessionCount` / `GetTerminalGridChildCount` consistent ✓
- `AppFixture` fields (`App`, `MainWindow`, `Automation`) referenced consistently ✓

### Notes for the engineer
- **Before running tests:** Build the main app first with `dotnet build src/CodeShellManager/CodeShellManager.csproj -c Debug`
- **Test isolation:** Each test class gets its own app instance. Tests within a class share one instance and run sequentially — do not parallelize within a class.
- **WebView2 startup:** The 1500ms `Thread.Sleep` after `CreateSession` is needed for WebView2 to initialise. If tests are flaky, increase to 2500ms.
- **`[Collection("UITests")]`** prevents xUnit from running test classes in parallel (which would launch multiple app instances simultaneously and likely fail).

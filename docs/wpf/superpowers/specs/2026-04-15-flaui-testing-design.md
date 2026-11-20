# FlaUI UI Test Suite — Design Spec

**Date:** 2026-04-15  
**Status:** Approved  
**Scope:** Local developer smoke tests for the WPF shell using FlaUI + xUnit

---

## Context

CodeShellManager is a WPF .NET 10 multi-terminal host. There are currently no automated tests. This spec covers a first test suite targeting the WPF shell layer (toolbar, sidebar, layout switching, settings dialog). WebView2/xterm.js terminal internals are out of scope for this iteration.

---

## Goals

- Catch regressions in session lifecycle, layout switching, and settings persistence
- Run locally by developers; no CI integration required at this stage
- Use real lightweight shell processes (e.g. `cmd /c pause`) — no mocking of PTY or WebView2

---

## Approach

**Per-class `IClassFixture<AppFixture>`** (xUnit).  
The app launches once per test class, torn down after the class completes. This gives good isolation between domains (sessions, layout, settings) without the cost of per-test launches (~3–5 s each).

State file isolation via `CSM_STATE_PATH` env var — `AppFixture` sets this to a temp file before launch so tests never read the developer's real saved sessions.

---

## Project Structure

```
tests/
  CodeShellManager.UITests/
    CodeShellManager.UITests.csproj     (.NET 10, xUnit, FlaUI.Core, FlaUI.UIA3)
    AppFixture.cs                       launches/tears down app, sets CSM_STATE_PATH
    Helpers/
      AppActions.cs                     reusable helpers (CreateSession, WaitFor, etc.)
    SessionTests.cs                     IClassFixture<AppFixture>
    LayoutTests.cs                      IClassFixture<AppFixture>
    SettingsTests.cs                    IClassFixture<AppFixture>
```

`CodeShellManager.slnx` gets the test project added to it.

---

## AutomationIds

The following `AutomationProperties.AutomationId` values will be added to `MainWindow.xaml` and `NewSessionDialog.xaml`:

| Control | AutomationId |
|---|---|
| New Session toolbar button | `NewSessionBtn` |
| RC All broadcast button | `BroadcastBtn` |
| Sidebar session list panel | `SidebarSessionList` |
| Terminal grid | `TerminalGrid` |
| Alert badge border | `AlertBadge` |
| Search toolbar button | `SearchBtn` |
| Settings toolbar button | `SettingsBtn` |
| Layout: Single button | `Layout_Single` |
| Layout: TwoColumn button | `Layout_Two` |
| Layout: TwoByTwo button | `Layout_Grid` |
| New Session dialog — OK button | `NewSessionOkBtn` |
| New Session dialog — Command ComboBox | `NewSessionCommandCombo` |
| New Session dialog — Folder TextBox | `NewSessionFolderBox` |
| Settings dialog — Save button | `SettingsSaveBtn` |
| Settings dialog — Max Results TextBox | `MaxSearchResultsBox` |

---

## Test Coverage

### SessionTests
- `CreateSession_AppearsInSidebar` — fill New Session dialog with `cmd /c pause`, confirm sidebar item count increases
- `CloseSession_RemovedFromSidebar` — close active session, confirm sidebar item count decreases
- `EmptyState_ShownWhenNoSessions` — after closing all sessions, confirm `EmptyState` element is visible

### LayoutTests
- `TwoColumn_ShowsTwoPanes` — create 2 sessions, switch to TwoColumn, assert TerminalGrid has 2 visible children
- `Single_ShowsOnePaneAfterGrid` — switch from TwoColumn back to Single, assert 1 visible child
- `TwoByTwo_OffscreenSession_BecomesVisible` — create 5 sessions in TwoByTwo, click 5th sidebar item, assert it is placed in TerminalGrid

### SettingsTests
- `MaxResults_PersistsAfterSave` — open Settings, set Max Results to 42, save, reopen Settings, assert value is 42

---

## AppFixture Design

```csharp
public class AppFixture : IDisposable
{
    public Application App { get; }
    public Window MainWindow { get; }
    public UIA3Automation Automation { get; }
    private readonly string _statePath;

    public AppFixture()
    {
        _statePath = Path.GetTempFileName();
        Environment.SetEnvironmentVariable("CSM_STATE_PATH", _statePath);

        Automation = new UIA3Automation();
        string exePath = /* resolve from build output */ ...;
        App = Application.Launch(exePath);
        MainWindow = App.GetMainWindow(Automation, TimeSpan.FromSeconds(10));
    }

    public void Dispose()
    {
        App.Close();
        Automation.Dispose();
        File.Delete(_statePath);
    }
}
```

---

## StateService Change

`StateService` reads `CSM_STATE_PATH` env var when set, falling back to the default `%AppData%/CodeShellManager/state.json`. This is the only change to production code.

---

## Dependencies

| Package | Version |
|---|---|
| `FlaUI.Core` | 4.x |
| `FlaUI.UIA3` | 4.x |
| `xunit` | 2.x |
| `xunit.runner.visualstudio` | 2.x |
| `Microsoft.NET.Test.Sdk` | latest |

---

## Out of Scope

- CI integration (deferred)
- WebView2 / xterm.js layer testing
- Alert/waiting state tests (requires PTY output — deferred)
- Performance or load tests

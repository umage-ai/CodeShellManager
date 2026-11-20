# Per-Session Run Commands ("Play Button") Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a configurable per-session "Run" feature that spawns headless child processes (e.g. `dotnet run`, `npm test`) in the session's working folder, captures their output, and lets the user redirect that output into the main terminal — without polluting the parent PTY (which may be running Claude).

**Architecture:**
- Each `ShellSession` carries a list of `RunCommandItem { Id, Label, CommandLine, IsDefault }`, persisted to `state.json`. The list is seeded from a static template (dotnet/cargo/node/python/make) at session creation, then evolves independently per session.
- Clicking ▶ spawns a **headless `PseudoTerminal`** (no WebView2 attached) hosting `cmd /c "<commandline>"` locally, or a second `ssh -t … "cd … && bash -c '…'"` for SSH parents. Output bytes flow into an in-memory string buffer.
- Process lifecycle is hardened with a Windows Job Object (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) so an app crash kills the whole child tree rather than leaving orphans.
- UI: a `[▶ Run ▼]` button on the terminal toolbar, a chips strip showing per-item state, a slide-down drawer with the captured output, and a `SessionRunCommandsDialog` modal editor reachable from three entry points.
- Send-to-terminal is context-aware: Claude parents get a fenced-with-preamble paste via `Bridge.SendToTerminal`; other parents fall back to clipboard.

**Tech Stack:** C# 13, WPF .NET 10, existing ConPTY-based `PseudoTerminal`, system `ssh`, xunit 2.9 for unit tests.

---

## File Map

| Action | File | Responsibility |
|---|---|---|
| Create | `src/CodeShellManager/Models/RunCommandItem.cs` | Persisted DTO for one configured command |
| Modify | `src/CodeShellManager/Models/ShellSession.cs` | Add `RunCommands: List<RunCommandItem>` and helper `EnsureSingleDefault()` |
| Create | `src/CodeShellManager/Services/RunCommandTemplatesService.cs` | Static project-type detectors + seed lists |
| Create | `src/CodeShellManager/Services/RunInstance.cs` | Runtime state for one in-flight (or last-finished) run |
| Create | `src/CodeShellManager/Services/SessionRunner.cs` | Per-session runner: owns RunInstances, raises change events |
| Modify | `src/CodeShellManager/Terminal/PseudoTerminal.cs` | Add `useJobObject` flag + `ExitCode` capture |
| Modify | `src/CodeShellManager/ViewModels/SessionViewModel.cs` | Add `Runner` property, kill-on-Dispose |
| Modify | `src/CodeShellManager/MainWindow.xaml.cs` | Play button + dropdown + chips strip + drawer; right-click menu wiring; F5 keybinding; seed template on new session; kill-all on close/sleep |
| Modify | `src/CodeShellManager/Views/NewSessionDialog.xaml.cs` | Trigger template seeding after `CreateSession` |
| Create | `src/CodeShellManager/Views/SessionRunCommandsDialog.xaml` | Modal editor XAML |
| Create | `src/CodeShellManager/Views/SessionRunCommandsDialog.xaml.cs` | Modal editor code-behind |
| Modify | `CLAUDE.md` | Document the new feature under a new section |
| Create | `tests/CodeShellManager.Tests/RunCommandItemTests.cs` | `EnsureSingleDefault` semantics |
| Create | `tests/CodeShellManager.Tests/RunCommandTemplatesTests.cs` | Detector + seed list verification |
| Create | `tests/CodeShellManager.Tests/ShellSessionRunCommandsTests.cs` | JSON round-trip of `RunCommands` |

---

## Task 1: `RunCommandItem` model + persistence round-trip test

**Files:**
- Create: `src/CodeShellManager/Models/RunCommandItem.cs`
- Modify: `src/CodeShellManager/Models/ShellSession.cs`
- Create: `tests/CodeShellManager.Tests/RunCommandItemTests.cs`
- Create: `tests/CodeShellManager.Tests/ShellSessionRunCommandsTests.cs`

- [ ] **Step 1: Write the failing tests**

Create `tests/CodeShellManager.Tests/RunCommandItemTests.cs`:

```csharp
using System.Collections.Generic;
using CodeShellManager.Models;
using Xunit;

namespace CodeShellManager.Tests;

public class RunCommandItemTests
{
    [Fact]
    public void EnsureSingleDefault_PromotesFirstWhenNoneMarked()
    {
        var list = new List<RunCommandItem>
        {
            new() { Label = "run",   CommandLine = "dotnet run" },
            new() { Label = "test",  CommandLine = "dotnet test" },
        };
        RunCommandItem.EnsureSingleDefault(list);
        Assert.True(list[0].IsDefault);
        Assert.False(list[1].IsDefault);
    }

    [Fact]
    public void EnsureSingleDefault_KeepsLastTrueWhenMultipleMarked()
    {
        var list = new List<RunCommandItem>
        {
            new() { Label = "a", CommandLine = "x", IsDefault = true },
            new() { Label = "b", CommandLine = "y", IsDefault = true },
            new() { Label = "c", CommandLine = "z" },
        };
        RunCommandItem.EnsureSingleDefault(list);
        // Convention: the LAST-marked default wins (matches the dialog's
        // "click row to promote" behavior — the most recent click is authoritative).
        Assert.False(list[0].IsDefault);
        Assert.True(list[1].IsDefault);
        Assert.False(list[2].IsDefault);
    }

    [Fact]
    public void EnsureSingleDefault_EmptyList_NoOp()
    {
        var list = new List<RunCommandItem>();
        RunCommandItem.EnsureSingleDefault(list);
        Assert.Empty(list);
    }
}
```

Create `tests/CodeShellManager.Tests/ShellSessionRunCommandsTests.cs`:

```csharp
using System.Collections.Generic;
using System.Text.Json;
using CodeShellManager.Models;
using Xunit;

namespace CodeShellManager.Tests;

public class ShellSessionRunCommandsTests
{
    [Fact]
    public void RunCommands_DefaultsToEmptyList()
    {
        var s = new ShellSession();
        Assert.NotNull(s.RunCommands);
        Assert.Empty(s.RunCommands);
    }

    [Fact]
    public void RunCommands_RoundTripsThroughJson()
    {
        var s = new ShellSession
        {
            RunCommands = new List<RunCommandItem>
            {
                new() { Id = "a", Label = "run",  CommandLine = "dotnet run",  IsDefault = true },
                new() { Id = "b", Label = "test", CommandLine = "dotnet test", IsDefault = false },
            }
        };
        string json = JsonSerializer.Serialize(s);
        var back = JsonSerializer.Deserialize<ShellSession>(json)!;
        Assert.Equal(2, back.RunCommands.Count);
        Assert.Equal("a", back.RunCommands[0].Id);
        Assert.True(back.RunCommands[0].IsDefault);
        Assert.Equal("dotnet test", back.RunCommands[1].CommandLine);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
dotnet test tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj --filter "FullyQualifiedName~RunCommandItem|FullyQualifiedName~ShellSessionRunCommands"
```
Expected: FAIL — `RunCommandItem`, `ShellSession.RunCommands`, `EnsureSingleDefault` don't exist.

- [ ] **Step 3: Create the model**

Create `src/CodeShellManager/Models/RunCommandItem.cs`:

```csharp
using System;
using System.Collections.Generic;
using System.Linq;

namespace CodeShellManager.Models;

/// <summary>
/// One configured "run" command on a session. The user can have many of these;
/// exactly one is the default (driven by the toolbar ▶ button and F5 keybinding).
/// Persisted to state.json under <see cref="ShellSession.RunCommands"/>.
/// </summary>
public class RunCommandItem
{
    public string Id { get; set; } = Guid.NewGuid().ToString();
    public string Label { get; set; } = "";
    public string CommandLine { get; set; } = "";
    public bool IsDefault { get; set; }

    /// <summary>
    /// Normalizes the list so exactly one item has IsDefault=true (when non-empty).
    /// If multiple are marked, the LAST one wins — this matches the editor dialog's
    /// "click to promote" UX, where the most recent user action is authoritative.
    /// If none are marked, the first item is promoted.
    /// </summary>
    public static void EnsureSingleDefault(List<RunCommandItem> items)
    {
        if (items.Count == 0) return;

        // Find the LAST item flagged default (or fall back to index 0 if none).
        int keep = -1;
        for (int i = items.Count - 1; i >= 0; i--)
        {
            if (items[i].IsDefault) { keep = i; break; }
        }
        if (keep < 0) keep = 0;

        for (int i = 0; i < items.Count; i++)
            items[i].IsDefault = (i == keep);
    }
}
```

- [ ] **Step 4: Add `RunCommands` to `ShellSession`**

Modify `src/CodeShellManager/Models/ShellSession.cs` — add a new property near the bottom of the field block (after `ProfileColorSchemeJson`, before `FullCommandLine`):

```csharp
/// <summary>
/// Configured run commands for this session — the source for the toolbar ▶ button
/// and the chips strip. Seeded at session creation from <see cref="Services.RunCommandTemplatesService"/>.
/// Exactly one item has IsDefault=true (when the list is non-empty);
/// see <see cref="RunCommandItem.EnsureSingleDefault"/>.
/// </summary>
public List<RunCommandItem> RunCommands { get; set; } = new();
```

Add `using System.Collections.Generic;` at the top if not already present.

- [ ] **Step 5: Run tests to verify they pass**

```bash
dotnet test tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj --filter "FullyQualifiedName~RunCommandItem|FullyQualifiedName~ShellSessionRunCommands"
```
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add src/CodeShellManager/Models/RunCommandItem.cs src/CodeShellManager/Models/ShellSession.cs tests/CodeShellManager.Tests/RunCommandItemTests.cs tests/CodeShellManager.Tests/ShellSessionRunCommandsTests.cs
git commit -m "feat: add RunCommandItem model with single-default invariant"
```

---

## Task 2: Project-type templates (`RunCommandTemplatesService`)

**Files:**
- Create: `src/CodeShellManager/Services/RunCommandTemplatesService.cs`
- Create: `tests/CodeShellManager.Tests/RunCommandTemplatesTests.cs`

- [ ] **Step 1: Write the failing tests**

Create `tests/CodeShellManager.Tests/RunCommandTemplatesTests.cs`:

```csharp
using System.IO;
using System.Linq;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class RunCommandTemplatesTests : System.IDisposable
{
    private readonly string _tmp;

    public RunCommandTemplatesTests()
    {
        _tmp = Path.Combine(Path.GetTempPath(), "csm-tpl-" + System.Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_tmp);
    }

    public void Dispose() { try { Directory.Delete(_tmp, recursive: true); } catch { } }

    [Fact]
    public void Detect_DotnetProject_ReturnsDotnetTemplate()
    {
        File.WriteAllText(Path.Combine(_tmp, "MyApp.csproj"), "<Project/>");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.NotNull(seed);
        Assert.Equal("dotnet", seed!.ProjectType);
        Assert.Contains(seed.Items, i => i.CommandLine == "dotnet run" && i.IsDefault);
        Assert.Contains(seed.Items, i => i.CommandLine == "dotnet build");
        Assert.Contains(seed.Items, i => i.CommandLine == "dotnet test");
    }

    [Fact]
    public void Detect_CargoProject_ReturnsCargoTemplate()
    {
        File.WriteAllText(Path.Combine(_tmp, "Cargo.toml"), "[package]");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("cargo", seed!.ProjectType);
        Assert.Contains(seed.Items, i => i.CommandLine == "cargo run" && i.IsDefault);
    }

    [Fact]
    public void Detect_NodeProject_DefaultsToNpm()
    {
        File.WriteAllText(Path.Combine(_tmp, "package.json"), "{}");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("node", seed!.ProjectType);
        Assert.Contains(seed.Items, i => i.CommandLine == "npm start" && i.IsDefault);
    }

    [Fact]
    public void Detect_NodeProject_WithPnpmLock_UsesPnpm()
    {
        File.WriteAllText(Path.Combine(_tmp, "package.json"), "{}");
        File.WriteAllText(Path.Combine(_tmp, "pnpm-lock.yaml"), "");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Contains(seed!.Items, i => i.CommandLine.StartsWith("pnpm "));
    }

    [Fact]
    public void Detect_NodeProject_WithYarnLock_UsesYarn()
    {
        File.WriteAllText(Path.Combine(_tmp, "package.json"), "{}");
        File.WriteAllText(Path.Combine(_tmp, "yarn.lock"), "");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Contains(seed!.Items, i => i.CommandLine.StartsWith("yarn"));
    }

    [Fact]
    public void Detect_NodeProject_WithBunLockb_UsesBun()
    {
        File.WriteAllText(Path.Combine(_tmp, "package.json"), "{}");
        File.WriteAllText(Path.Combine(_tmp, "bun.lockb"), "");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Contains(seed!.Items, i => i.CommandLine.StartsWith("bun "));
    }

    [Fact]
    public void Detect_PythonProject_ReturnsPythonTemplate()
    {
        File.WriteAllText(Path.Combine(_tmp, "pyproject.toml"), "[project]");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("python", seed!.ProjectType);
    }

    [Fact]
    public void Detect_Makefile_ReturnsMakeTemplate()
    {
        File.WriteAllText(Path.Combine(_tmp, "Makefile"), "run:");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("make", seed!.ProjectType);
    }

    [Fact]
    public void Detect_DotnetBeatsCargo()
    {
        // Both markers present — dotnet template wins because it's first in the priority list.
        File.WriteAllText(Path.Combine(_tmp, "MyApp.csproj"), "<Project/>");
        File.WriteAllText(Path.Combine(_tmp, "Cargo.toml"), "[package]");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("dotnet", seed!.ProjectType);
    }

    [Fact]
    public void Detect_EmptyFolder_ReturnsNull()
    {
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Null(seed);
    }

    [Fact]
    public void Detect_NonexistentFolder_ReturnsNull()
    {
        var seed = RunCommandTemplatesService.SeedFor(Path.Combine(_tmp, "does-not-exist"));
        Assert.Null(seed);
    }

    [Fact]
    public void Detect_NoRecursiveScan_IgnoresMatchInSubfolder()
    {
        var sub = Path.Combine(_tmp, "subdir");
        Directory.CreateDirectory(sub);
        File.WriteAllText(Path.Combine(sub, "MyApp.csproj"), "<Project/>");
        // Top-level is empty → must NOT detect.
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Null(seed);
    }

    [Fact]
    public void SeedItems_HaveExactlyOneDefault()
    {
        File.WriteAllText(Path.Combine(_tmp, "MyApp.csproj"), "<Project/>");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal(1, seed!.Items.Count(i => i.IsDefault));
    }

    [Fact]
    public void SeedItems_HaveFreshIds()
    {
        File.WriteAllText(Path.Combine(_tmp, "MyApp.csproj"), "<Project/>");
        var a = RunCommandTemplatesService.SeedFor(_tmp)!.Items;
        var b = RunCommandTemplatesService.SeedFor(_tmp)!.Items;
        // Two seedings of the same folder must produce DIFFERENT ids — otherwise
        // multiple sessions on the same folder would share RunCommandItem.Id, which
        // collides in the per-session RunInstance dictionary.
        Assert.NotEqual(a[0].Id, b[0].Id);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
dotnet test tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj --filter "FullyQualifiedName~RunCommandTemplates"
```
Expected: FAIL — `RunCommandTemplatesService` doesn't exist.

- [ ] **Step 3: Create the service**

Create `src/CodeShellManager/Services/RunCommandTemplatesService.cs`:

```csharp
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// One project-type template — a label plus a seed list of run commands that will
/// be COPIED into a new session's <see cref="ShellSession.RunCommands"/> on creation.
/// </summary>
public record RunCommandTemplate(string ProjectType, IReadOnlyList<RunCommandItem> Items);

/// <summary>
/// Resolves a working folder to the matching project-type template (first match wins).
/// Detection is non-recursive (top-level files only) and runs once at session creation.
/// </summary>
public static class RunCommandTemplatesService
{
    /// <summary>
    /// Returns the matching template with fresh (new-Guid) item Ids, or null if no
    /// detector matched (empty folder, unknown project type, or non-existent path).
    /// </summary>
    public static RunCommandTemplate? SeedFor(string workingFolder)
    {
        if (string.IsNullOrWhiteSpace(workingFolder) || !Directory.Exists(workingFolder))
            return null;

        // Enumerate ONCE — repeated File.Exists is slow on network shares.
        // EnumerateFiles is non-recursive by default.
        HashSet<string> files;
        try
        {
            files = new HashSet<string>(
                Directory.EnumerateFiles(workingFolder).Select(p => Path.GetFileName(p) ?? ""),
                StringComparer.OrdinalIgnoreCase);
        }
        catch { return null; }

        bool Has(string name) => files.Contains(name);
        bool HasExt(string ext) => files.Any(f => f.EndsWith(ext, StringComparison.OrdinalIgnoreCase));

        // Priority order: dotnet → cargo → node → python → make. First match wins.
        if (HasExt(".sln") || HasExt(".csproj"))
            return Build("dotnet",
                ("Run",   "dotnet run",   isDefault: true),
                ("Build", "dotnet build", isDefault: false),
                ("Test",  "dotnet test",  isDefault: false));

        if (Has("Cargo.toml"))
            return Build("cargo",
                ("Run",    "cargo run",    isDefault: true),
                ("Build",  "cargo build",  isDefault: false),
                ("Test",   "cargo test",   isDefault: false),
                ("Clippy", "cargo clippy", isDefault: false));

        if (Has("package.json"))
        {
            string pm =
                Has("pnpm-lock.yaml") ? "pnpm"
              : Has("yarn.lock")      ? "yarn"
              : Has("bun.lockb")      ? "bun"
              : "npm";

            // yarn's invocation differs slightly: `yarn start` (no `run`) is conventional.
            string runPrefix = pm == "yarn" ? "yarn" : $"{pm} run";
            return Build("node",
                ("Start", $"{pm} start",         isDefault: true),
                ("Test",  $"{pm} test",          isDefault: false),
                ("Build", $"{runPrefix} build",  isDefault: false));
        }

        if (Has("pyproject.toml") || Has("requirements.txt"))
            return Build("python",
                ("Run",  "python main.py",     isDefault: true),
                ("Test", "python -m pytest",   isDefault: false));

        if (Has("Makefile") || Has("makefile"))
            return Build("make",
                ("Run",   "make",       isDefault: true),
                ("Test",  "make test",  isDefault: false),
                ("Clean", "make clean", isDefault: false));

        return null;
    }

    private static RunCommandTemplate Build(string projectType, params (string Label, string Cmd, bool IsDefault)[] items)
    {
        var list = items.Select(t => new RunCommandItem
        {
            Id = Guid.NewGuid().ToString(),
            Label = t.Label,
            CommandLine = t.Cmd,
            IsDefault = t.IsDefault,
        }).ToList();
        return new RunCommandTemplate(projectType, list);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
dotnet test tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj --filter "FullyQualifiedName~RunCommandTemplates"
```
Expected: PASS (13 tests).

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Services/RunCommandTemplatesService.cs tests/CodeShellManager.Tests/RunCommandTemplatesTests.cs
git commit -m "feat: add project-type templates (dotnet/cargo/node/python/make)"
```

---

## Task 3: Job Object + ExitCode in `PseudoTerminal`

**Files:**
- Modify: `src/CodeShellManager/Terminal/PseudoTerminal.cs`

**Why this change:** spawning `cmd /c "dotnet test"` creates a tree (cmd → dotnet → testhost). Disposing the PTY closes `cmd`'s handle but the dotnet/testhost children may not die — they get orphaned and keep consuming the user's machine. A Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` kills the whole tree when we close the job handle. We also need `ExitCode` so the chip can render ✓ vs ✗.

**This task adds capabilities but the existing call sites (line 579 in MainWindow) are unchanged because the new parameter defaults to false.**

- [ ] **Step 1: Add P/Invoke + struct declarations**

In `src/CodeShellManager/Terminal/PseudoTerminal.cs`, in the P/Invoke section (around line 56, just before the `Structs` comment), add:

```csharp
[DllImport("kernel32.dll", SetLastError = true)]
private static extern IntPtr CreateJobObject(IntPtr lpJobAttributes, string? lpName);

[DllImport("kernel32.dll", SetLastError = true)]
private static extern bool SetInformationJobObject(IntPtr hJob, int JobObjectInfoClass,
    ref JOBOBJECT_EXTENDED_LIMIT_INFORMATION lpJobObjectInfo, int cbJobObjectInfoLength);

[DllImport("kernel32.dll", SetLastError = true)]
private static extern bool AssignProcessToJobObject(IntPtr hJob, IntPtr hProcess);

[DllImport("kernel32.dll", SetLastError = true)]
private static extern uint ResumeThread(IntPtr hThread);

[DllImport("kernel32.dll", SetLastError = true)]
private static extern bool GetExitCodeProcess(IntPtr hProcess, out uint lpExitCode);
```

In the `Structs` section (around line 56), add:

```csharp
[StructLayout(LayoutKind.Sequential)]
private struct JOBOBJECT_BASIC_LIMIT_INFORMATION
{
    public long PerProcessUserTimeLimit;
    public long PerJobUserTimeLimit;
    public uint LimitFlags;
    public IntPtr MinimumWorkingSetSize;
    public IntPtr MaximumWorkingSetSize;
    public uint ActiveProcessLimit;
    public IntPtr Affinity;
    public uint PriorityClass;
    public uint SchedulingClass;
}

[StructLayout(LayoutKind.Sequential)]
private struct IO_COUNTERS
{
    public ulong ReadOperationCount, WriteOperationCount, OtherOperationCount;
    public ulong ReadTransferCount, WriteTransferCount, OtherTransferCount;
}

[StructLayout(LayoutKind.Sequential)]
private struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION
{
    public JOBOBJECT_BASIC_LIMIT_INFORMATION BasicLimitInformation;
    public IO_COUNTERS IoInfo;
    public IntPtr ProcessMemoryLimit;
    public IntPtr JobMemoryLimit;
    public IntPtr PeakProcessMemoryUsed;
    public IntPtr PeakJobMemoryUsed;
}
```

In the constants block (around line 85), add:

```csharp
private const uint CREATE_SUSPENDED = 0x00000004;
private const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000;
private const int JobObjectExtendedLimitInformation = 9;
```

- [ ] **Step 2: Add `_hJob` and `ExitCode` fields**

In the `Fields` section, add after `_hProcess`:

```csharp
private IntPtr _hJob = IntPtr.Zero;

/// <summary>
/// Process exit code, populated once <see cref="Exited"/> fires.
/// Null while the process is still running. Uint cast to int (Windows exit codes can be negative).
/// </summary>
public int? ExitCode { get; private set; }
```

- [ ] **Step 3: Add the `useJobObject` parameter to `Start`**

Replace the signature of `Start` (line 120):

```csharp
public void Start(string command, string args, string workingDirectory,
    int cols = 220, int rows = 50, bool useJobObject = false)
```

Inside `Start`, after the `if (!CreateProcess(...))` block but BEFORE `_hProcess = pi.hProcess;`, insert the job-object branch:

```csharp
// Build CreateProcess flags. When useJobObject=true we add CREATE_SUSPENDED so
// we can attach the new process to the Job Object before it starts spawning children.
uint creationFlags = EXTENDED_STARTUPINFO_PRESENT;
if (useJobObject) creationFlags |= CREATE_SUSPENDED;
```

…and update the `CreateProcess` call to use `creationFlags` instead of `EXTENDED_STARTUPINFO_PRESENT`:

```csharp
if (!CreateProcess(null, cmdLine, IntPtr.Zero, IntPtr.Zero, false,
        creationFlags, IntPtr.Zero, workDir, ref si, out var pi))
    throw new InvalidOperationException($"CreateProcess failed: {Marshal.GetLastWin32Error()}");
```

Then between `_hProcess = pi.hProcess;` and `CloseHandle(pi.hThread);`, insert:

```csharp
if (useJobObject)
{
    _hJob = CreateJobObject(IntPtr.Zero, null);
    if (_hJob == IntPtr.Zero)
        throw new InvalidOperationException("CreateJobObject failed");

    var limits = new JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    {
        BasicLimitInformation = new JOBOBJECT_BASIC_LIMIT_INFORMATION
        {
            LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        }
    };
    if (!SetInformationJobObject(_hJob, JobObjectExtendedLimitInformation,
            ref limits, Marshal.SizeOf<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()))
        throw new InvalidOperationException("SetInformationJobObject failed");

    if (!AssignProcessToJobObject(_hJob, _hProcess))
        throw new InvalidOperationException("AssignProcessToJobObject failed");

    // Process was started suspended — resume it now that it's in the job.
    ResumeThread(pi.hThread);
}
```

- [ ] **Step 4: Capture exit code in `MonitorExitAsync`**

Replace the body of `MonitorExitAsync` (line 247):

```csharp
private async Task MonitorExitAsync()
{
    await Task.Run(() => WaitForSingleObject(_hProcess, 0xFFFFFFFF));
    if (_hProcess != IntPtr.Zero && GetExitCodeProcess(_hProcess, out uint code))
        ExitCode = unchecked((int)code);
    Exited?.Invoke();
}
```

- [ ] **Step 5: Close the job in `Dispose`**

In `Dispose` (line 253), add — BEFORE the `ClosePseudoConsole` call — a close for `_hJob`. Closing the job handle is what triggers `KILL_ON_JOB_CLOSE`, so this MUST run before `_hProcess` is closed (otherwise the close-order doesn't guarantee child death):

```csharp
if (_hJob != IntPtr.Zero) { CloseHandle(_hJob); _hJob = IntPtr.Zero; }
```

So the modified `Dispose` looks like:

```csharp
public void Dispose()
{
    if (_disposed) return;
    _disposed = true;
    _cts.Cancel();
    _stdin?.Dispose();
    _stdout?.Dispose();
    _inputRead?.Dispose();
    _inputWrite?.Dispose();
    _outputRead?.Dispose();
    _outputWrite?.Dispose();
    if (_hJob != IntPtr.Zero) { CloseHandle(_hJob); _hJob = IntPtr.Zero; }
    if (_hPC != IntPtr.Zero) { ClosePseudoConsole(_hPC); _hPC = IntPtr.Zero; }
    if (_hProcess != IntPtr.Zero) { CloseHandle(_hProcess); _hProcess = IntPtr.Zero; }
}
```

- [ ] **Step 6: Build to verify it compiles**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
```
Expected: build succeeds. Existing session-launch path is unchanged (default `useJobObject: false`), so all session behavior must remain identical.

- [ ] **Step 7: Commit**

```bash
git add src/CodeShellManager/Terminal/PseudoTerminal.cs
git commit -m "feat(pty): add Job Object support and ExitCode capture for child runs"
```

---

## Task 4: `RunInstance` — single-run runtime state

**Files:**
- Create: `src/CodeShellManager/Services/RunInstance.cs`

`RunInstance` owns one in-flight (or last-finished) execution of one `RunCommandItem`. It wraps a headless `PseudoTerminal`, accumulates its output in a string buffer, and exposes observable state for the UI.

- [ ] **Step 1: Create the class**

Create `src/CodeShellManager/Services/RunInstance.cs`:

```csharp
using System;
using System.IO;
using System.Text;
using System.Text.RegularExpressions;
using CodeShellManager.Models;
using CodeShellManager.Terminal;
using CommunityToolkit.Mvvm.ComponentModel;

namespace CodeShellManager.Services;

public enum RunState
{
    Idle,           // never started, or output cleared
    Running,
    ExitedOk,       // exit code 0
    ExitedFailed,   // exit code != 0
}

/// <summary>
/// Runtime state for one invocation of a <see cref="RunCommandItem"/>.
/// Owns a headless <see cref="PseudoTerminal"/> and accumulates output into
/// a string buffer for display in the drawer / sending to the parent terminal.
/// NOT persisted to state.json.
/// </summary>
public partial class RunInstance : ObservableObject, IDisposable
{
    private const int MaxBufferChars = 1_000_000; // ~1MB ceiling; older content is dropped from the head

    public string ItemId { get; }
    public string Label { get; }
    public string CommandLine { get; }

    [ObservableProperty] private RunState _state = RunState.Idle;
    [ObservableProperty] private int? _exitCode;
    [ObservableProperty] private string _outputBuffer = "";
    [ObservableProperty] private DateTime? _startedAt;
    [ObservableProperty] private DateTime? _endedAt;

    public TimeSpan? Duration => StartedAt is { } s && EndedAt is { } e ? e - s : null;

    private PseudoTerminal? _pty;
    private readonly StringBuilder _ansiStripped = new();
    private readonly object _bufLock = new();
    private bool _disposed;

    public event Action? OutputChanged;
    public event Action? StateChanged;

    public RunInstance(RunCommandItem item)
    {
        ItemId = item.Id;
        Label = item.Label;
        CommandLine = item.CommandLine;
    }

    /// <summary>
    /// Spawns the child PTY. Builds the command line based on whether the parent
    /// is local or remote — see <see cref="BuildLocalCmd"/> / <see cref="BuildSshArgs"/>.
    /// </summary>
    public void Start(ShellSession parent)
    {
        if (_pty != null) throw new InvalidOperationException("Already started — call Dispose() first.");

        lock (_bufLock) { _ansiStripped.Clear(); }
        OutputBuffer = "";
        ExitCode = null;
        StartedAt = DateTime.Now;
        EndedAt = null;
        State = RunState.Running;
        StateChanged?.Invoke();

        _pty = new PseudoTerminal();
        _pty.DataReceived += OnPtyData;
        _pty.Exited += OnPtyExited;

        string command, args, workDir;
        if (parent.IsRemote)
        {
            command = "ssh";
            args = BuildSshArgs(parent, CommandLine);
            workDir = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }
        else
        {
            command = "cmd";
            args = BuildLocalCmd(CommandLine);
            workDir = Directory.Exists(parent.WorkingFolder)
                ? parent.WorkingFolder
                : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }

        _pty.Start(command, args, workDir, cols: 200, rows: 50, useJobObject: true);
    }

    public void Stop()
    {
        // Disposing the PTY closes the Job Object → kills the whole process tree.
        Dispose();
    }

    private void OnPtyData(string text)
    {
        // Strip ANSI for the readonly drawer view + clipboard. Match the
        // OutputIndexer regex so any visible quirks stay consistent across the app.
        string stripped = AnsiPattern().Replace(text, "");
        lock (_bufLock)
        {
            _ansiStripped.Append(stripped);
            if (_ansiStripped.Length > MaxBufferChars)
                _ansiStripped.Remove(0, _ansiStripped.Length - MaxBufferChars);
        }
        // Marshal to UI thread is the consumer's responsibility — OutputChanged
        // fires from the PTY read loop's thread.
        OutputChanged?.Invoke();
    }

    private void OnPtyExited()
    {
        EndedAt = DateTime.Now;
        ExitCode = _pty?.ExitCode;
        State = ExitCode == 0 ? RunState.ExitedOk : RunState.ExitedFailed;
        StateChanged?.Invoke();
    }

    /// <summary>
    /// Snapshots the current ANSI-stripped buffer. Thread-safe.
    /// </summary>
    public string SnapshotOutput()
    {
        lock (_bufLock) return _ansiStripped.ToString();
    }

    /// <summary>
    /// Wraps a single-statement CommandLine for cmd.exe so &amp;&amp;, pipes,
    /// redirects, and quoted args all parse correctly. The outer cmd /c
    /// exits when the wrapped process exits — needed for clean Exited firing.
    /// </summary>
    internal static string BuildLocalCmd(string commandLine) => $"/c \"{commandLine}\"";

    /// <summary>
    /// Builds ssh args for a remote run. Pattern:
    ///   -p PORT -t user@host "cd '/folder' &amp;&amp; bash -c '<escaped>'"
    /// </summary>
    internal static string BuildSshArgs(ShellSession parent, string commandLine)
    {
        var sb = new StringBuilder();
        if (parent.SshPort != 22) sb.Append($"-p {parent.SshPort} ");
        sb.Append("-t ");
        sb.Append(string.IsNullOrWhiteSpace(parent.SshUser)
            ? parent.SshHost
            : $"{parent.SshUser}@{parent.SshHost}");
        sb.Append(" \"");
        if (!string.IsNullOrWhiteSpace(parent.SshRemoteFolder))
            sb.Append($"cd '{parent.SshRemoteFolder}' && ");
        sb.Append("bash -c ");
        sb.Append(SingleQuoteEscape(commandLine));
        sb.Append("\"");
        return sb.ToString();
    }

    /// <summary>
    /// POSIX single-quote escape: wraps in single quotes, replacing any inner
    /// single quote with '\'' so the shell still receives the literal char.
    /// E.g. <c>can't do</c> → <c>'can'\''t do'</c>.
    /// </summary>
    internal static string SingleQuoteEscape(string s) => "'" + s.Replace("'", "'\\''") + "'";

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        if (_pty != null)
        {
            _pty.DataReceived -= OnPtyData;
            _pty.Exited -= OnPtyExited;
            _pty.Dispose();
            _pty = null;
        }
        // If we were killed externally before the child exited naturally,
        // mark as failed with no exit code (it didn't get to report one).
        if (State == RunState.Running)
        {
            EndedAt = DateTime.Now;
            State = RunState.ExitedFailed;
            StateChanged?.Invoke();
        }
    }

    [GeneratedRegex(@"\x1B\[[0-9;]*[mGKHFJABCDsuhl]|\x1B\].*?\x07|\x1B[=>]|\r", RegexOptions.Compiled)]
    private static partial Regex AnsiPattern();
}
```

- [ ] **Step 2: Add escape-helper unit tests**

Append to `tests/CodeShellManager.Tests/RunCommandTemplatesTests.cs` (same file is fine, or create a new `RunInstanceTests.cs` — but the escape logic is the only easily-unit-testable bit; the rest needs a real child process). Create a separate file:

Create `tests/CodeShellManager.Tests/RunInstanceTests.cs`:

```csharp
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class RunInstanceTests
{
    [Fact]
    public void SingleQuoteEscape_PlainString_WrapsInQuotes()
    {
        Assert.Equal("'dotnet test'", RunInstance.SingleQuoteEscape("dotnet test"));
    }

    [Fact]
    public void SingleQuoteEscape_ContainsSingleQuote_EscapesIt()
    {
        // can't → 'can'\''t'
        Assert.Equal(@"'can'\''t'", RunInstance.SingleQuoteEscape("can't"));
    }

    [Fact]
    public void SingleQuoteEscape_Empty_ReturnsEmptyPair()
    {
        Assert.Equal("''", RunInstance.SingleQuoteEscape(""));
    }

    [Fact]
    public void BuildLocalCmd_WrapsForCmd()
    {
        Assert.Equal("/c \"dotnet test --filter X\"", RunInstance.BuildLocalCmd("dotnet test --filter X"));
    }

    [Fact]
    public void BuildSshArgs_LocalFolder_BuildsExpectedShape()
    {
        var p = new ShellSession
        {
            IsRemote = true, SshUser = "alice", SshHost = "dev.example.com",
            SshPort = 22, SshRemoteFolder = "/proj",
        };
        string args = RunInstance.BuildSshArgs(p, "cargo test");
        Assert.Equal("-t alice@dev.example.com \"cd '/proj' && bash -c 'cargo test'\"", args);
    }

    [Fact]
    public void BuildSshArgs_NonDefaultPort_IncludesPortFlag()
    {
        var p = new ShellSession
        {
            IsRemote = true, SshUser = "bob", SshHost = "h", SshPort = 2222, SshRemoteFolder = "",
        };
        string args = RunInstance.BuildSshArgs(p, "ls");
        Assert.StartsWith("-p 2222 ", args);
    }

    [Fact]
    public void BuildSshArgs_CommandLineWithApostrophe_IsEscaped()
    {
        var p = new ShellSession
        {
            IsRemote = true, SshUser = "u", SshHost = "h", SshPort = 22, SshRemoteFolder = "/p",
        };
        string args = RunInstance.BuildSshArgs(p, "echo it's me");
        Assert.Contains(@"bash -c 'echo it'\''s me'", args);
    }
}
```

- [ ] **Step 3: Run tests**

```bash
dotnet test tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj --filter "FullyQualifiedName~RunInstance"
```
Expected: PASS (7 tests).

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/Services/RunInstance.cs tests/CodeShellManager.Tests/RunInstanceTests.cs
git commit -m "feat: add RunInstance — headless PTY wrapper with output buffer"
```

---

## Task 5: `SessionRunner` — per-session run coordinator

**Files:**
- Create: `src/CodeShellManager/Services/SessionRunner.cs`
- Modify: `src/CodeShellManager/ViewModels/SessionViewModel.cs`

`SessionRunner` owns the per-item `RunInstance` map for one session. The UI binds to it for the chips strip and dropdown.

- [ ] **Step 1: Create the runner**

Create `src/CodeShellManager/Services/SessionRunner.cs`:

```csharp
using System;
using System.Collections.Generic;
using System.Linq;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// Owns the per-item run state for one session. One <see cref="RunInstance"/>
/// per <see cref="RunCommandItem.Id"/>; running an item again disposes the prior
/// instance and creates a fresh one (kill-and-restart semantics).
/// </summary>
public class SessionRunner : IDisposable
{
    private readonly ShellSession _session;
    private readonly Dictionary<string, RunInstance> _instances = new();

    /// <summary>Fires when any instance is added, replaced, or removed, or any state changes.</summary>
    public event Action? InstancesChanged;

    public SessionRunner(ShellSession session) { _session = session; }

    public IReadOnlyDictionary<string, RunInstance> Instances => _instances;

    public RunInstance? GetInstance(string itemId) =>
        _instances.TryGetValue(itemId, out var inst) ? inst : null;

    /// <summary>
    /// Starts (or restarts) a run for the given item. If a prior instance exists,
    /// it is disposed first (which kills the child process tree).
    /// </summary>
    public RunInstance Run(RunCommandItem item)
    {
        if (_instances.TryGetValue(item.Id, out var existing))
        {
            existing.StateChanged -= OnInstanceStateChanged;
            existing.OutputChanged -= OnInstanceOutputChanged;
            existing.Dispose();
            _instances.Remove(item.Id);
        }

        var inst = new RunInstance(item);
        inst.StateChanged += OnInstanceStateChanged;
        inst.OutputChanged += OnInstanceOutputChanged;
        _instances[item.Id] = inst;
        inst.Start(_session);
        InstancesChanged?.Invoke();
        return inst;
    }

    /// <summary>
    /// Stops (kills) the run for the given item. The instance is kept around so
    /// the chip still shows the failed/cancelled state — call <see cref="Dismiss"/>
    /// to remove it entirely.
    /// </summary>
    public void Stop(string itemId)
    {
        if (_instances.TryGetValue(itemId, out var inst))
        {
            inst.Stop();
            InstancesChanged?.Invoke();
        }
    }

    /// <summary>
    /// Removes the instance entirely (kills if still running, then forgets it).
    /// The chip disappears; next click on the item starts fresh.
    /// </summary>
    public void Dismiss(string itemId)
    {
        if (_instances.TryGetValue(itemId, out var inst))
        {
            inst.StateChanged -= OnInstanceStateChanged;
            inst.OutputChanged -= OnInstanceOutputChanged;
            inst.Dispose();
            _instances.Remove(itemId);
            InstancesChanged?.Invoke();
        }
    }

    /// <summary>Kills every running child. Called on parent session close / sleep / app exit.</summary>
    public void StopAll()
    {
        foreach (var inst in _instances.Values.ToList())
        {
            inst.StateChanged -= OnInstanceStateChanged;
            inst.OutputChanged -= OnInstanceOutputChanged;
            inst.Dispose();
        }
        _instances.Clear();
        InstancesChanged?.Invoke();
    }

    private void OnInstanceStateChanged() => InstancesChanged?.Invoke();
    private void OnInstanceOutputChanged() => InstancesChanged?.Invoke();

    public void Dispose() => StopAll();
}
```

- [ ] **Step 2: Wire the runner into `SessionViewModel`**

Modify `src/CodeShellManager/ViewModels/SessionViewModel.cs`:

Add a property after the existing observable properties (around line 28):

```csharp
public SessionRunner Runner { get; }
```

In the constructor (line 73), initialize it:

```csharp
public SessionViewModel(ShellSession session)
{
    Session = session;
    Runner = new SessionRunner(session);
    _ = RefreshGitInfoAsync();
    _ = PollGitInfoAsync(_gitPollCts.Token);
}
```

Update `Dispose` (line 151) to kill all runs first:

```csharp
public void Dispose()
{
    Runner.Dispose();
    _gitPollCts.Cancel();
    _gitPollCts.Dispose();
    AlertDetector?.Dispose();
    OutputIndexer?.Dispose();
    Bridge?.Dispose();
    Pty?.Dispose();
}
```

- [ ] **Step 3: Build to verify the project compiles**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
```
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/Services/SessionRunner.cs src/CodeShellManager/ViewModels/SessionViewModel.cs
git commit -m "feat: add SessionRunner — per-session run coordinator"
```

---

## Task 6: Seed `RunCommands` on new session creation

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

When a new session is created via the New Session dialog (`OpenNewSessionDialog`) or via "New session here…" / "Duplicate session" / "New worktree from this branch…", we seed `RunCommands` from `RunCommandTemplatesService` if the session is local and the list is currently empty. SSH sessions and sessions with already-populated lists are skipped.

- [ ] **Step 1: Add a helper method on `MainWindow`**

In `MainWindow.xaml.cs`, near the other `SessionManager`-adjacent helpers (e.g. near `DuplicateSessionAsync` around line 437), add:

```csharp
/// <summary>
/// Stamps the session's RunCommands list from the matching project-type template,
/// if the list is currently empty AND the session is local (not SSH). Runs on a
/// background task so the UI doesn't block on folder enumeration. No-op if the
/// folder doesn't match any template.
/// </summary>
private void SeedRunCommandsAsync(Models.ShellSession session)
{
    if (session.IsRemote) return;
    if (session.RunCommands.Count > 0) return;
    if (string.IsNullOrWhiteSpace(session.WorkingFolder)) return;

    string folder = session.WorkingFolder;
    _ = System.Threading.Tasks.Task.Run(() =>
    {
        var template = Services.RunCommandTemplatesService.SeedFor(folder);
        if (template == null) return;
        Dispatcher.Invoke(() =>
        {
            // Re-check on UI thread — the user may have edited the list manually
            // while we were scanning (race with the editor dialog).
            if (session.RunCommands.Count == 0)
            {
                foreach (var item in template.Items)
                    session.RunCommands.Add(item);
                _ = _vm.SaveStateAsync();
                RefreshTerminalRunControls(session.Id);
            }
        });
    });
}
```

The `RefreshTerminalRunControls` call references a method we'll define in Task 7 — for now it can be a stub:

Add this stub next to `SeedRunCommandsAsync`:

```csharp
/// <summary>
/// Rebuilds the play button / chips strip for a single session.
/// Stub for Task 6 — fully implemented in Task 7.
/// </summary>
private void RefreshTerminalRunControls(string sessionId) { /* implemented in Task 7 */ }
```

- [ ] **Step 2: Call the seeder from every new-session entry point**

Find each `await LaunchSessionAsync(...)` call that creates a NEW (not restored) session and add `SeedRunCommandsAsync(...)` just BEFORE it:

| Approximate line | Context | Add before the line |
|---|---|---|
| 383 | `await LaunchSessionAsync(primary);` — after `OpenNewSessionDialog` accepts | `SeedRunCommandsAsync(primary);` |
| 415 | Sibling-worktree spawn | `SeedRunCommandsAsync(sibling);` |
| 455 | `DuplicateSessionAsync` clone path | `SeedRunCommandsAsync(clone);` — but ALSO copy parent's RunCommands first; see next sub-step |
| 505 | `LaunchSessionInSiblingWorktreeAsync` | `SeedRunCommandsAsync(sibling);` |
| 2014 | After "New session here…" dialog | `SeedRunCommandsAsync(newSession);` |
| 3159 | `OpenNewWorktreeDialogAsync` accepted path | `SeedRunCommandsAsync(newSession);` |

For `DuplicateSessionAsync` specifically (line 455 area), prefer COPYING the parent's RunCommands instead of re-detecting:

```csharp
// Copy parent's run commands with fresh Ids so the duplicate has its own list.
foreach (var item in vm.Session.RunCommands)
{
    clone.RunCommands.Add(new Models.RunCommandItem
    {
        Id = System.Guid.NewGuid().ToString(),
        Label = item.Label,
        CommandLine = item.CommandLine,
        IsDefault = item.IsDefault,
    });
}
// If the parent had no commands, fall back to detection.
if (clone.RunCommands.Count == 0) SeedRunCommandsAsync(clone);
```

Place this block between `var clone = _sessionManager.CreateSession(...)` and `await LaunchSessionAsync(clone);` — read the surrounding code to find the right insertion point.

- [ ] **Step 3: Build and smoke-test**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
```
Expected: builds.

Run the app:
```bash
dotnet run --project src/CodeShellManager/CodeShellManager.csproj
```
- Open a new session on a folder containing a `*.csproj`.
- Close the app. Inspect `%AppData%/CodeShellManager/state.json` — the session entry should now have a `RunCommands` array with three items (`dotnet run` / `build` / `test`).
- Open a new session on a folder with no project files. `RunCommands` should be empty in state.json.

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat: seed RunCommands from project-type template on new session"
```

---

## Task 7: Toolbar ▶ button + dropdown + chips strip + drawer

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

This is the bulk of the UI work. The play button, dropdown menu, chips strip, and drawer are all built imperatively inside `BuildTerminalWrapper` (following the existing notes-panel pattern).

**Layout in the terminal wrapper from top to bottom:**

```
┌──────────────────────────────────────────────────┐
│ toolbar:  badge | name | folder ... [▶▼] [📁][📝][💤][📍] │
│ chips strip (visible when any RunInstance exists)│
│   [● dotnet watch] [✓ dotnet test] [✗ build]     │
│ drawer (visible when a chip is selected)         │
│   [⏹ Stop] [📋 Copy] [↗ Send to terminal]        │
│   <output text in monospace, scrollable>         │
│ notesPanel (existing)                            │
│ webView (existing)                               │
└──────────────────────────────────────────────────┘
```

- [ ] **Step 1: Track run-control UI per session**

At the top of the `MainWindow` class fields (near `_dormantSidebarItems` around line 48), add a dictionary that lets `RefreshTerminalRunControls` find the controls to rebuild:

```csharp
/// <summary>
/// Per-session references to the run-related controls inside the terminal wrapper.
/// Used by RefreshTerminalRunControls() to update the play button / chips strip
/// when the session's RunCommands list or its RunInstances change.
/// </summary>
private readonly Dictionary<string, (
    System.Windows.Controls.Button playBtn,
    System.Windows.Controls.Button chevronBtn,
    System.Windows.Controls.Border chipsStrip,
    System.Windows.Controls.StackPanel chipsPanel,
    System.Windows.Controls.Border drawer,
    System.Windows.Controls.TextBox drawerText,
    System.Windows.Controls.TextBlock drawerHeader,
    System.Windows.Controls.Button drawerStopBtn,
    System.Windows.Controls.Button drawerCopyBtn,
    System.Windows.Controls.Button drawerSendBtn)> _runControls = new();
```

(Plus a per-session "currently-viewed-in-drawer" item id:)

```csharp
private readonly Dictionary<string, string> _drawerItemBySession = new();
```

- [ ] **Step 2: Build the play button + chevron in `BuildTerminalWrapper`**

In `BuildTerminalWrapper` (around line 2699, where `sleepBtn` is created), insert BEFORE the `sleepBtn` block:

```csharp
// ── Play (run) button + chevron ──────────────────────────────────────
var playBtn = new WpfButton
{
    Content = "▶",
    ToolTip = "Run the default command (F5)",
    Background = Brushes.Transparent,
    BorderThickness = new Thickness(0),
    Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xe3, 0xa1)),  // green ▶
    FontSize = 12,
    Cursor = System.Windows.Input.Cursors.Hand,
    Padding = new Thickness(4, 2, 2, 2),
    Margin = new Thickness(0, 0, 0, 0),
    Visibility = vm.Session.RunCommands.Count == 0 ? Visibility.Collapsed : Visibility.Visible,
};
playBtn.Click += (_, _) => RunDefaultCommand(vm);
playBtn.MouseRightButtonUp += (_, _) => OpenRunCommandsEditor(vm);

var chevronBtn = new WpfButton
{
    Content = "▼",
    ToolTip = "Run commands…",
    Background = Brushes.Transparent,
    BorderThickness = new Thickness(0),
    Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
    FontSize = 9,
    Cursor = System.Windows.Input.Cursors.Hand,
    Padding = new Thickness(2, 2, 4, 2),
    Margin = new Thickness(0, 0, 4, 0),
    Visibility = playBtn.Visibility,
};
chevronBtn.Click += (_, _) => ShowRunCommandsDropdown(vm, chevronBtn);
```

Then update the `DockPanel.SetDock` block to dock both new buttons to the right (insert just after `DockPanel.SetDock(sleepBtn, Dock.Right);`):

```csharp
DockPanel.SetDock(chevronBtn, Dock.Right);
DockPanel.SetDock(playBtn, Dock.Right);
```

And add them to `toolbarContent.Children` AFTER the `sleepBtn` add:

```csharp
toolbarContent.Children.Add(chevronBtn);
toolbarContent.Children.Add(playBtn);
```

(Order matters because DockPanel Right items pack right-to-left: the FIRST added is rightmost. We want `[▶][▼]…[💤][📝][>_][📁][●]` left-to-right, which means status dot added first, … then ▼ then ▶ near the left edge of the right cluster — adjust ordering by experimentation when running.)

- [ ] **Step 3: Build the chips strip + drawer panels**

Below the `notesPanel` definition (around line 2745), insert:

```csharp
// ── Chips strip ─────────────────────────────────────────────────────
var chipsPanel = new StackPanel
{
    Orientation = Orientation.Horizontal,
};
var chipsStrip = new Border
{
    Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25)),
    BorderBrush = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
    BorderThickness = new Thickness(0, 0, 0, 1),
    Padding = new Thickness(8, 2, 8, 2),
    Visibility = Visibility.Collapsed,  // shown only when at least one RunInstance exists
    Child = new ScrollViewer
    {
        HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
        VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
        Content = chipsPanel,
    },
};

// ── Drawer ──────────────────────────────────────────────────────────
var drawerHeader = new TextBlock
{
    Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
    FontSize = 11,
    FontWeight = FontWeights.SemiBold,
    VerticalAlignment = VerticalAlignment.Center,
    Margin = new Thickness(0, 0, 8, 0),
};
var drawerStopBtn = MakeDrawerActionButton("⏹ Stop");
var drawerCopyBtn = MakeDrawerActionButton("📋 Copy");
var drawerSendBtn = MakeDrawerActionButton("↗ Send to terminal");

var drawerActions = new DockPanel { LastChildFill = false };
DockPanel.SetDock(drawerHeader, Dock.Left);
DockPanel.SetDock(drawerStopBtn, Dock.Right);
DockPanel.SetDock(drawerCopyBtn, Dock.Right);
DockPanel.SetDock(drawerSendBtn, Dock.Right);
drawerActions.Children.Add(drawerHeader);
drawerActions.Children.Add(drawerSendBtn);
drawerActions.Children.Add(drawerCopyBtn);
drawerActions.Children.Add(drawerStopBtn);

var drawerText = new WpfTextBox
{
    Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25)),
    Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
    BorderThickness = new Thickness(0),
    IsReadOnly = true,
    AcceptsReturn = true,
    VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
    FontFamily = new FontFamily("Consolas"),
    FontSize = 12,
    Padding = new Thickness(8, 6, 8, 6),
    TextWrapping = TextWrapping.NoWrap,
};

var drawerInner = new DockPanel();
DockPanel.SetDock(drawerActions, Dock.Top);
drawerInner.Children.Add(drawerActions);
drawerInner.Children.Add(drawerText);

var drawer = new Border
{
    Background = new SolidColorBrush(Color.FromRgb(0x11, 0x11, 0x1b)),
    BorderBrush = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
    BorderThickness = new Thickness(0, 0, 0, 1),
    Height = 200,
    Visibility = Visibility.Collapsed,
    Child = drawerInner,
};

drawerStopBtn.Click += (_, _) =>
{
    if (_drawerItemBySession.TryGetValue(vm.Id, out var itemId))
        vm.Runner.Stop(itemId);
};
drawerCopyBtn.Click += (_, _) =>
{
    if (_drawerItemBySession.TryGetValue(vm.Id, out var itemId) &&
        vm.Runner.GetInstance(itemId) is { } inst)
    {
        string text = !string.IsNullOrEmpty(drawerText.SelectedText)
            ? drawerText.SelectedText
            : inst.SnapshotOutput();
        try { System.Windows.Clipboard.SetText(text); } catch { }
    }
};
drawerSendBtn.Click += (_, _) => SendRunOutputToTerminal(vm, drawerText);
```

Add `using System.Windows.Controls;` if not already imported (it is — confirm at top of file).

- [ ] **Step 4: Insert the chips strip and drawer into the vertical stack**

In `BuildTerminalWrapper`, after the existing `outer.Children.Add(notesPanel);` line (around line 2791), the existing order is `toolbar → notesPanel → webView`. Change it so the chips strip and drawer sit BETWEEN the toolbar and notesPanel:

```csharp
DockPanel.SetDock(toolbar, Dock.Top);
DockPanel.SetDock(chipsStrip, Dock.Top);
DockPanel.SetDock(drawer, Dock.Top);
DockPanel.SetDock(notesPanel, Dock.Top);
outer.Children.Add(toolbar);
outer.Children.Add(chipsStrip);
outer.Children.Add(drawer);
outer.Children.Add(notesPanel);
outer.Children.Add(webView);
```

- [ ] **Step 5: Register the controls and subscribe to runner events**

Just before `return activeRing;` at the end of `BuildTerminalWrapper`, add:

```csharp
_runControls[vm.Id] = (playBtn, chevronBtn, chipsStrip, chipsPanel, drawer,
    drawerText, drawerHeader, drawerStopBtn, drawerCopyBtn, drawerSendBtn);

vm.Runner.InstancesChanged += () => Dispatcher.Invoke(() => RefreshTerminalRunControls(vm.Id));
```

- [ ] **Step 6: Implement the helper methods**

Below `BuildTerminalWrapper`, add the helpers:

```csharp
private static System.Windows.Controls.Button MakeDrawerActionButton(string label) => new()
{
    Content = label,
    Background = Brushes.Transparent,
    BorderThickness = new Thickness(0),
    Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
    FontSize = 11,
    Cursor = System.Windows.Input.Cursors.Hand,
    Padding = new Thickness(8, 4, 8, 4),
};

/// <summary>
/// Rebuilds chips + play-button visibility + drawer content for one session.
/// Idempotent — safe to call from every InstancesChanged event.
/// </summary>
private void RefreshTerminalRunControls(string sessionId)
{
    if (!_runControls.TryGetValue(sessionId, out var c)) return;
    var vm = _vm.Sessions.FirstOrDefault(s => s.Id == sessionId);
    if (vm == null) return;

    // Play / chevron visibility — driven by whether the list has anything to run.
    var vis = vm.Session.RunCommands.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    c.playBtn.Visibility = vis;
    c.chevronBtn.Visibility = vis;

    // Rebuild chips strip.
    c.chipsPanel.Children.Clear();
    var instances = vm.Runner.Instances;
    foreach (var (itemId, inst) in instances)
    {
        var chip = BuildRunChip(vm, inst);
        c.chipsPanel.Children.Add(chip);
    }
    c.chipsStrip.Visibility = instances.Count > 0 ? Visibility.Visible : Visibility.Collapsed;

    // Update drawer if a viewed item exists.
    if (_drawerItemBySession.TryGetValue(sessionId, out var viewedItemId) &&
        vm.Runner.GetInstance(viewedItemId) is { } viewedInst)
    {
        c.drawerHeader.Text = $"{viewedInst.Label} — {DescribeState(viewedInst)}";
        c.drawerText.Text = viewedInst.SnapshotOutput();
        // Auto-scroll to the end while the run is active.
        if (viewedInst.State == RunState.Running)
            c.drawerText.ScrollToEnd();
        c.drawerStopBtn.IsEnabled = viewedInst.State == RunState.Running;
    }
    else
    {
        // Viewed item disappeared (was dismissed). Hide the drawer.
        c.drawer.Visibility = Visibility.Collapsed;
        _drawerItemBySession.Remove(sessionId);
    }
}

private static string DescribeState(RunInstance inst) => inst.State switch
{
    RunState.Idle => "idle",
    RunState.Running => "running…",
    RunState.ExitedOk => $"finished (exit 0, {inst.Duration?.TotalSeconds:F1}s)",
    RunState.ExitedFailed => $"failed (exit {inst.ExitCode?.ToString() ?? "?"})",
    _ => "?",
};

private System.Windows.Controls.Border BuildRunChip(SessionViewModel vm, RunInstance inst)
{
    (Color fill, Color text) ColorsFor(RunState s) => s switch
    {
        RunState.Running       => (Color.FromRgb(0x89, 0xb4, 0xfa), Color.FromRgb(0x18, 0x18, 0x25)),
        RunState.ExitedOk      => (Color.FromRgb(0xa6, 0xe3, 0xa1), Color.FromRgb(0x18, 0x18, 0x25)),
        RunState.ExitedFailed  => (Color.FromRgb(0xf3, 0x8b, 0xa8), Color.FromRgb(0x18, 0x18, 0x25)),
        _                      => (Color.FromRgb(0x45, 0x47, 0x5a), Color.FromRgb(0xcd, 0xd6, 0xf4)),
    };
    string Icon(RunState s) => s switch
    {
        RunState.Running => "●",
        RunState.ExitedOk => "✓",
        RunState.ExitedFailed => "✗",
        _ => "▶",
    };
    var (fillC, textC) = ColorsFor(inst.State);

    var chip = new System.Windows.Controls.Border
    {
        Background = new SolidColorBrush(fillC),
        CornerRadius = new CornerRadius(10),
        Padding = new Thickness(8, 2, 4, 2),
        Margin = new Thickness(0, 0, 6, 0),
        Cursor = System.Windows.Input.Cursors.Hand,
    };
    var sp = new System.Windows.Controls.StackPanel { Orientation = Orientation.Horizontal };
    sp.Children.Add(new TextBlock
    {
        Text = $"{Icon(inst.State)} {inst.Label}",
        Foreground = new SolidColorBrush(textC),
        FontSize = 11,
        VerticalAlignment = VerticalAlignment.Center,
        Margin = new Thickness(0, 0, 4, 0),
    });
    var dismiss = new WpfButton
    {
        Content = "✕",
        Background = Brushes.Transparent,
        BorderThickness = new Thickness(0),
        Foreground = new SolidColorBrush(textC),
        FontSize = 9,
        Padding = new Thickness(2, 0, 2, 0),
        Cursor = System.Windows.Input.Cursors.Hand,
        ToolTip = "Dismiss",
    };
    dismiss.Click += (_, _) => vm.Runner.Dismiss(inst.ItemId);
    sp.Children.Add(dismiss);
    chip.Child = sp;

    chip.MouseLeftButtonUp += (_, _) => ToggleDrawer(vm, inst.ItemId);
    return chip;
}

private void ToggleDrawer(SessionViewModel vm, string itemId)
{
    if (!_runControls.TryGetValue(vm.Id, out var c)) return;
    if (_drawerItemBySession.TryGetValue(vm.Id, out var current) && current == itemId
        && c.drawer.Visibility == Visibility.Visible)
    {
        c.drawer.Visibility = Visibility.Collapsed;
        _drawerItemBySession.Remove(vm.Id);
    }
    else
    {
        _drawerItemBySession[vm.Id] = itemId;
        c.drawer.Visibility = Visibility.Visible;
        RefreshTerminalRunControls(vm.Id);
    }
}

private void RunDefaultCommand(SessionViewModel vm)
{
    var def = vm.Session.RunCommands.FirstOrDefault(i => i.IsDefault);
    if (def == null) return;
    vm.Runner.Run(def);
}

private void ShowRunCommandsDropdown(SessionViewModel vm, System.Windows.Controls.Button anchor)
{
    var menu = new System.Windows.Controls.ContextMenu
    {
        PlacementTarget = anchor,
        Placement = System.Windows.Controls.Primitives.PlacementMode.Bottom,
    };
    foreach (var item in vm.Session.RunCommands)
    {
        var label = item.IsDefault ? $"▶ {item.Label} (default)" : $"▶ {item.Label}";
        var mi = new System.Windows.Controls.MenuItem { Header = label };
        mi.Click += (_, _) => vm.Runner.Run(item);
        menu.Items.Add(mi);
    }
    menu.Items.Add(new System.Windows.Controls.Separator());
    var edit = new System.Windows.Controls.MenuItem { Header = "Edit commands…" };
    edit.Click += (_, _) => OpenRunCommandsEditor(vm);
    menu.Items.Add(edit);
    menu.IsOpen = true;
}

private void OpenRunCommandsEditor(SessionViewModel vm)
{
    // Implemented in Task 9.
    System.Windows.MessageBox.Show("Editor coming in Task 9", "TODO");
}

private void SendRunOutputToTerminal(SessionViewModel vm, System.Windows.Controls.TextBox drawerText)
{
    if (!_drawerItemBySession.TryGetValue(vm.Id, out var itemId)) return;
    var inst = vm.Runner.GetInstance(itemId);
    if (inst == null) return;

    string text = !string.IsNullOrEmpty(drawerText.SelectedText)
        ? drawerText.SelectedText
        : inst.SnapshotOutput();
    if (string.IsNullOrWhiteSpace(text)) return;

    bool isClaude = ClaudeSessionService.IsClaudeCommand(vm.Session.Command);
    if (isClaude && vm.Bridge != null)
    {
        string exit = inst.ExitCode is { } code ? $" (exit code {code})" : "";
        // No trailing \r — leave it in Claude's input box for the user to submit.
        string wrapped = $"\nOutput of `{inst.CommandLine}`{exit}:\n```\n{text}\n```\n";
        vm.Bridge.SendToTerminal(wrapped);
        ToastHelper.Show("Sent to Claude", $"{text.Length} chars wrapped in fence");
    }
    else
    {
        // Non-Claude shell: clipboard fallback to avoid auto-execution.
        try { System.Windows.Clipboard.SetText(text); } catch { }
        ToastHelper.Show("Sent to clipboard", "Paste with Ctrl+V to be safe");
    }
}
```

Replace the existing stub `RefreshTerminalRunControls(string sessionId) { /* implemented in Task 7 */ }` you added in Task 6 — it's now superseded by the real method above.

- [ ] **Step 7: Clean up `_runControls` and `_drawerItemBySession` on session removal**

Look for the spots that today remove session UI: `SleepSession` (line ~2851) and the close-path in `MainViewModel.OnSessionCloseRequested` (search for `_sessionUi.Remove`). After each `_sessionUi.Remove(vm.Id);`, add:

```csharp
_runControls.Remove(vm.Id);
_drawerItemBySession.Remove(vm.Id);
```

- [ ] **Step 8: Build + smoke test**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
dotnet run --project src/CodeShellManager/CodeShellManager.csproj
```

- Open a session on a .NET folder. The toolbar should show `[▶][▼]`.
- Click ▶ — a chip appears, drawer can be opened by clicking the chip, output streams in.
- Click ⏹ in drawer — chip turns red (failed) since we forced exit.
- Click 📋 — toast says "Copied N chars" (verify clipboard contents).
- Send-to-terminal: untested without Claude — verify the warning toast path works on a `pwsh` session.

- [ ] **Step 9: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat: add Run toolbar button, chips strip, and output drawer"
```

---

## Task 8: F5 / Shift+F5 global keybindings

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

- [ ] **Step 1: Add cases to `TryHandleGlobalShortcut`**

In `MainWindow.xaml.cs` `TryHandleGlobalShortcut` (line 3436), insert before `return false;`:

```csharp
if (key == Key.F5 && mods == ModifierKeys.None)
{
    if (_vm.ActiveSession != null) RunDefaultCommand(_vm.ActiveSession);
    return true;
}
if (key == Key.F5 && mods == ModifierKeys.Shift)
{
    if (_vm.ActiveSession is { } vm)
    {
        var def = vm.Session.RunCommands.FirstOrDefault(i => i.IsDefault);
        if (def != null) vm.Runner.Stop(def.Id);
    }
    return true;
}
```

- [ ] **Step 2: Smoke test**

Build and run; press F5 in an active session with run commands — default command starts.

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat: F5 runs default command, Shift+F5 stops it"
```

---

## Task 9: `SessionRunCommandsDialog` modal editor

**Files:**
- Create: `src/CodeShellManager/Views/SessionRunCommandsDialog.xaml`
- Create: `src/CodeShellManager/Views/SessionRunCommandsDialog.xaml.cs`
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

Modal dialog with inline-edit rows, drag-reorder via up/down buttons (drag-and-drop in WPF ListBox is doable but verbose; up/down buttons are simpler and equally usable for short lists), a default-radio column, and Cancel/Save.

- [ ] **Step 1: Create the XAML**

Create `src/CodeShellManager/Views/SessionRunCommandsDialog.xaml`:

```xml
<Window x:Class="CodeShellManager.Views.SessionRunCommandsDialog"
        xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
        xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
        Title="Run commands"
        Width="640" Height="460"
        WindowStartupLocation="CenterOwner"
        Background="#1e1e2e"
        Foreground="#cdd6f4"
        ResizeMode="CanResizeWithGrip">
    <DockPanel Margin="14">
        <TextBlock x:Name="TitleText" DockPanel.Dock="Top" FontSize="16" FontWeight="SemiBold" Margin="0,0,0,8"/>
        <TextBlock DockPanel.Dock="Top" FontSize="11" Foreground="#6c7086" Margin="0,0,0,10"
                   Text="Commands run in this session's folder. Use &amp;&amp; / | / &gt; as in any shell. Select the radio to set the default (F5 / toolbar ▶)."
                   TextWrapping="Wrap"/>

        <StackPanel DockPanel.Dock="Bottom" Orientation="Horizontal" HorizontalAlignment="Right" Margin="0,10,0,0">
            <Button x:Name="CancelBtn" Content="Cancel" Width="80" Margin="0,0,8,0" Click="Cancel_Click"/>
            <Button x:Name="SaveBtn"   Content="Save"   Width="80" IsDefault="True" Click="Save_Click"
                    Background="#89b4fa" Foreground="#1e1e2e" BorderThickness="0"/>
        </StackPanel>

        <Button x:Name="AddBtn" DockPanel.Dock="Bottom" Content="+ Add command" HorizontalAlignment="Left"
                Background="Transparent" BorderBrush="#45475a" BorderThickness="1" Padding="10,4"
                Foreground="#cdd6f4" Margin="0,8,0,0" Click="Add_Click"/>

        <ScrollViewer VerticalScrollBarVisibility="Auto">
            <ItemsControl x:Name="RowsList">
                <ItemsControl.ItemTemplate>
                    <DataTemplate>
                        <Border BorderBrush="#313244" BorderThickness="0,0,0,1" Padding="0,6,0,6">
                            <Grid>
                                <Grid.ColumnDefinitions>
                                    <ColumnDefinition Width="Auto"/>
                                    <ColumnDefinition Width="*"/>
                                    <ColumnDefinition Width="2*"/>
                                    <ColumnDefinition Width="Auto"/>
                                </Grid.ColumnDefinitions>
                                <RadioButton Grid.Column="0" GroupName="DefaultGroup" Margin="0,0,8,0"
                                             IsChecked="{Binding IsDefault, Mode=TwoWay}"
                                             VerticalAlignment="Center" Foreground="#cdd6f4"
                                             ToolTip="Set as default (toolbar ▶ / F5)"/>
                                <TextBox Grid.Column="1" Text="{Binding Label, UpdateSourceTrigger=PropertyChanged}"
                                         Background="#313244" Foreground="#cdd6f4" BorderBrush="#45475a"
                                         CaretBrush="#cdd6f4" Padding="4,2" Margin="0,0,6,0"/>
                                <TextBox Grid.Column="2" Text="{Binding CommandLine, UpdateSourceTrigger=PropertyChanged}"
                                         Background="#313244" Foreground="#cdd6f4" BorderBrush="#45475a"
                                         CaretBrush="#cdd6f4" Padding="4,2" Margin="0,0,6,0"
                                         FontFamily="Consolas"/>
                                <StackPanel Grid.Column="3" Orientation="Horizontal">
                                    <Button Content="▲" Tag="{Binding}" Click="MoveUp_Click" Width="24"
                                            Background="Transparent" BorderThickness="0" Foreground="#a6adc8" Cursor="Hand"/>
                                    <Button Content="▼" Tag="{Binding}" Click="MoveDown_Click" Width="24"
                                            Background="Transparent" BorderThickness="0" Foreground="#a6adc8" Cursor="Hand"/>
                                    <Button Content="🗑" Tag="{Binding}" Click="Delete_Click" Width="24"
                                            Background="Transparent" BorderThickness="0" Foreground="#a6adc8" Cursor="Hand"
                                            ToolTip="Delete"/>
                                </StackPanel>
                            </Grid>
                        </Border>
                    </DataTemplate>
                </ItemsControl.ItemTemplate>
            </ItemsControl>
        </ScrollViewer>
    </DockPanel>
</Window>
```

- [ ] **Step 2: Create the code-behind**

Create `src/CodeShellManager/Views/SessionRunCommandsDialog.xaml.cs`:

```csharp
using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Windows;
using CodeShellManager.Models;

namespace CodeShellManager.Views;

public partial class SessionRunCommandsDialog : Window
{
    private readonly ObservableCollection<RunCommandRow> _rows = new();

    /// <summary>The new list to write back to ShellSession.RunCommands. Populated on Save.</summary>
    public List<RunCommandItem>? Result { get; private set; }

    public SessionRunCommandsDialog(string sessionName, IReadOnlyList<RunCommandItem> initial)
    {
        InitializeComponent();
        TitleText.Text = $"Run commands for \"{sessionName}\"";
        foreach (var item in initial)
        {
            _rows.Add(new RunCommandRow
            {
                Id = item.Id,
                Label = item.Label,
                CommandLine = item.CommandLine,
                IsDefault = item.IsDefault,
            });
        }
        RowsList.ItemsSource = _rows;
    }

    private void Add_Click(object sender, RoutedEventArgs e)
    {
        _rows.Add(new RunCommandRow
        {
            Id = Guid.NewGuid().ToString(),
            Label = "",
            CommandLine = "",
            IsDefault = _rows.Count == 0,
        });
    }

    private void Delete_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.Tag is RunCommandRow row)
            _rows.Remove(row);
    }

    private void MoveUp_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.Tag is RunCommandRow row)
        {
            int i = _rows.IndexOf(row);
            if (i > 0) _rows.Move(i, i - 1);
        }
    }

    private void MoveDown_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.Tag is RunCommandRow row)
        {
            int i = _rows.IndexOf(row);
            if (i >= 0 && i < _rows.Count - 1) _rows.Move(i, i + 1);
        }
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        // Validate: every row needs label + commandline.
        foreach (var r in _rows)
        {
            if (string.IsNullOrWhiteSpace(r.Label) || string.IsNullOrWhiteSpace(r.CommandLine))
            {
                MessageBox.Show("Every row needs both a label and a command line.",
                    "Validation", MessageBoxButton.OK, MessageBoxImage.Warning);
                return;
            }
        }

        var list = _rows.Select(r => new RunCommandItem
        {
            Id = r.Id,
            Label = r.Label.Trim(),
            CommandLine = r.CommandLine.Trim(),
            IsDefault = r.IsDefault,
        }).ToList();
        RunCommandItem.EnsureSingleDefault(list);

        Result = list;
        DialogResult = true;
        Close();
    }

    /// <summary>
    /// Backing class for the bound grid. Has to implement INotifyPropertyChanged so
    /// the RadioButton's TwoWay binding can clear sibling rows on selection.
    /// </summary>
    private class RunCommandRow : System.ComponentModel.INotifyPropertyChanged
    {
        public string Id { get; set; } = "";
        private string _label = "";
        private string _commandLine = "";
        private bool _isDefault;
        public string Label
        {
            get => _label;
            set { _label = value; OnChanged(nameof(Label)); }
        }
        public string CommandLine
        {
            get => _commandLine;
            set { _commandLine = value; OnChanged(nameof(CommandLine)); }
        }
        public bool IsDefault
        {
            get => _isDefault;
            set { _isDefault = value; OnChanged(nameof(IsDefault)); }
        }

        public event System.ComponentModel.PropertyChangedEventHandler? PropertyChanged;
        private void OnChanged(string n) => PropertyChanged?.Invoke(this,
            new System.ComponentModel.PropertyChangedEventArgs(n));
    }
}
```

- [ ] **Step 3: Replace the stub `OpenRunCommandsEditor` in `MainWindow.xaml.cs`**

Replace the body of `OpenRunCommandsEditor` (added in Task 7) with the real implementation:

```csharp
private void OpenRunCommandsEditor(SessionViewModel vm)
{
    var dlg = new Views.SessionRunCommandsDialog(vm.DisplayName, vm.Session.RunCommands)
    {
        Owner = this,
    };
    if (dlg.ShowDialog() == true && dlg.Result != null)
    {
        vm.Session.RunCommands.Clear();
        foreach (var item in dlg.Result)
            vm.Session.RunCommands.Add(item);
        _ = _vm.SaveStateAsync();
        RefreshTerminalRunControls(vm.Id);
    }
}
```

- [ ] **Step 4: Build + smoke test**

```bash
dotnet build src/CodeShellManager/CodeShellManager.csproj
dotnet run --project src/CodeShellManager/CodeShellManager.csproj
```

- Right-click the ▶ button → dialog opens with the current list.
- Add a row, set its label + command, click Save. Close the dialog. Toolbar now shows two items in the chevron dropdown.
- Reorder with up/down buttons; promote a different row to default by clicking its radio. Save. The toolbar ▶ now runs the newly-promoted default.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Views/SessionRunCommandsDialog.xaml src/CodeShellManager/Views/SessionRunCommandsDialog.xaml.cs src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat: add per-session run commands editor dialog"
```

---

## Task 10: Sidebar right-click "Session commands" submenu

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

Augments `BuildSessionContextMenu` (line ~1837) to insert a "Session commands" submenu near the top of the menu (before "Sleep"). For single-target right-clicks only — multi-select operations skip it.

- [ ] **Step 1: Insert the submenu in `BuildSessionContextMenu`**

In `BuildSessionContextMenu` (around line 1906, just before `menu.Items.Add(new System.Windows.Controls.Separator());` that precedes the Sleep item), add inside the `if (!isMulti)` branch (which already exists for single-target items):

```csharp
// Session commands submenu — only for single targets.
var runMenu = new System.Windows.Controls.MenuItem { Header = "Session commands" };
if (vm.Session.RunCommands.Count == 0)
{
    runMenu.Items.Add(new System.Windows.Controls.MenuItem
    {
        Header = "(none configured)",
        IsEnabled = false,
    });
}
else
{
    foreach (var item in vm.Session.RunCommands)
    {
        string lbl = item.IsDefault ? $"▶ {item.Label} (default)" : $"▶ {item.Label}";
        var mi = new System.Windows.Controls.MenuItem { Header = lbl };
        mi.Click += (_, _) => vm.Runner.Run(item);
        runMenu.Items.Add(mi);
    }
}
runMenu.Items.Add(new System.Windows.Controls.Separator());
var editMi = new System.Windows.Controls.MenuItem { Header = "Edit commands…" };
editMi.Click += (_, _) => OpenRunCommandsEditor(vm);
runMenu.Items.Add(editMi);
menu.Items.Add(runMenu);
```

- [ ] **Step 2: Smoke test**

Build, run, right-click a sidebar entry. Confirm the "Session commands" submenu appears with the configured items + "Edit commands…" at the bottom.

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat: sidebar right-click adds 'Session commands' submenu"
```

---

## Task 11: Kill all runs on parent close / sleep / app exit

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

Per design: every exit path kills all of the session's runs. Most of this is FREE because `SessionViewModel.Dispose()` already calls `Runner.Dispose()` (Task 5) which kills all instances. We only need to verify the three exit paths actually dispose the VM. They do today — but we should audit:

- **Close** (✕): `OnSessionCloseRequested` in `MainViewModel` → calls `vm.Dispose()`. ✓
- **Sleep** (💤): `SleepSession` line 2870 → `vm.Dispose()`. ✓
- **App close** (`OnClosing` line 3474): iterates `_vm.Sessions` and calls `vm.Dispose()` on each. ✓

- [ ] **Step 1: Add an explicit `StopAll` call in `SleepSession` for clarity**

Sleep is the trickiest case because the `ShellSession` is kept (just IsDormant=true). Without explicit kill, a future bug that disposes only the PTY could leave runs orphaned. Add a defensive `vm.Runner.StopAll()` call near the top of `SleepSession` (line 2851) — before any UI cleanup:

```csharp
private void SleepSession(SessionViewModel vm)
{
    vm.Runner.StopAll();  // kills child processes before tearing down UI
    var session = vm.Session;
    session.IsDormant = true;
    // ...rest unchanged
}
```

- [ ] **Step 2: Manual lifecycle test**

Run the app:
- Start a long-running command (e.g. `cmd /c "ping localhost -t"` configured as a run command).
- Verify the chip is `● running`.
- Click 💤 sleep on the session. Open Task Manager → no orphan `ping.exe` or `cmd.exe`. ✓
- Repeat with ✕ close. ✓
- Repeat: start the ping, then close the entire CSM window. No orphans. ✓
- (Optional, harder test) Kill CSM via Task Manager while a run is active. The Job Object should kill children when the handle is reclaimed at process exit. Verify no orphans.

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat: kill active runs on session sleep (explicit) + verify close paths"
```

---

## Task 12: Documentation update in `CLAUDE.md`

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add a new section "Per-Session Run Commands"**

Add to `CLAUDE.md` after the "Sleep / Wake (Dormant Sessions)" section:

```markdown
## Per-Session Run Commands

Each session can have a list of "run commands" — labelled command lines invoked by the toolbar ▶ button, the F5 keybinding, or the sidebar right-click submenu. Runs spawn a **separate headless `PseudoTerminal`** in the session's working folder (or a fresh `ssh` connection for SSH parents); they do **not** type into the parent PTY, so a Claude session is untouched.

**Data:** `ShellSession.RunCommands: List<RunCommandItem> { Id, Label, CommandLine, IsDefault }`. Exactly one item has `IsDefault=true`; see `RunCommandItem.EnsureSingleDefault`. Persisted to `state.json`.

**Templates:** `RunCommandTemplatesService.SeedFor(folder)` detects project type (top-level scan, first-match: dotnet → cargo → node → python → make) and returns a seed list with fresh Ids. Templates are *copied* onto new sessions at creation time; subsequent edits don't propagate back. SSH sessions skip detection (empty list).

**Runtime:** `SessionRunner` (one per `SessionViewModel`) owns a dictionary of `RunInstance` keyed by item Id. Each `RunInstance` wraps a `PseudoTerminal` started with `useJobObject: true` so the whole child tree dies when the PTY is disposed (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`). Output is captured to an ANSI-stripped string buffer (capped at 1MB). Not persisted.

**UI:**
- Toolbar `[▶][▼]` next to 💤. Hidden when `RunCommands` is empty.
- Chips strip between toolbar and terminal — one chip per active/finished run, color-coded (blue=running, green=ok, pink=failed). Click a chip to open the drawer; ✕ on a chip to dismiss it.
- Drawer (slide-down panel, like Notes) shows the selected run's output with `[⏹ Stop] [📋 Copy] [↗ Send to terminal]`.
- **Send to terminal:** for Claude parents (`ClaudeSessionService.IsClaudeCommand`), wraps in fenced preamble and writes to PTY (no trailing `\r`). For non-Claude shells, falls back to clipboard with a toast — auto-paste would risk executing pasted lines.

**Editor:** `SessionRunCommandsDialog` modal — reachable from right-click on ▶, the ▼ dropdown's "Edit commands…" entry, and the sidebar right-click "Session commands" submenu. Inline-edit rows with up/down reorder, default-radio column, +Add / 🗑 Delete, Cancel/Save.

**Keybindings:** `F5` runs the active session's default. `Shift+F5` stops it. Mirrors Visual Studio; deliberately not `Ctrl+R` (collides with shell history search).

**Lifecycle:** All runs are killed on session close, session sleep, and app exit. `SessionViewModel.Dispose()` calls `Runner.Dispose()` which iterates and disposes every instance.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document Per-Session Run Commands feature in CLAUDE.md"
```

---

## Task 13: Final integration testing

**Files:** none (manual testing pass)

- [ ] **Step 1: Run the full test suite**

```bash
dotnet test tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj
```
Expected: all green.

- [ ] **Step 2: End-to-end manual flows**

Launch `dotnet run --project src/CodeShellManager/CodeShellManager.csproj` and walk through:

**Flow A — Claude parent, .NET project:**
1. New session on a `.csproj` folder, command = `claude`.
2. Confirm chips don't appear yet, toolbar has `[▶][▼]`.
3. Press F5 → chip `● Run` appears, click it → drawer opens, output streams.
4. After exit (probably failed because no compile target yet), chip is `✗`.
5. Click `↗ Send to terminal` → text appears in Claude's input box, fenced with preamble. Submit it.

**Flow B — pwsh parent, custom command:**
1. New session, command = `pwsh`.
2. RunCommands is empty (no detection); ▶ hidden.
3. Right-click sidebar → "Session commands" → "Edit commands…" → add `ping localhost -n 3`.
4. Save, press F5, chip running, output streams.
5. ↗ Send to terminal → toast says clipboard (correct: not Claude).

**Flow C — SSH parent:**
1. New SSH session to a remote with passwordless key auth.
2. Add `ls -la` via the editor.
3. Press F5; verify the child ssh runs and output appears.

**Flow D — lifecycle:**
1. Start a long-running `ping -t` run.
2. 💤 Sleep the session — verify Task Manager has no orphan `ping.exe` / `cmd.exe`.
3. Wake the session — RunCommands still configured, chips empty.
4. Run again; ✕ close session — no orphans.
5. Run again; close entire app via tray "Quit" — no orphans.

**Flow E — persistence:**
1. Add 3 commands to a session; close + reopen app.
2. Verify the list survived (chevron dropdown still has 3 entries with correct labels + default).

- [ ] **Step 3: Final commit (only if any fixes were needed)**

If you found and fixed bugs during the manual flows, commit them with a descriptive message:

```bash
git add <changed files>
git commit -m "fix: <specific bug found during integration testing>"
```

---

## Self-Review

**1. Spec coverage** — every grilled decision maps to a task:

- Q1 (List of `RunCommandItem`) → Task 1
- Q2 (Templates seed sessions) → Tasks 2 + 6
- Q3 revised (separate child, not PTY-injection) → Task 4 (RunInstance.Start)
- Q4 (Headless `PseudoTerminal`) → Tasks 3 + 4
- Q5 (Per-item concurrency) → Task 5 (`SessionRunner.Run` keys by item Id, multi-instance)
- Q6 (Drawer + chips strip) → Task 7
- Q7 (SSH = second connection) → Task 4 (BuildSshArgs in RunInstance)
- Q8 (Kill on every exit path) → Task 11
- Q9 (Data shape) → Task 1
- Q10 (Templates: 5 detectors, first-match, top-level, SSH skipped) → Task 2
- Q11 (Modal dialog, 3 entry points) → Tasks 9 + 10 + Task 7's right-click on ▶
- Q12 (Send-to-terminal: Claude fence / non-Claude clipboard) → Task 7's `SendRunOutputToTerminal`
- Q13a (ANSI-stripped plain text) → Task 4 (`AnsiPattern().Replace`)
- Q13b (No kill confirmation) → Task 5/7 (Stop is a direct call)
- Q13c (F5 / Shift+F5) → Task 8
- Q13d (One chip per item, persists until dismissed or session ends) → Task 7 (Runner is dict by item.Id)
- Q13e (List persists, runtime doesn't) → Task 1 + Task 4 (RunInstance has no `[Serialize]` path)

**2. Placeholder scan** — none of "TBD", "implement later", "handle edge cases", "similar to" remain. Task 6 had a `RefreshTerminalRunControls` stub that Task 7 replaces in place; this is intentional staged work, not a placeholder.

**3. Type consistency** — checked:
- `RunCommandItem.Id` (string) used as dict key in `SessionRunner.Instances` and `_drawerItemBySession`. ✓
- `RunInstance.ItemId` mirrors `RunCommandItem.Id`. ✓
- `RunState` enum used consistently in Task 4 + 7. ✓
- `SessionRunner.Run/Stop/Dismiss/StopAll` referenced from Task 7 / 8 / 11 match Task 5's signatures. ✓
- `OpenRunCommandsEditor` stub in Task 7 is replaced (not duplicated) by Task 9 Step 3. ✓
- `RefreshTerminalRunControls` stub in Task 6 superseded by Task 7 Step 6. ✓

using System;
using System.Diagnostics;
using System.IO;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.UIA3;

namespace CodeShellManager.UITests;

/// <summary>
/// Launches and tears down the CodeShellManager process once per test class.
/// Sets CSM_STATE_PATH on the child process's environment directly (via ProcessStartInfo)
/// so the env var is scoped to that process only — no test-runner-level mutation.
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

        // Scope CSM_STATE_PATH to the child process only — avoids env var races when
        // xUnit constructs multiple AppFixture instances concurrently.
        var psi = new ProcessStartInfo(GetExePath());
        psi.EnvironmentVariables["CSM_STATE_PATH"] = _statePath;

        Automation = new UIA3Automation();
        App = Application.Launch(psi);
        MainWindow = App.GetMainWindow(Automation, TimeSpan.FromSeconds(15));
    }

    public void Dispose()
    {
        try { App.Close(); } catch { /* ignore if already closed */ }
        Automation.Dispose();
        try { File.Delete(_statePath); } catch { }
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

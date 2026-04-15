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

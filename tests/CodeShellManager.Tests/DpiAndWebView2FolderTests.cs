using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// See "DPI awareness" and "WebView2 data folder" in CLAUDE.md.
/// </summary>
public class DpiAndWebView2FolderTests
{
    [Fact]
    public void Normal_run_keeps_the_existing_folder()
    {
        // Renaming this would silently orphan every user's existing WebView2 profile.
        Assert.Equal(@"C:\AppData\CodeShellManager\WebView2",
            WebView2DataFolder.Resolve(@"C:\AppData", cleanStart: false));
    }

    [Fact]
    public void Clean_run_never_shares_the_normal_folder()
    {
        // Sharing is what broke a --clean dev build next to the installed app: one browser
        // process per folder, and the two copies could not both attach to it.
        string normal = WebView2DataFolder.Resolve(@"C:\AppData", cleanStart: false);
        string clean = WebView2DataFolder.Resolve(@"C:\AppData", cleanStart: true);
        Assert.NotEqual(normal, clean, StringComparer.OrdinalIgnoreCase);
        // Siblings, not nested, so neither copy's folder can contain the other's.
        Assert.False(clean.StartsWith(normal + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase));
    }

    [Fact]
    public void Incompatible_copy_is_recognised_from_the_hresult_actually_observed()
    {
        // The exact exception crash.log recorded for a per-monitor build on a folder whose
        // browser process belonged to the system-DPI installed app.
        var observed = new COMException(
            "The group or resource is not in the correct state to perform the requested operation. (0x8007139F)",
            unchecked((int)0x8007139F));

        Assert.True(WebView2StartupError.IsIncompatibleCopy(observed));
        string text = WebView2StartupError.Describe(observed);
        Assert.Contains("another copy", text);
        Assert.Contains("Close the other copy", text);
    }

    [Fact]
    public void Other_failures_are_not_misdiagnosed_as_another_copy()
    {
        Assert.False(WebView2StartupError.IsIncompatibleCopy(new COMException("x", unchecked((int)0x80070005))));
        Assert.False(WebView2StartupError.IsIncompatibleCopy(new UnauthorizedAccessException()));
        Assert.False(WebView2StartupError.IsIncompatibleCopy(new InvalidOperationException()));

        string text = WebView2StartupError.Describe(new InvalidOperationException("boom"));
        Assert.Contains("boom", text);
        Assert.DoesNotContain("another copy", text);
    }

    [Fact]
    public void Built_executable_declares_per_monitor_v2()
    {
        // Pins the manifest into the binary that actually ships, not just the source file:
        // dropping <ApplicationManifest> from the csproj would leave app.manifest on disk and
        // quietly revert the app to system-DPI scaling.
        string dir = Path.GetDirectoryName(typeof(App).Assembly.Location)!;
        string exe = Path.Combine(dir, "CodeShellManager.exe");
        Assert.True(File.Exists(exe), $"apphost not found next to the app assembly: {exe}");

        string bytes = Encoding.ASCII.GetString(File.ReadAllBytes(exe));
        Assert.Contains("PerMonitorV2", bytes);
    }
}

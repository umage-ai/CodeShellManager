using System;
using System.IO;
using System.Runtime.InteropServices;

namespace CodeShellManager.Terminal;

/// <summary>
/// Where the terminals' WebView2 keeps its user data. See "WebView2 data folder" in CLAUDE.md.
/// </summary>
/// <remarks>
/// Every <see cref="TerminalBridge"/> in a process uses one folder, and WebView2 runs one
/// browser process per folder — shared by every app process that points at it. Two copies of
/// CodeShellManager that create WebView2 differently (DPI awareness above all) cannot share
/// that browser process; the second copy's start fails with <c>0x8007139F</c>. A
/// <c>--clean</c> run is by definition a second copy next to the user's real one, and usually
/// a different build, so it gets its own folder.
/// </remarks>
public static class WebView2DataFolder
{
    public const string Normal = "WebView2";
    public const string Clean = "WebView2-clean";

    public static string Resolve(string appDataRoot, bool cleanStart) =>
        Path.Combine(appDataRoot, "CodeShellManager", cleanStart ? Clean : Normal);
}

/// <summary>Turns a WebView2 start failure into something a user can act on.</summary>
public static class WebView2StartupError
{
    /// <summary>
    /// HRESULT_FROM_WIN32(ERROR_INVALID_STATE): "The group or resource is not in the correct
    /// state…". Observed when a per-monitor-DPI build started WebView2 on a data folder whose
    /// browser process belonged to a system-DPI copy of the app.
    /// </summary>
    public const int IncompatibleBrowserProcess = unchecked((int)0x8007139F);

    public static bool IsIncompatibleCopy(Exception ex) =>
        ex is COMException { HResult: IncompatibleBrowserProcess };

    public static string Describe(Exception ex) => IsIncompatibleCopy(ex)
        ? "The terminal renderer (WebView2) could not start, because another copy of " +
          "CodeShellManager — a different version, a portable copy or a dev build — is " +
          "already using its WebView2 data folder with different settings. Close the other " +
          "copy and try again."
        : $"The terminal renderer (WebView2) could not start: {ex.Message}";
}

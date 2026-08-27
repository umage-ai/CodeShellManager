using System;
using System.Diagnostics;

namespace CodeShellManager.Services;

/// <summary>
/// Decides once per process whether to spell PowerShell as <c>pwsh.exe</c> (7+) or
/// <c>powershell.exe</c> (Windows-bundled 5.1).
///
/// Two callers need this and used to answer it separately:
///   - <see cref="RunInstance"/>, wrapping a RunMode.PowerShell run command.
///   - <see cref="Terminal.PseudoTerminal"/>, wrapping a non-shell session command so
///     the shell sets up the Win32 console before the target process launches.
///
/// Prefer pwsh because that is where modern users keep their profile functions —
/// wrapping in 5.1 loads a different profile and won't see them.
///
/// We only pick a *name*; CreateProcess resolves PATH. So a PATH lookup is the whole
/// question, and <c>where.exe</c> answers it in ~10ms. The earlier RunInstance version
/// spawned <c>pwsh -Command "exit 0"</c> instead, which additionally proved pwsh could
/// actually run — but at the cost of a full PowerShell startup (hundreds of ms) on a
/// path that runs during session restore. Not worth it for the rare broken install:
/// that case now surfaces as a failed session rather than a slower launch for everyone.
/// </summary>
internal static class PwshLocator
{
    private static readonly Lazy<string> s_executable = new(Resolve);

    /// <summary>
    /// <c>"pwsh.exe"</c> when PowerShell 7+ is on PATH, otherwise <c>"powershell.exe"</c>.
    /// Resolved on first use and cached for the process lifetime.
    /// </summary>
    internal static string Executable => s_executable.Value;

    private static string Resolve()
    {
        Process? probe = null;
        try
        {
            probe = Process.Start(new ProcessStartInfo
            {
                FileName = "where.exe",
                Arguments = "pwsh.exe",
                UseShellExecute = false,
                CreateNoWindow = true,
                // Read the resolved path back so it can be sanity-checked below.
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            });

            // A hung lookup is treated as "not available" rather than blocking startup.
            if (probe != null && probe.WaitForExit(2000) && probe.ExitCode == 0)
            {
                // where.exe proves a NAME resolves, not that it runs (issue #104). The
                // common false positive is a Microsoft Store App Execution Alias in
                // %LOCALAPPDATA%\Microsoft\WindowsApps — a zero-byte reparse point that
                // resolves on PATH and fails on execution when the app isn't installed.
                //
                // This matters more than it used to: since the two locators merged, this
                // also picks the wrapper for every non-shell SESSION command. Getting it
                // wrong means every claude session fails to launch, where the old code
                // would simply have used powershell.exe.
                string first = (probe.StandardOutput.ReadToEnd() ?? "")
                    .Split('\n')[0].Trim();

                if (IsRunnable(first)) return "pwsh.exe";
            }
        }
        catch { /* where.exe missing or blocked by policy — fall through */ }
        finally
        {
            try { if (probe is { HasExited: false }) probe.Kill(entireProcessTree: true); }
            catch { /* best effort */ }
            probe?.Dispose();
        }

        return "powershell.exe";
    }

    /// <summary>
    /// True when <paramref name="path"/> looks like a real executable rather than a Store
    /// App Execution Alias stub.
    ///
    /// The stubs live under WindowsApps, are zero bytes on disk, and are reparse points.
    /// Length is the cheap discriminator and needs no extra API; the reparse-point check
    /// is the belt-and-braces one. An unreadable path is treated as not runnable, because
    /// falling back to powershell.exe is always safe and picking a broken pwsh is not.
    /// </summary>
    internal static bool IsRunnable(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        try
        {
            var info = new System.IO.FileInfo(path);
            if (!info.Exists || info.Length == 0) return false;
            return (info.Attributes & System.IO.FileAttributes.ReparsePoint) == 0;
        }
        catch { return false; }
    }
}

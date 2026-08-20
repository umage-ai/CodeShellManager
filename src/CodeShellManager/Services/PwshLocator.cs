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
                // Redirected so a match doesn't print into whatever console we inherit.
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            });

            // A hung lookup is treated as "not available" rather than blocking startup.
            if (probe != null && probe.WaitForExit(2000) && probe.ExitCode == 0)
                return "pwsh.exe";
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
}

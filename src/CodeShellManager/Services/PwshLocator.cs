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
/// We only pick a *name*; CreateProcess resolves PATH. So a PATH lookup is most of the
/// question, and <c>where.exe</c> answers it in ~10ms. That alone is enough for an
/// ordinary executable.
///
/// It is NOT enough for a Microsoft Store App Execution Alias, which is a zero-byte
/// reparse point whether the app behind it is installed or not — so a working Store
/// PowerShell 7 and a stub left by an uninstalled one are identical on disk. Only that
/// ambiguous case pays an execution probe; see <see cref="IsRunnable"/>.
///
/// <see cref="Executable"/> is warmed off the UI thread in MainWindow.OnLoaded, because
/// the Lazy is otherwise first forced from PseudoTerminal.Start on the UI thread and the
/// probe would freeze the window.
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
    /// True when <paramref name="path"/> can actually be executed.
    ///
    /// Ordinary executables are decided from metadata alone — non-zero length and not a
    /// reparse point — so the common install costs nothing. A zero-byte reparse point is
    /// a Store App Execution Alias, which is *ambiguous* rather than bad, and only that
    /// case is settled by probing. An unreadable path is treated as not runnable, because
    /// falling back to powershell.exe is always safe and picking a broken pwsh is not.
    /// </summary>
    internal static bool IsRunnable(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        try
        {
            var info = new System.IO.FileInfo(path);
            if (!info.Exists) return false;

            // A real executable — decided, no probe needed.
            if (info.Length > 0 &&
                (info.Attributes & System.IO.FileAttributes.ReparsePoint) == 0)
                return true;

            // Zero-byte and/or a reparse point: a Microsoft Store App Execution Alias.
            //
            // The earlier version rejected these outright to skip stubs left behind for
            // uninstalled apps. That was wrong: a WORKING Store install of PowerShell 7 is
            // exactly the same shape — a zero-byte AppExecLink at
            // %LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe. The two are indistinguishable
            // on disk, so rejecting the shape silently downgraded Store-PowerShell users to
            // 5.1 — losing the PS7 profile functions that are the whole reason for
            // preferring pwsh, on every session launch since the locators merged.
            //
            // Neither answer is safe from metadata alone, so ask the alias to run. Only
            // reached for the alias shape, so the common MSI install still costs nothing.
            return CanExecute(path);
        }
        catch { return false; }
    }

    /// <summary>
    /// Runs <paramref name="path"/> with a trivial no-op and reports whether it exited
    /// cleanly. Used only to disambiguate a Store App Execution Alias, where the on-disk
    /// metadata cannot tell a live alias from a dead stub.
    /// </summary>
    private static bool CanExecute(string path)
    {
        Process? probe = null;
        try
        {
            probe = Process.Start(new ProcessStartInfo
            {
                FileName = path,
                Arguments = "-NoLogo -NoProfile -Command \"exit 0\"",
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            });
            // A dead alias fails fast (Win32Exception). A live one still pays a PowerShell
            // startup, hence the generous ceiling — but a hang must not become a hang here.
            return probe != null && probe.WaitForExit(5000) && probe.ExitCode == 0;
        }
        catch { return false; }
        finally
        {
            try { if (probe is { HasExited: false }) probe.Kill(entireProcessTree: true); }
            catch { /* best effort */ }
            probe?.Dispose();
        }
    }
}

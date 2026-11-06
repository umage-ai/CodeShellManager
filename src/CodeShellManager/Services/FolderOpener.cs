using System;
using System.Diagnostics;
using System.IO;
using System.Threading.Tasks;

namespace CodeShellManager.Services;

/// <summary>
/// Opens a folder in the user's file manager, reliably. See "Opening a folder in Explorer"
/// in CLAUDE.md.
/// </summary>
/// <remarks>
/// It used to be <c>Process.Start("explorer.exe", folder)</c> — the folder as raw command-line
/// text — and Explorer silently opened <b>Documents</b> instead of the folder for two kinds of
/// path (measured, by asking the Shell which folder each new Explorer window showed):
/// <list type="bullet">
/// <item>forward slashes (<c>C:/repos/wt-feat</c>) — the form git prints, and so the form a
/// session opened through "New session in sibling worktree" stored;</item>
/// <item>a comma anywhere in the name — Explorer splits its arguments on commas, and quoting
/// the path does not stop it.</item>
/// </list>
/// Opening the folder itself through the shell (ShellExecute "open" on the full Windows path)
/// opened the right folder for every case tried: plain, spaces, comma, <c>&amp;</c>, <c>%</c>,
/// forward slashes, trailing separator, 8.3 short name. It also honours a replacement file
/// manager the user has set as the folder handler.
/// </remarks>
public static class FolderOpener
{
    /// <summary>
    /// The path to hand to the shell, or null when it must not be opened: not rooted, or not
    /// an existing DIRECTORY. The second is a security check, not tidiness — ShellExecute on a
    /// file runs it, and a session's folder comes from state.json and imports, which are
    /// untrusted. Touches the disk (and \\wsl$), so call it off the UI thread.
    /// </summary>
    public static string? Target(string? folder)
    {
        if (string.IsNullOrWhiteSpace(folder)) return null;
        try
        {
            if (!Path.IsPathFullyQualified(folder)) return null;
            string full = WindowsPath(folder);
            return Directory.Exists(full) ? full : null;
        }
        catch { return null; }
    }

    /// <summary>
    /// Backslashes, absolute, no trailing separator (except a drive root). The form every
    /// Windows tool accepts — use it for any folder that came from git.
    /// </summary>
    public static string WindowsPath(string folder)
    {
        string full = Path.GetFullPath(folder);
        string trimmed = Path.TrimEndingDirectorySeparator(full);
        return trimmed.EndsWith(':') ? trimmed + Path.DirectorySeparatorChar : trimmed;
    }

    /// <summary>
    /// Opens <paramref name="folder"/> off the calling thread. Returns false when it was not
    /// an existing folder or the shell refused; never throws.
    /// </summary>
    public static Task<bool> OpenAsync(string? folder) => Task.Run(() =>
    {
        string? target = Target(folder);
        if (target == null) return false;
        try
        {
            Process.Start(new ProcessStartInfo(target) { UseShellExecute = true, Verb = "open" })?.Dispose();
            return true;
        }
        catch { return false; }
    });
}

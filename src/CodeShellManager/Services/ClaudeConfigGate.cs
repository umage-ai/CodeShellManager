using System;
using System.IO;
using System.Threading.Tasks;

namespace CodeShellManager.Services;

/// <summary>
/// Spaces out consecutive Claude launches by watching Claude's config file settle,
/// instead of sleeping a fixed <see cref="Models.AppSettings.ClaudeLaunchStaggerMs"/>
/// between each one (issue #82).
///
/// Why the stagger exists: the Claude CLI rewrites its config on startup, and two
/// claude.exe processes doing that at once can lose one another's updates. Evidence
/// that this is real, not theoretical — a machine here has two orphaned
/// <c>.claude.json.tmp.&lt;pid&gt;.&lt;hash&gt;</c> files with the same timestamp and
/// different pids, left behind by two writers racing.
///
/// Why a fixed delay is the wrong shape: it pays the worst case every time. Restoring
/// 20 Claude sessions spent 19 × 2s = 38 seconds purely asleep. Watching the file
/// instead costs whatever it actually takes — usually a few hundred milliseconds —
/// and the cap keeps the worst case exactly where it was.
/// </summary>
internal static class ClaudeConfigGate
{
    /// <summary>How long the file must stay unchanged before we call it settled.</summary>
    internal static readonly TimeSpan DefaultQuietFor = TimeSpan.FromMilliseconds(250);

    /// <summary>
    /// Resolves the config file Claude actually writes.
    ///
    /// Note the layout differs between the two cases, so this is not just
    /// <see cref="ClaudeSessionService.ResolveClaudeHome"/> plus a filename:
    ///   default            -> %USERPROFILE%\.claude.json   (a *sibling* of ~/.claude)
    ///   CLAUDE_CONFIG_DIR  -> %CLAUDE_CONFIG_DIR%\.claude.json  (*inside* it)
    ///
    /// Getting this wrong means watching a file nobody writes and always waiting the
    /// full cap — which is how it behaves today on any machine with the env var set.
    /// </summary>
    internal static string ResolveConfigFile(string? configDir, string userProfile) =>
        string.IsNullOrWhiteSpace(configDir)
            ? Path.Combine(userProfile, ".claude.json")
            : Path.Combine(configDir, ".claude.json");

    /// <summary>Live resolution against the current environment.</summary>
    internal static string ResolveConfigFile() =>
        ResolveConfigFile(
            Environment.GetEnvironmentVariable("CLAUDE_CONFIG_DIR"),
            Environment.GetFolderPath(Environment.SpecialFolder.UserProfile));

    /// <summary>Last-write time of <paramref name="path"/>, or null if it isn't there.</summary>
    internal static DateTime? LastWriteUtcOrNull(string path)
    {
        try { return File.Exists(path) ? File.GetLastWriteTimeUtc(path) : null; }
        catch { return null; }
    }

    /// <summary>
    /// Waits until the config file has changed from <paramref name="baseline"/> and then
    /// stayed unchanged for <paramref name="quietFor"/>, or until <paramref name="cap"/>
    /// elapses — whichever comes first.
    ///
    /// If no write is ever observed we wait the full cap, deliberately: that is exactly
    /// today's behaviour, so a machine where the file can't be watched is never worse
    /// off than before. Take the baseline *before* launching, or a fast write lands
    /// before the first poll and looks like no write at all.
    ///
    /// Clock and I/O are injected so the state machine is unit-testable without real
    /// files or real time.
    /// </summary>
    internal static async Task WaitForQuiesceAsync(
        DateTime? baseline,
        Func<DateTime?> lastWriteUtc,
        Func<DateTime> utcNow,
        Func<int, Task> delayAsync,
        TimeSpan cap,
        TimeSpan quietFor,
        int pollMs = 50)
    {
        if (cap <= TimeSpan.Zero) return;

        DateTime start = utcNow();
        DateTime? seen = baseline;
        DateTime? changedAt = null;

        while (utcNow() - start < cap)
        {
            await delayAsync(pollMs);

            DateTime? current = lastWriteUtc();
            if (current != seen)
            {
                seen = current;
                changedAt = utcNow();
                continue;
            }

            // Unchanged since the last poll. Only counts as settled once we've actually
            // seen a write — otherwise a file Claude hasn't touched yet would let the
            // next launch start immediately, which is the race we're preventing.
            if (changedAt is { } t && utcNow() - t >= quietFor) return;
        }
    }
}

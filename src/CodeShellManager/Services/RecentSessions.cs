using System;
using System.Collections.Generic;
using System.Linq;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// The "🕘 Recent" toolbar menu (Ctrl+Shift+E): the sessions used most recently, newest first.
/// WPF-free so ordering and wording are unit-tested (<c>RecentSessionsTests</c>).
///
/// "Used" is <see cref="ShellSession.LastActivityAt"/>, stamped whenever a session becomes the
/// active one (sidebar click, typing or clicking in its pane, Ctrl+Tab, …) and persisted, so
/// the list survives a restart. The restore loop does not stamp — see MainWindow's
/// ActiveSession handler.
/// </summary>
public static class RecentSessions
{
    /// <summary>How many sessions the menu lists.</summary>
    public const int MaxItems = 10;

    /// <summary>
    /// Live and sleeping sessions by most recent use, without the active one (it is already
    /// on screen — leaving it out makes the first item "the session I was in before", so
    /// Ctrl+Shift+E, Enter flips between the two).
    /// </summary>
    public static IReadOnlyList<ShellSession> Pick(
        IEnumerable<ShellSession> sessions, string? activeId, int max = MaxItems)
        => sessions
            .Where(s => s.Id != activeId)
            .Select((s, i) => (s, i))
            // Stable on ties: keep sidebar order for sessions never used (same backfilled stamp).
            .OrderByDescending(t => t.s.LastActivityAt)
            .ThenBy(t => t.i)
            .Take(Math.Max(0, max))
            .Select(t => t.s)
            .ToList();

    /// <summary>Compact "how long ago": now, 5m, 3h, 2d, 6w; blank for "never".</summary>
    public static string Ago(DateTime thenUtc, DateTime nowUtc)
    {
        if (thenUtc == default) return "";
        var d = nowUtc - thenUtc;
        if (d < TimeSpan.FromMinutes(1)) return "now";
        if (d < TimeSpan.FromHours(1)) return $"{(int)d.TotalMinutes}m";
        if (d < TimeSpan.FromDays(1)) return $"{(int)d.TotalHours}h";
        if (d < TimeSpan.FromDays(14)) return $"{(int)d.TotalDays}d";
        return $"{(int)(d.TotalDays / 7)}w";
    }
}

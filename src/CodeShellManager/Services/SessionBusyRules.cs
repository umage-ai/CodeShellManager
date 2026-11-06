using System;
using System.Collections.Generic;

namespace CodeShellManager.Services;

/// <summary>
/// Decides whether a live session is "busy" — doing something that closing the app would
/// cut off — and says why, for the "still running" warning on app close.
///
/// WPF-free on purpose, like <see cref="BulkRestartEstimate"/>: the rule is the only part
/// of that warning worth testing.
///
/// <list type="bullet">
/// <item>A session that speaks OSC 7501 is judged by its report alone: <c>working</c>
/// (with progress) and <c>blocked</c> (mid-turn, waiting for an answer) are busy;
/// idle/done/error are not. Its output is ignored — an idle TUI may still repaint.</item>
/// <item>Any other session is busy while it has produced output within
/// <see cref="RecentOutputWindow"/>: a Claude spinner, a build, a dev server. An idle
/// shell prompt or a heuristic "waiting for input" prompt prints nothing.</item>
/// <item>A running run command is busy in either case.</item>
/// </list>
/// </summary>
public static class SessionBusyRules
{
    /// <summary>How recent the last PTY output must be for a non-7501 session to count as busy.</summary>
    public static readonly TimeSpan RecentOutputWindow = TimeSpan.FromSeconds(4);

    /// <summary>
    /// Why the session is busy, or null when it is idle. Pure function of its inputs.
    /// </summary>
    public static string? Reason(
        bool hasProgramStatus,
        ProgramState? agentState,
        int? progress,
        int runningRunCommands,
        DateTime? lastOutputUtc,
        DateTime nowUtc)
    {
        var parts = new List<string>(2);

        if (hasProgramStatus)
        {
            if (agentState == ProgramState.Working)
                parts.Add(progress is int p ? $"working · {p}%" : "working");
            else if (agentState == ProgramState.Blocked)
                parts.Add("waiting for you mid-task");
        }
        else if (lastOutputUtc is DateTime last)
        {
            // A negative age is output that landed after `now` was read (the PTY reader
            // runs on its own thread) — that is as recent as it gets, not clock noise.
            if (nowUtc - last <= RecentOutputWindow)
                parts.Add("producing output");
        }

        if (runningRunCommands == 1) parts.Add("1 run command running");
        else if (runningRunCommands > 1) parts.Add($"{runningRunCommands} run commands running");

        return parts.Count == 0 ? null : string.Join(" · ", parts);
    }
}

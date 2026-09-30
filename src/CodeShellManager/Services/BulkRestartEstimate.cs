using System;
using System.Globalization;
using System.Text;

namespace CodeShellManager.Services;

/// <summary>
/// The wording of the "Restart all…" confirmation — how long the queue will take, and
/// whether the user is told that it is a long one.
///
/// WPF-free on purpose, like <see cref="ShellIntegrationPayload"/> and
/// <see cref="SessionConfigEditor"/>: this is the only part of the bulk-restart UI with
/// logic worth testing, and it can only be tested if it does not touch a Window.
/// </summary>
public static class BulkRestartEstimate
{
    /// <summary>
    /// Target count at which the confirmation stops merely quoting a duration and starts
    /// explaining that the restarts are sequential. Below this the operation is short
    /// enough that the estimate alone carries the point.
    /// </summary>
    public const int ScaleWarningThreshold = 10;

    // Per-session cost, in seconds, as a low–high range.
    //
    // A Claude session dominates: RestartSessionCoreAsync waits for the outgoing process
    // to actually exit (CLAUDE.md measures busy shutdowns at 2.3–4.7s), then a flat
    // stagger of up to 1s, then the relaunch itself (~1.4s in the RESTORE log lines).
    // A plain shell skips the exit wait entirely and is just the relaunch.
    //
    // A range rather than a point because the spread is real — quoting "4 minutes" to
    // three significant figures would be false precision dressed up as help.
    private const int ClaudeLowSeconds = 4;
    private const int ClaudeHighSeconds = 8;
    private const int OtherLowSeconds = 1;
    private const int OtherHighSeconds = 2;

    // Switch to minutes only once the OPTIMISTIC bound passes a minute. Deciding on the
    // pessimistic bound instead would report "roughly ½–2 minutes" for a queue that
    // usually finishes in forty seconds.
    private const int MinutesThresholdSeconds = 60;

    /// <summary>
    /// A human range for how long restarting <paramref name="totalTargets"/> sessions
    /// will take, of which <paramref name="claudeTargets"/> are Claude sessions.
    /// </summary>
    public static string DescribeDuration(int totalTargets, int claudeTargets)
    {
        // Clamped at both ends. `others` was already floored at 0, but an uncapped claude
        // count invented duration out of nothing — (3 targets, 10 Claude) quoted 40–80
        // seconds for three sessions. Not reachable from MainWindow, where claudeCount is
        // a subset count of the same list, but this is public and cheap to make honest.
        int total = Math.Max(0, totalTargets);
        int claude = Math.Min(total, Math.Max(0, claudeTargets));
        int others = total - claude;

        int lowSeconds = ClaudeLowSeconds * claude + OtherLowSeconds * others;
        int highSeconds = ClaudeHighSeconds * claude + OtherHighSeconds * others;

        if (lowSeconds < MinutesThresholdSeconds)
            return $"about {lowSeconds}–{highSeconds} seconds";

        return $"roughly {FormatMinutes(lowSeconds / 60.0)}–{FormatMinutes(highSeconds / 60.0)} minutes";
    }

    /// <summary>
    /// The body of the confirmation dialog. <paramref name="headline"/> is the caller's
    /// scope-naming first line ("Restart all 55 live sessions?").
    /// </summary>
    public static string BuildConfirmText(string headline, int totalTargets, int claudeTargets)
    {
        int claude = Math.Max(0, claudeTargets);
        string duration = DescribeDuration(totalTargets, claude);

        var sb = new StringBuilder();
        sb.Append(headline);
        sb.Append(Environment.NewLine).Append(Environment.NewLine);

        if (totalTargets >= ScaleWarningThreshold)
        {
            // Naming the Claude count is what makes the estimate look earned rather than
            // arbitrary — it is the reason the queue is slow. Omitted when there are none,
            // since blaming absent sessions would just be wrong.
            sb.Append(claude > 0
                ? $"{claude} of them are Claude sessions and restart one at a time — expect {duration}."
                : $"They restart one at a time — expect {duration}.");
        }
        else
        {
            sb.Append($"Expect {duration}.");
        }

        sb.Append(Environment.NewLine).Append(Environment.NewLine);
        sb.Append("Each running process is terminated and relaunched in turn, and any running "
                + "session commands are stopped.");
        if (claude > 0)
            sb.Append(" Claude sessions resume their conversation.");

        // Only when there is actually a queue to stop. The bulk entry points confirm
        // whatever the count, but SetRestartProgress hides the rail and pill at
        // totalTargets <= 1 — so at exactly one session this sentence was pointing the
        // user at a control that never appears.
        if (totalTargets > 1)
            sb.Append(" You can stop the queue from the toolbar once it starts.");

        return sb.ToString();
    }

    /// <summary>Minutes to the nearest half, with the half written as a glyph.</summary>
    private static string FormatMinutes(double minutes)
    {
        double rounded = Math.Round(minutes * 2, MidpointRounding.AwayFromZero) / 2;
        int whole = (int)rounded;
        bool hasHalf = Math.Abs(rounded - whole) > 0.01;

        if (whole == 0) return "½";
        return hasHalf
            ? whole.ToString(CultureInfo.InvariantCulture) + "½"
            : whole.ToString(CultureInfo.InvariantCulture);
    }
}

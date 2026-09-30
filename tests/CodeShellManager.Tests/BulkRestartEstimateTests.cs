using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// The confirmation a bulk restart shows before it runs. A restart queue is strictly
/// sequential and a Claude session waits for its old process to exit, so "restart all" on
/// a large fleet is a minute-scale operation — the whole point of this text is that the
/// user learns that BEFORE clicking Yes rather than four minutes in.
/// </summary>
public class BulkRestartEstimateTests
{
    // ── Duration ──────────────────────────────────────────────────────────────

    [Fact]
    public void NoClaudeSessions_StaysInSeconds()
    {
        // Plain shells skip the exit wait entirely; six of them is a few seconds, and
        // quoting minutes for that would train the user to ignore the estimate.
        string text = BulkRestartEstimate.DescribeDuration(totalTargets: 6, claudeTargets: 0);

        Assert.Contains("seconds", text);
        Assert.DoesNotContain("minute", text);
    }

    [Fact]
    public void SingleClaudeSession_StaysInSeconds()
    {
        string text = BulkRestartEstimate.DescribeDuration(totalTargets: 1, claudeTargets: 1);

        Assert.Contains("seconds", text);
        Assert.DoesNotContain("minute", text);
    }

    [Fact]
    public void LargeClaudeFleet_IsReportedAsAMinuteRange()
    {
        // 52 Claude at 4-8s each plus 3 plain shells at 1-2s: 211s to 422s.
        string text = BulkRestartEstimate.DescribeDuration(totalTargets: 55, claudeTargets: 52);

        Assert.Contains("3½", text);
        Assert.Contains("7", text);
        Assert.Contains("minutes", text);
    }

    [Fact]
    public void WholeMinuteRange_OmitsTheHalfGlyph()
    {
        // 30 Claude: 120s to 240s, i.e. exactly 2-4 minutes. A bare "2" reads better
        // than "2½" rounded down to it, so the half glyph must be conditional.
        string text = BulkRestartEstimate.DescribeDuration(totalTargets: 30, claudeTargets: 30);

        Assert.DoesNotContain("½", text);
        Assert.Contains("minutes", text);
    }

    [Fact]
    public void DurationGrowsWithTheNumberOfClaudeSessions()
    {
        // Guards the direction of the model rather than its exact constants: swapping the
        // Claude and non-Claude costs would still satisfy every assertion above. Same
        // fleet size both times, so only the Claude share can move the answer.
        string small = BulkRestartEstimate.DescribeDuration(totalTargets: 40, claudeTargets: 0);
        string large = BulkRestartEstimate.DescribeDuration(totalTargets: 40, claudeTargets: 40);

        Assert.NotEqual(small, large);
        Assert.DoesNotContain("minute", small);
        Assert.Contains("minutes", large);
    }

    // ── Confirmation text ─────────────────────────────────────────────────────

    [Fact]
    public void ConfirmText_OpensWithTheHeadlineOnItsOwnLine()
    {
        string text = BulkRestartEstimate.BuildConfirmText(
            "Restart all 55 live sessions?", totalTargets: 55, claudeTargets: 52);

        string[] lines = text.Split('\n');
        Assert.Equal("Restart all 55 live sessions?", lines[0].TrimEnd('\r'));
    }

    [Fact]
    public void ConfirmText_SaysTheQueueCanBeStopped()
    {
        // The stop affordance is the answer to "I clicked Yes on 55 by mistake". If the
        // dialog doesn't mention it, the user has no reason to look for the toolbar pill.
        string text = BulkRestartEstimate.BuildConfirmText(
            "Restart all 55 live sessions?", totalTargets: 55, claudeTargets: 52);

        Assert.Contains("stop", text, System.StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void ConfirmText_SaysRunCommandsAreStopped()
    {
        string text = BulkRestartEstimate.BuildConfirmText(
            "Restart 3 sessions?", totalTargets: 3, claudeTargets: 1);

        Assert.Contains("session commands", text);
    }

    [Fact]
    public void ConfirmText_BelowTheThreshold_HasNoScaleWarning()
    {
        string text = BulkRestartEstimate.BuildConfirmText(
            "Restart 3 sessions?", totalTargets: 3, claudeTargets: 3);

        Assert.DoesNotContain("one at a time", text);
    }

    [Fact]
    public void ConfirmText_AtTheThreshold_WarnsAboutScale()
    {
        string text = BulkRestartEstimate.BuildConfirmText(
            "Restart all 10 live sessions?",
            totalTargets: BulkRestartEstimate.ScaleWarningThreshold,
            claudeTargets: BulkRestartEstimate.ScaleWarningThreshold);

        Assert.Contains("one at a time", text);
    }

    [Fact]
    public void ConfirmText_ScaleWarning_NamesHowManyAreClaude()
    {
        // "52 of them are Claude sessions" is the line that explains WHY it is slow.
        // Without it the estimate looks arbitrary.
        string text = BulkRestartEstimate.BuildConfirmText(
            "Restart all 55 live sessions?", totalTargets: 55, claudeTargets: 52);

        Assert.Contains("52", text);
    }

    [Fact]
    public void ConfirmText_ScaleWarning_WithNoClaudeSessions_DoesNotBlameClaude()
    {
        // 20 plain shells are still 20 relaunches, so the scale warning belongs — but
        // naming Claude sessions that aren't there would be a lie.
        string text = BulkRestartEstimate.BuildConfirmText(
            "Restart all 20 live sessions?", totalTargets: 20, claudeTargets: 0);

        Assert.Contains("one at a time", text);
        Assert.DoesNotContain("Claude session", text);
    }
}

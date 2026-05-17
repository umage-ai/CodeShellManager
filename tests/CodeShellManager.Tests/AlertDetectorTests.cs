using System;
using System.Threading;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for <see cref="AlertDetector"/>.
///
/// AlertDetector's only public surface for triggering an alert is <see cref="AlertDetector.Feed"/>,
/// which starts a hardcoded 1500ms <see cref="System.Threading.Timer"/>. There is no clock-injection
/// hook, and the regexes are private statics — so to assert on the regex behavior we have to let
/// the real timer fire.
///
/// To stay deterministic we subscribe to <see cref="AlertDetector.AlertRaised"/> with a
/// <see cref="ManualResetEventSlim"/> and wait with a generous timeout. The wait completes
/// as soon as the event fires (typically ~1.5s after Feed) — it is NOT a blind Thread.Sleep.
/// A short negative-case timeout (200ms past the idle threshold) is used when asserting that
/// no alert was raised.
///
/// IMPORTANT: every test disposes the detector BEFORE the signal goes out of scope. Otherwise
/// the in-flight Timer callback can race with the ManualResetEventSlim finalizer and throw
/// ObjectDisposedException on a thread-pool thread, which crashes the test host.
/// </summary>
public class AlertDetectorTests
{
    // Generous upper bound; the event normally fires ~1500ms after Feed. The ManualResetEventSlim
    // returns the instant the event fires, so a higher cap doesn't slow passing tests.
    private const int WaitForAlertMs = 5000;

    // Used to assert that *no* alert was raised. Anything > 1500ms is enough.
    private const int WaitForNoAlertMs = 1800;

    // ---- Tool approval detection -------------------------------------------------------------

    [Theory]
    [InlineData("Do you want to run this command?")]
    [InlineData("Allow Claude to edit this file?")]
    [InlineData("Approve this tool use?")]
    [InlineData("Bash command: rm -rf /tmp/foo")]
    [InlineData("Continue?")]
    [InlineData("Proceed?")]
    [InlineData("tool_use requested")]
    [InlineData("Grant permission to read")]
    public void Feed_ClaudeApprovalPhrasing_RaisesToolApprovalAlert(string line)
    {
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        AlertEvent? captured = null;
        detector.AlertRaised += evt => { captured = evt; signal.Set(); };

        try
        {
            detector.Feed(line + "\n");

            Assert.True(signal.Wait(WaitForAlertMs), $"AlertRaised did not fire for: {line}");
            Assert.NotNull(captured);
            Assert.Equal(AlertType.ToolApproval, captured!.Type);
            Assert.Equal("s1", captured.SessionId);
            Assert.Equal("session", captured.SessionName);
        }
        finally
        {
            detector.Dispose();
        }
    }

    // ---- Input-required detection ------------------------------------------------------------

    [Theory]
    [InlineData("Delete file [y/N]")]
    [InlineData("Continue [Y/n]")]
    [InlineData("Proceed [yes/no]")]
    [InlineData("Are you sure (y/n)")]
    [InlineData("Confirm (yes/no)")]
    [InlineData("❯")]                 // Claude's "❯" prompt alone (U+276F)
    [InlineData("something ❯")]       // ❯ at end of line
    [InlineData("Pick an option ? ›")] // "? ›" pattern (U+203A)
    [InlineData("What now?")]               // trailing "?"
    public void Feed_InputRequiredPhrasing_RaisesInputRequiredAlert(string line)
    {
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        AlertEvent? captured = null;
        detector.AlertRaised += evt => { captured = evt; signal.Set(); };

        try
        {
            detector.Feed(line + "\n");

            Assert.True(signal.Wait(WaitForAlertMs), $"AlertRaised did not fire for: {line}");
            Assert.NotNull(captured);
            Assert.Equal(AlertType.InputRequired, captured!.Type);
        }
        finally
        {
            detector.Dispose();
        }
    }

    [Fact]
    public void Feed_TrailingGreaterThanPrompt_RaisesInputRequiredAlert()
    {
        // The s_prompt regex matches ">" at end of line.
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        AlertEvent? captured = null;
        detector.AlertRaised += evt => { captured = evt; signal.Set(); };

        try
        {
            detector.Feed("user@host:~$ >\n");

            Assert.True(signal.Wait(WaitForAlertMs));
            Assert.NotNull(captured);
            Assert.Equal(AlertType.InputRequired, captured!.Type);
        }
        finally
        {
            detector.Dispose();
        }
    }

    // ---- Negative cases ----------------------------------------------------------------------

    [Theory]
    [InlineData("just some regular log output")]
    [InlineData("Compilation succeeded.")]
    [InlineData("12 files indexed")]
    public void Feed_NonMatchingLine_DoesNotRaiseAlert(string line)
    {
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        detector.AlertRaised += _ => signal.Set();

        try
        {
            detector.Feed(line + "\n");

            Assert.False(signal.Wait(WaitForNoAlertMs),
                $"AlertRaised should NOT have fired for: {line}");
        }
        finally
        {
            detector.Dispose();
        }
    }

    [Fact]
    public void Feed_WhitespaceOnly_DoesNotRaiseAlert()
    {
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        detector.AlertRaised += _ => signal.Set();

        try
        {
            detector.Feed("   \n\t\n");

            Assert.False(signal.Wait(WaitForNoAlertMs));
        }
        finally
        {
            detector.Dispose();
        }
    }

    // ---- ANSI stripping ----------------------------------------------------------------------

    [Fact]
    public void Feed_LineWrappedInAnsiEscapes_StillFiresAlert()
    {
        // Real Claude output is colorized — verify the detector strips ANSI before regex match.
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        AlertEvent? captured = null;
        detector.AlertRaised += evt => { captured = evt; signal.Set(); };

        try
        {
            const string esc = "\x1B";
            string ansiWrapped = $"{esc}[1;33mDo you want to proceed?{esc}[0m\n";

            detector.Feed(ansiWrapped);

            Assert.True(signal.Wait(WaitForAlertMs));
            Assert.NotNull(captured);
            Assert.Equal(AlertType.ToolApproval, captured!.Type);
            // The captured message should contain the human-readable text from inside the ANSI wrapping.
            Assert.Contains("Do you want to proceed?", captured.Message);
        }
        finally
        {
            detector.Dispose();
        }
    }

    [Fact]
    public void Feed_PromptWrappedInAnsiEscapes_StillFiresInputRequired()
    {
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        AlertEvent? captured = null;
        detector.AlertRaised += evt => { captured = evt; signal.Set(); };

        try
        {
            const string esc = "\x1B";
            string ansiWrapped = $"{esc}[36m❯{esc}[0m\n";

            detector.Feed(ansiWrapped);

            Assert.True(signal.Wait(WaitForAlertMs));
            Assert.NotNull(captured);
            Assert.Equal(AlertType.InputRequired, captured!.Type);
        }
        finally
        {
            detector.Dispose();
        }
    }

    // ---- NotifyUserInteracted ----------------------------------------------------------------

    [Fact]
    public void NotifyUserInteracted_BeforeIdleTimer_CancelsPendingAlert()
    {
        // Feed a matching line, but interact before the 1500ms idle window expires.
        // The pending timer should be cancelled, so no AlertRaised should ever fire.
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        detector.AlertRaised += _ => signal.Set();

        try
        {
            detector.Feed("Do you want to continue?\n");
            // Cancel immediately — no sleep, no race against the 1500ms timer.
            detector.NotifyUserInteracted();

            Assert.False(signal.Wait(WaitForNoAlertMs),
                "NotifyUserInteracted should have cancelled the pending idle timer.");
        }
        finally
        {
            detector.Dispose();
        }
    }

    [Fact]
    public void NotifyUserInteracted_RaisesAlertCleared()
    {
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        string? clearedSessionId = null;
        detector.AlertCleared += id => { clearedSessionId = id; signal.Set(); };

        try
        {
            detector.NotifyUserInteracted();

            Assert.True(signal.Wait(1000));
            Assert.Equal("s1", clearedSessionId);
        }
        finally
        {
            detector.Dispose();
        }
    }

    [Fact]
    public void NotifyUserInteracted_ResetsFiredFlag_SoNextMatchingFeedFiresAgain()
    {
        // Once an alert has fired, _alertFired latches true. NotifyUserInteracted clears it
        // so a subsequent matching feed re-fires.
        var detector = new AlertDetector("s1", "session");
        using var firstSignal = new ManualResetEventSlim(false);
        using var secondSignal = new ManualResetEventSlim(false);

        try
        {
            AlertEvent? first = null;
            Action<AlertEvent> firstHandler = evt => { first = evt; firstSignal.Set(); };
            detector.AlertRaised += firstHandler;

            detector.Feed("Do you want to proceed?\n");
            Assert.True(firstSignal.Wait(WaitForAlertMs), "First alert should have fired.");
            Assert.NotNull(first);
            detector.AlertRaised -= firstHandler;

            // Clear, then re-feed; expect a second fire.
            detector.NotifyUserInteracted();

            AlertEvent? second = null;
            detector.AlertRaised += evt => { second = evt; secondSignal.Set(); };

            detector.Feed("Allow this action?\n");
            Assert.True(secondSignal.Wait(WaitForAlertMs), "Second alert should have re-fired after NotifyUserInteracted.");
            Assert.NotNull(second);
            Assert.Equal(AlertType.ToolApproval, second!.Type);
        }
        finally
        {
            detector.Dispose();
        }
    }

    // ---- Idle-timer reset behavior -----------------------------------------------------------

    [Fact]
    public void Feed_LastLineWins_OnlyFinalLineEvaluatedForAlertType()
    {
        // Feed() processes all non-empty lines but only the LAST trimmed line is kept in
        // _lastLine. So if the final line is a tool-approval prompt, that's what we get —
        // even if earlier lines would also have matched a different type.
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        AlertEvent? captured = null;
        detector.AlertRaised += evt => { captured = evt; signal.Set(); };

        try
        {
            // Earlier line is an input-required pattern; final line is a tool-approval phrase.
            detector.Feed("Continue [y/N]\nDo you want to run this?\n");

            Assert.True(signal.Wait(WaitForAlertMs));
            Assert.NotNull(captured);
            Assert.Equal(AlertType.ToolApproval, captured!.Type);
        }
        finally
        {
            detector.Dispose();
        }
    }

    [Fact]
    public void Feed_MessageTruncatedAt100Chars_WhenLineIsLonger()
    {
        // Lines longer than 100 chars are truncated with "…".
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        AlertEvent? captured = null;
        detector.AlertRaised += evt => { captured = evt; signal.Set(); };

        try
        {
            // Build a 140-char line ending with an approval phrase.
            string padding = new string('x', 130);
            string line = padding + " Continue?";
            Assert.True(line.Length > 100);

            detector.Feed(line + "\n");

            Assert.True(signal.Wait(WaitForAlertMs));
            Assert.NotNull(captured);
            Assert.EndsWith("…", captured!.Message);  // U+2026 = …
            Assert.Equal(101, captured.Message.Length); // 100 chars + the ellipsis
        }
        finally
        {
            detector.Dispose();
        }
    }

    // ---- Dispose -----------------------------------------------------------------------------

    [Fact]
    public void Dispose_AfterFeed_PreventsAlertFromFiring()
    {
        var detector = new AlertDetector("s1", "session");
        using var signal = new ManualResetEventSlim(false);
        detector.AlertRaised += _ => signal.Set();

        detector.Feed("Do you want to continue?\n");
        detector.Dispose();

        Assert.False(signal.Wait(WaitForNoAlertMs),
            "Disposing should have stopped the pending idle timer.");
    }
}

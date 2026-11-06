using System;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Which sessions the "still running — close anyway?" warning lists on app close.
/// </summary>
public class SessionBusyRulesTests
{
    private static readonly DateTime Now = new(2026, 11, 6, 12, 0, 0, DateTimeKind.Utc);

    private static string? Reason(bool status = false, ProgramState? state = null, int? progress = null,
        int runs = 0, DateTime? lastOutput = null)
        => SessionBusyRules.Reason(status, state, progress, runs, lastOutput, Now);

    [Fact]
    public void FreshSession_NoOutput_IsIdle() => Assert.Null(Reason());

    [Fact]
    public void QuietShell_IsIdle() => Assert.Null(Reason(lastOutput: Now.AddMinutes(-2)));

    [Fact]
    public void RecentOutput_WithoutProgramStatus_IsBusy()
        => Assert.Equal("producing output", Reason(lastOutput: Now.AddSeconds(-1)));

    [Fact]
    public void OutputExactlyAtWindowEdge_IsBusy()
        => Assert.NotNull(Reason(lastOutput: Now - SessionBusyRules.RecentOutputWindow));

    [Fact]
    public void OutputJustOutsideWindow_IsIdle()
        => Assert.Null(Reason(lastOutput: Now - SessionBusyRules.RecentOutputWindow - TimeSpan.FromMilliseconds(1)));

    [Fact]
    public void Working_WithProgress()
        => Assert.Equal("working · 40%", Reason(status: true, state: ProgramState.Working, progress: 40));

    [Fact]
    public void Working_Indeterminate()
        => Assert.Equal("working", Reason(status: true, state: ProgramState.Working));

    [Fact]
    public void Blocked_IsBusy_MidTask()
        => Assert.Equal("waiting for you mid-task", Reason(status: true, state: ProgramState.Blocked));

    [Theory]
    [InlineData(ProgramState.Idle)]
    [InlineData(ProgramState.Done)]
    [InlineData(ProgramState.Error)]
    public void ProgramStatus_SettledStates_AreIdle_EvenWithRecentOutput(ProgramState state)
        // A 7501 session is judged by its report: an idle TUI may still repaint.
        => Assert.Null(Reason(status: true, state: state, lastOutput: Now));

    [Fact]
    public void ProgramStatus_NoRecords_IgnoresOutput()
        => Assert.Null(Reason(status: true, state: null, lastOutput: Now));

    [Fact]
    public void RunningRunCommand_IsBusy_EvenWhenTerminalIsQuiet()
        => Assert.Equal("1 run command running", Reason(runs: 1));

    [Fact]
    public void Reasons_Combine()
        => Assert.Equal("working · 3 run commands running",
            Reason(status: true, state: ProgramState.Working, runs: 3));

    [Fact]
    public void OutputAfterNowWasRead_IsBusy()
        // The PTY reader thread can stamp output just after the close handler read the clock.
        => Assert.NotNull(Reason(lastOutput: Now.AddMilliseconds(5)));
}

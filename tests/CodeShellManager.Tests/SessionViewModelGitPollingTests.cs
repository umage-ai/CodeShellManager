using System;
using CodeShellManager.Models;
using CodeShellManager.ViewModels;
using Xunit;

namespace CodeShellManager.Tests;

public class SessionViewModelGitPollingTests
{
    [Fact]
    public void GitPollInterval_LocalIsTenSeconds() =>
        Assert.Equal(TimeSpan.FromSeconds(10), SessionViewModel.GitPollIntervalFor(SessionKind.Local, isForeground: true));

    [Fact]
    public void GitPollInterval_WslIsSlower()
    {
        // Each WSL probe is a wsl.exe spawn (LxssManager hop) and keeps the VM awake;
        // three times the local cadence is the documented trade.
        Assert.Equal(TimeSpan.FromSeconds(30), SessionViewModel.GitPollIntervalFor(SessionKind.Wsl, isForeground: true));
    }

    // A poll is ~all process creation, so 46 background sessions polling every 10s was pure
    // overhead to learn nothing had changed. Real git operations arrive via GitRepoWatcher;
    // this slow poll only catches working-tree edits (issue #70).
    [Theory]
    [InlineData(SessionKind.Local)]
    [InlineData(SessionKind.Wsl)]
    public void GitPollInterval_BackgroundBacksOffHard(SessionKind kind) =>
        Assert.Equal(TimeSpan.FromSeconds(120),
            SessionViewModel.GitPollIntervalFor(kind, isForeground: false));

    [Fact]
    public void GitPollInterval_BackgroundIsAlwaysSlowerThanForeground()
    {
        foreach (var kind in new[] { SessionKind.Local, SessionKind.Wsl, SessionKind.Ssh })
        {
            Assert.True(
                SessionViewModel.GitPollIntervalFor(kind, isForeground: false) >
                SessionViewModel.GitPollIntervalFor(kind, isForeground: true),
                $"{kind}: a pane you cannot see must never poll more often than the one you can");
        }
    }
}

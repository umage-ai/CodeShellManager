using System;
using CodeShellManager.Models;
using CodeShellManager.ViewModels;
using Xunit;

namespace CodeShellManager.Tests;

public class SessionViewModelGitPollingTests
{
    [Fact]
    public void GitPollInterval_LocalIsTenSeconds() =>
        Assert.Equal(TimeSpan.FromSeconds(10), SessionViewModel.GitPollIntervalFor(SessionKind.Local));

    [Fact]
    public void GitPollInterval_WslIsSlower()
    {
        // Each WSL probe is a wsl.exe spawn (LxssManager hop) and keeps the VM awake;
        // three times the local cadence is the documented trade.
        Assert.Equal(TimeSpan.FromSeconds(30), SessionViewModel.GitPollIntervalFor(SessionKind.Wsl));
    }
}

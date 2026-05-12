using System.Collections.Generic;
using CodeShellManager.Models;
using Xunit;

namespace CodeShellManager.Tests;

public class RunCommandItemTests
{
    [Fact]
    public void EnsureSingleDefault_PromotesFirstWhenNoneMarked()
    {
        var list = new List<RunCommandItem>
        {
            new() { Label = "run",   CommandLine = "dotnet run" },
            new() { Label = "test",  CommandLine = "dotnet test" },
        };
        RunCommandItem.EnsureSingleDefault(list);
        Assert.True(list[0].IsDefault);
        Assert.False(list[1].IsDefault);
    }

    [Fact]
    public void EnsureSingleDefault_KeepsLastTrueWhenMultipleMarked()
    {
        var list = new List<RunCommandItem>
        {
            new() { Label = "a", CommandLine = "x", IsDefault = true },
            new() { Label = "b", CommandLine = "y", IsDefault = true },
            new() { Label = "c", CommandLine = "z" },
        };
        RunCommandItem.EnsureSingleDefault(list);
        // Convention: the LAST-marked default wins (matches the dialog's
        // "click row to promote" behavior — the most recent click is authoritative).
        Assert.False(list[0].IsDefault);
        Assert.True(list[1].IsDefault);
        Assert.False(list[2].IsDefault);
    }

    [Fact]
    public void EnsureSingleDefault_EmptyList_NoOp()
    {
        var list = new List<RunCommandItem>();
        RunCommandItem.EnsureSingleDefault(list);
        Assert.Empty(list);
    }
}

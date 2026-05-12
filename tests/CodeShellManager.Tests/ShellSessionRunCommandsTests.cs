using System.Collections.Generic;
using System.Text.Json;
using CodeShellManager.Models;
using Xunit;

namespace CodeShellManager.Tests;

public class ShellSessionRunCommandsTests
{
    [Fact]
    public void RunCommands_DefaultsToEmptyList()
    {
        var s = new ShellSession();
        Assert.NotNull(s.RunCommands);
        Assert.Empty(s.RunCommands);
    }

    [Fact]
    public void RunCommands_RoundTripsThroughJson()
    {
        var s = new ShellSession
        {
            RunCommands = new List<RunCommandItem>
            {
                new() { Id = "a", Label = "run",  CommandLine = "dotnet run",  IsDefault = true },
                new() { Id = "b", Label = "test", CommandLine = "dotnet test", IsDefault = false },
            }
        };
        string json = JsonSerializer.Serialize(s);
        var back = JsonSerializer.Deserialize<ShellSession>(json)!;
        Assert.Equal(2, back.RunCommands.Count);
        Assert.Equal("a", back.RunCommands[0].Id);
        Assert.True(back.RunCommands[0].IsDefault);
        Assert.Equal("dotnet test", back.RunCommands[1].CommandLine);
    }
}

using System.Linq;
using CodeShellManager.Models;
using Xunit;

namespace CodeShellManager.Tests;

public class RecentlyClosedEntryTests
{
    [Fact]
    public void FromSession_CopiesScalarAndSshFields()
    {
        var s = new ShellSession
        {
            Name = "alpha",
            WorkingFolder = @"C:\proj",
            Command = "claude",
            Args = "--resume foo",
            GroupId = "g1",
            ColorOverride = "#abcdef",
            IsRemote = true,
            SshUser = "alice",
            SshHost = "dev.example.com",
            SshPort = 2222,
            SshRemoteFolder = "/srv",
        };

        var e = RecentlyClosedEntry.FromSession(s);

        Assert.Equal("alpha", e.Name);
        Assert.Equal(@"C:\proj", e.WorkingFolder);
        Assert.Equal("claude", e.Command);
        Assert.Equal("--resume foo", e.Args);
        Assert.Equal("g1", e.GroupId);
        Assert.Equal("#abcdef", e.ColorOverride);
        Assert.True(e.IsRemote);
        Assert.Equal("alice", e.SshUser);
        Assert.Equal("dev.example.com", e.SshHost);
        Assert.Equal(2222, e.SshPort);
        Assert.Equal("/srv", e.SshRemoteFolder);
    }

    [Fact]
    public void FromSession_CopiesProfileOverrides()
    {
        var s = new ShellSession
        {
            ProfileFontFamily = "Cascadia",
            ProfileFontSize = 16,
            ProfileFontWeight = "bold",
            ProfileFontLigatures = true,
            ProfileCursorShape = "underline",
            ProfileCursorBlink = false,
            ProfilePadding = "8 8 8 8",
            ProfileBackgroundOpacity = 0.85,
            ProfileRetroEffect = true,
            ProfileColorSchemeJson = "{\"foreground\":\"#fff\"}",
        };

        var e = RecentlyClosedEntry.FromSession(s);

        Assert.Equal("Cascadia", e.ProfileFontFamily);
        Assert.Equal(16, e.ProfileFontSize);
        Assert.Equal("bold", e.ProfileFontWeight);
        Assert.True(e.ProfileFontLigatures);
        Assert.Equal("underline", e.ProfileCursorShape);
        Assert.False(e.ProfileCursorBlink);
        Assert.Equal("8 8 8 8", e.ProfilePadding);
        Assert.Equal(0.85, e.ProfileBackgroundOpacity);
        Assert.True(e.ProfileRetroEffect);
        Assert.Equal("{\"foreground\":\"#fff\"}", e.ProfileColorSchemeJson);
    }

    [Fact]
    public void FromSession_DeepCopiesRunCommandsWithFreshIds()
    {
        var s = new ShellSession();
        s.RunCommands.Add(new RunCommandItem
        {
            Id = "original-id",
            Label = "Test",
            CommandLine = "dotnet test",
            IsDefault = true,
            Mode = RunMode.PowerShell,
            PostRunUrl = "http://localhost:5173",
        });

        var e = RecentlyClosedEntry.FromSession(s);

        Assert.Single(e.RunCommands);
        var copy = e.RunCommands[0];
        Assert.NotEqual("original-id", copy.Id); // fresh Id
        Assert.Equal("Test", copy.Label);
        Assert.Equal("dotnet test", copy.CommandLine);
        Assert.True(copy.IsDefault);
        Assert.Equal(RunMode.PowerShell, copy.Mode);
        Assert.Equal("http://localhost:5173", copy.PostRunUrl);
        // Mutating the copy must NOT touch the source.
        copy.Label = "MUTATED";
        Assert.Equal("Test", s.RunCommands[0].Label);
    }

    [Fact]
    public void Subtitle_RemoteSession_ReturnsUserAtHost()
    {
        var e = new RecentlyClosedEntry
        {
            IsRemote = true,
            SshUser = "bob",
            SshHost = "dev.local",
            WorkingFolder = @"C:\should-be-ignored",
        };
        Assert.Equal("bob@dev.local", e.Subtitle);
    }

    [Fact]
    public void Subtitle_RemoteSessionWithoutUser_ReturnsHostOnly()
    {
        var e = new RecentlyClosedEntry
        {
            IsRemote = true,
            SshHost = "dev.local",
        };
        Assert.Equal("dev.local", e.Subtitle);
    }

    [Fact]
    public void Subtitle_LocalSession_ReturnsWorkingFolder()
    {
        var e = new RecentlyClosedEntry { WorkingFolder = @"C:\proj" };
        Assert.Equal(@"C:\proj", e.Subtitle);
    }
}

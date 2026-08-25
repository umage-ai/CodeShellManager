using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class SessionConfigEditorTests
{
    private static ShellSession LocalSession() => new()
    {
        Name = "web",
        WorkingFolder = @"C:\src\web",
        Command = "claude",
        Args = "--continue",
    };

    private static ShellSession RemoteSession() => new()
    {
        Name = "dev box",
        IsRemote = true,
        SshUser = "alice",
        SshHost = "dev.example.com",
        SshPort = 22,
        SshRemoteFolder = "/home/alice/project",
        Command = "bash",
    };

    [Fact]
    public void Diff_IdenticalDraft_ReportsNoChange()
    {
        var s = LocalSession();
        var change = SessionConfigEditor.Diff(s, SessionConfigDraft.FromSession(s));

        Assert.False(change.AnyChange);
        Assert.False(change.RequiresRelaunch);
        Assert.False(change.WorkingFolderChanged);
        Assert.False(change.AppearanceChanged);
    }

    [Fact]
    public void Diff_RenameOnly_NoRelaunch()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.Name = "frontend";

        var change = SessionConfigEditor.Diff(s, d);

        Assert.True(change.AnyChange);
        Assert.False(change.RequiresRelaunch);
    }

    [Theory]
    [InlineData("codex", "--continue")]
    [InlineData("claude", "")]
    [InlineData("claude", "--dangerously-skip-permissions")]
    public void Diff_LaunchLineChanged_RequiresRelaunch(string command, string args)
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.Command = command;
        d.Args = args;

        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
    }

    [Fact]
    public void Diff_WorkingFolderChanged_RequiresRelaunchAndFlagsFolder()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WorkingFolder = @"C:\src\api";

        var change = SessionConfigEditor.Diff(s, d);

        Assert.True(change.RequiresRelaunch);
        Assert.True(change.WorkingFolderChanged);
    }

    [Fact]
    public void Diff_WorkingFolderTrailingSlashOnly_IsNotAChange()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WorkingFolder = @"c:\src\web\";

        var change = SessionConfigEditor.Diff(s, d);

        Assert.False(change.WorkingFolderChanged);
        Assert.False(change.RequiresRelaunch);
    }

    [Fact]
    public void Diff_SwitchingLocalToRemote_RequiresRelaunch()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.IsRemote = true;
        d.SshHost = "dev.example.com";
        d.WorkingFolder = "";

        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
    }

    [Theory]
    [InlineData("bob", "dev.example.com", 22, "/home/alice/project")]
    [InlineData("alice", "other.example.com", 22, "/home/alice/project")]
    [InlineData("alice", "dev.example.com", 2222, "/home/alice/project")]
    [InlineData("alice", "dev.example.com", 22, "/srv/app")]
    public void Diff_SshTargetChanged_RequiresRelaunch(string user, string host, int port, string folder)
    {
        var s = RemoteSession();
        var d = SessionConfigDraft.FromSession(s);
        d.SshUser = user;
        d.SshHost = host;
        d.SshPort = port;
        d.SshRemoteFolder = folder;

        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
    }

    [Fact]
    public void Diff_LocalSessionWithLeftoverSshFields_DoesNotSeeAnSshChange()
    {
        // A session that was switched remote -> local keeps its old ssh values in state.json;
        // the local form reports blanks for them, which must not read as a relaunch trigger.
        var s = LocalSession();
        s.SshUser = "alice";
        s.SshHost = "dev.example.com";
        var d = SessionConfigDraft.FromSession(s);
        d.SshUser = "";
        d.SshHost = "";

        var change = SessionConfigEditor.Diff(s, d);

        Assert.False(change.RequiresRelaunch);
        Assert.False(change.AnyChange);
    }

    [Fact]
    public void Diff_AppearanceOverrideAdded_AppliesLiveWithoutRelaunch()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.ProfileFontFamily = "Cascadia Code";
        d.ProfileFontSize = 15;

        var change = SessionConfigEditor.Diff(s, d);

        Assert.True(change.AppearanceChanged);
        Assert.False(change.RequiresRelaunch);
    }

    [Fact]
    public void Diff_AppearanceOverrideCleared_RequiresRelaunch()
    {
        // TerminalBridge.ApplyProfileOverrides only ever sets options, so going back to
        // "use the global settings" can't be pushed to a live xterm.
        var s = LocalSession();
        s.ProfileFontFamily = "Cascadia Code";
        var d = SessionConfigDraft.FromSession(s);
        d.ProfileFontFamily = null;

        var change = SessionConfigEditor.Diff(s, d);

        Assert.True(change.AppearanceChanged);
        Assert.True(change.RequiresRelaunch);
    }

    [Fact]
    public void Diff_TransparencyTurnedOn_RequiresRelaunch()
    {
        // Transparent sessions navigate to a different xterm host page.
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.ProfileBackgroundOpacity = 0.8;

        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
    }

    [Fact]
    public void Diff_TransparencyTurnedOff_RequiresRelaunch()
    {
        var s = LocalSession();
        s.ProfileBackgroundOpacity = 0.8;
        var d = SessionConfigDraft.FromSession(s);
        d.ProfileBackgroundOpacity = 1.0;

        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
    }

    [Fact]
    public void Diff_OpaqueOpacityRecorded_IsNotATransparencyChange()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.ProfileBackgroundOpacity = 1.0;

        var change = SessionConfigEditor.Diff(s, d);

        Assert.True(change.AppearanceChanged);
        Assert.False(change.RequiresRelaunch);
    }

    [Fact]
    public void Apply_WritesEveryFormFieldAndLeavesRuntimeStateAlone()
    {
        var s = LocalSession();
        s.GroupId = "group-1";
        s.Status = SessionStatus.Running;
        s.IsDormant = false;
        s.RunCommands.Add(new RunCommandItem { Label = "build", CommandLine = "dotnet build" });
        string originalId = s.Id;

        var d = new SessionConfigDraft
        {
            Name = "api",
            WorkingFolder = @"C:\src\api",
            Command = "codex",
            Args = "--verbose",
            IsRemote = false,
            ProfileFontFamily = "Cascadia Code",
            ProfileFontSize = 15,
            ProfileCursorShape = "bar",
            ProfileBackgroundOpacity = 0.9,
            ProfileColorSchemeJson = "{}",
        };

        SessionConfigEditor.Apply(s, d);

        Assert.Equal("api", s.Name);
        Assert.Equal(@"C:\src\api", s.WorkingFolder);
        Assert.Equal("codex", s.Command);
        Assert.Equal("--verbose", s.Args);
        Assert.Equal("Cascadia Code", s.ProfileFontFamily);
        Assert.Equal(15, s.ProfileFontSize);
        Assert.Equal("bar", s.ProfileCursorShape);
        Assert.Equal(0.9, s.ProfileBackgroundOpacity);
        Assert.Equal("{}", s.ProfileColorSchemeJson);

        // Untouched by the form
        Assert.Equal(originalId, s.Id);
        Assert.Equal("group-1", s.GroupId);
        Assert.Equal(SessionStatus.Running, s.Status);
        Assert.Single(s.RunCommands);
    }

    [Fact]
    public void Apply_ClearedOverrides_ActuallyClearThem()
    {
        var s = LocalSession();
        s.ProfileFontFamily = "Cascadia Code";
        s.ProfileFontSize = 15;
        s.ProfileRetroEffect = true;
        s.ProfileColorSchemeJson = "{\"background\":\"#000000\"}";

        var d = SessionConfigDraft.FromSession(s);
        d.ProfileFontFamily = null;
        d.ProfileFontSize = null;
        d.ProfileRetroEffect = null;
        d.ProfileColorSchemeJson = null;

        SessionConfigEditor.Apply(s, d);

        Assert.Null(s.ProfileFontFamily);
        Assert.Null(s.ProfileFontSize);
        Assert.Null(s.ProfileRetroEffect);
        Assert.Null(s.ProfileColorSchemeJson);
    }

    [Fact]
    public void Apply_SwitchingToRemote_UsesTheSshTargetAndDropsTheLocalFolder()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.IsRemote = true;
        d.WorkingFolder = "";
        d.SshUser = "alice";
        d.SshHost = "dev.example.com";
        d.SshPort = 2222;
        d.SshRemoteFolder = "/srv/app";
        d.Command = "bash";
        d.Args = "";

        SessionConfigEditor.Apply(s, d);

        Assert.True(s.IsRemote);
        Assert.Equal("", s.WorkingFolder);
        Assert.Equal("-p 2222 -t alice@dev.example.com \"cd '/srv/app' && bash\"", s.BuildSshArgs());
    }
}

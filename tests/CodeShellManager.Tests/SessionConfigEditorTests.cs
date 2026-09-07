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
        Kind = SessionKind.Ssh,
        SshUser = "alice",
        SshHost = "dev.example.com",
        SshPort = 22,
        SshRemoteFolder = "/home/alice/project",
        Command = "bash",
    };

    private static ShellSession WslSession() => new()
    {
        Name = "ubuntu proj",
        Kind = SessionKind.Wsl,
        WslDistro = "Ubuntu",
        WslUser = "alice",
        WslWorkingFolder = "/home/alice/proj",
        WorkingFolder = @"\\wsl$\Ubuntu\home\alice\proj",
        Command = "claude",
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
        d.Kind = SessionKind.Ssh;
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
            Kind = SessionKind.Local,
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
        d.Kind = SessionKind.Ssh;
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

    [Fact]
    public void Apply_SshToLocal_DemotesKind()
    {
        // Regression: with the promote-only IsRemote setter this silently left Kind=Ssh.
        var s = RemoteSession();
        var d = SessionConfigDraft.FromSession(s);
        d.Kind = SessionKind.Local;
        d.WorkingFolder = @"C:\src";

        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
        SessionConfigEditor.Apply(s, d);

        Assert.Equal(SessionKind.Local, s.Kind);
        Assert.False(s.IsRemote);
        Assert.Equal(@"C:\src", s.WorkingFolder);
    }

    [Fact]
    public void Diff_WslIdentical_NoChange()
    {
        var s = WslSession();
        Assert.False(SessionConfigEditor.Diff(s, SessionConfigDraft.FromSession(s)).AnyChange);
    }

    [Fact]
    public void Diff_WslDistroChanged_RequiresRelaunchAndFolderChanged()
    {
        var s = WslSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WslDistro = "Debian";
        var c = SessionConfigEditor.Diff(s, d);
        Assert.True(c.AnyChange);
        Assert.True(c.RequiresRelaunch);
        Assert.True(c.WorkingFolderChanged);
    }

    [Fact]
    public void Diff_WslUserChanged_RequiresRelaunchButFolderUnchanged()
    {
        var s = WslSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WslUser = "root";
        var c = SessionConfigEditor.Diff(s, d);
        Assert.True(c.RequiresRelaunch);
        Assert.False(c.WorkingFolderChanged);
    }

    [Fact]
    public void Diff_WslLinuxFolderTrailingSlash_IsNotAChange()
    {
        var s = WslSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WslWorkingFolder = "/home/alice/proj/";
        Assert.False(SessionConfigEditor.Diff(s, d).AnyChange);
    }

    [Fact]
    public void Apply_WslFolderChanged_ResyncsUncWorkingFolder()
    {
        var s = WslSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WslWorkingFolder = "/srv/other";
        SessionConfigEditor.Apply(s, d);
        Assert.Equal("/srv/other", s.WslWorkingFolder);
        Assert.Equal(@"\\wsl$\Ubuntu\srv\other", s.WorkingFolder);
    }

    [Fact]
    public void Apply_LocalToWsl_SetsKindAndUnc()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.Kind = SessionKind.Wsl;
        d.WslDistro = "Ubuntu";
        d.WslWorkingFolder = "/home/alice";
        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
        SessionConfigEditor.Apply(s, d);
        Assert.Equal(SessionKind.Wsl, s.Kind);
        Assert.Equal(@"\\wsl$\Ubuntu\home\alice", s.WorkingFolder);
    }

    [Fact]
    public void Diff_StaleWslFieldsOnLocalSession_DoNotCount()
    {
        var s = LocalSession();
        s.WslDistro = "leftover";
        var d = SessionConfigDraft.FromSession(s);
        d.WslDistro = "";
        Assert.False(SessionConfigEditor.Diff(s, d).AnyChange);
    }

    [Fact]
    public void Diff_StaleSshFieldsOnWslSession_DoNotCount()
    {
        // Mirror of Diff_StaleWslFieldsOnLocalSession_DoNotCount: SSH fields only count
        // while the session stays SSH (see Diff's "sameKind" guards), so leftovers from
        // a previous Local/Ssh mode blanked in the draft must not read as a change either.
        var s = WslSession();
        s.SshUser = "leftover";
        s.SshHost = "leftover.example.com";
        s.SshPort = 2222;
        s.SshRemoteFolder = "/leftover";
        var d = SessionConfigDraft.FromSession(s);
        d.SshUser = "";
        d.SshHost = "";
        d.SshPort = 22;
        d.SshRemoteFolder = "";
        Assert.False(SessionConfigEditor.Diff(s, d).AnyChange);
    }
}

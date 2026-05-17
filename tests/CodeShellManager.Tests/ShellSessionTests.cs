using CodeShellManager.Models;
using Xunit;

namespace CodeShellManager.Tests;

public class ShellSessionTests
{
    [Fact]
    public void BuildSshArgs_DefaultPort_OmitsPortFlag()
    {
        var s = new ShellSession
        {
            IsRemote = true, SshUser = "alice", SshHost = "dev.example.com",
            SshPort = 22, SshRemoteFolder = "/home/alice/project", Command = "bash"
        };
        Assert.Equal("-t alice@dev.example.com \"cd '/home/alice/project' && bash\"",
            s.BuildSshArgs());
    }

    [Fact]
    public void BuildSshArgs_NonDefaultPort_IncludesPortFlag()
    {
        var s = new ShellSession
        {
            IsRemote = true, SshUser = "bob", SshHost = "192.168.1.100",
            SshPort = 2222, SshRemoteFolder = "", Command = "pwsh"
        };
        Assert.Equal("-p 2222 -t bob@192.168.1.100 \"pwsh\"", s.BuildSshArgs());
    }

    [Fact]
    public void BuildSshArgs_NoUser_OmitsAtSign()
    {
        var s = new ShellSession
        {
            IsRemote = true, SshUser = "", SshHost = "myserver",
            SshPort = 22, SshRemoteFolder = "", Command = "bash"
        };
        Assert.Equal("-t myserver \"bash\"", s.BuildSshArgs());
    }

    [Fact]
    public void BuildSshArgs_NoRemoteFolder_OmitsCdCommand()
    {
        var s = new ShellSession
        {
            IsRemote = true, SshUser = "ci", SshHost = "build-server",
            SshPort = 22, SshRemoteFolder = "", Command = "bash"
        };
        Assert.Equal("-t ci@build-server \"bash\"", s.BuildSshArgs());
    }

    [Fact]
    public void FullCommandLine_Remote_StartsWithSsh()
    {
        var s = new ShellSession
        {
            IsRemote = true, SshUser = "alice", SshHost = "dev.example.com",
            SshPort = 22, SshRemoteFolder = "/proj", Command = "bash"
        };
        Assert.StartsWith("ssh ", s.FullCommandLine);
    }

    [Fact]
    public void BuildSshArgs_FolderWithSpaces_ProducesValidCommand()
    {
        var s = new ShellSession
        {
            IsRemote = true, SshUser = "alice", SshHost = "dev.example.com",
            SshPort = 22, SshRemoteFolder = "/home/alice/my project", Command = "bash"
        };
        Assert.Equal("-t alice@dev.example.com \"cd '/home/alice/my project' && bash\"",
            s.BuildSshArgs());
    }

    [Fact]
    public void FullCommandLine_Local_ReturnsCommandAndArgs()
    {
        var s = new ShellSession { Command = "claude", Args = "--continue" };
        Assert.Equal("claude --continue", s.FullCommandLine);
    }

    [Fact]
    public void BuildSshArgs_EmptyHost_ThrowsInvalidOperationException()
    {
        var s = new ShellSession
        {
            IsRemote = true, SshUser = "alice", SshHost = "",
            SshPort = 22, Command = "bash"
        };
        Assert.Throws<InvalidOperationException>(() => s.BuildSshArgs());
    }

    [Fact]
    public void IsRemote_SetTrue_PromotesKindToSsh()
    {
        var s = new ShellSession { IsRemote = true };
        Assert.Equal(SessionKind.Ssh, s.Kind);
        Assert.True(s.IsRemote);
    }

    [Fact]
    public void IsRemote_GetterTrueOnlyForSsh()
    {
        Assert.False(new ShellSession { Kind = SessionKind.Local }.IsRemote);
        Assert.True(new ShellSession { Kind = SessionKind.Ssh }.IsRemote);
        Assert.False(new ShellSession { Kind = SessionKind.Wsl }.IsRemote);
    }

    [Fact]
    public void BuildWslArgs_HappyPath_BuildsExpectedShape()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu", WslUser = "alice",
            WslWorkingFolder = "/home/alice/proj", Command = "claude",
        };
        Assert.Equal("-d Ubuntu -u alice --cd /home/alice/proj -- bash -lc \"claude\"",
            s.BuildWslArgs());
    }

    [Fact]
    public void BuildWslArgs_NoUser_OmitsUserFlag()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Debian",
            WslWorkingFolder = "/srv", Command = "bash",
        };
        Assert.Equal("-d Debian --cd /srv -- bash -lc \"bash\"", s.BuildWslArgs());
    }

    [Fact]
    public void BuildWslArgs_NoWorkingFolder_OmitsCdFlag()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu", Command = "bash",
        };
        Assert.Equal("-d Ubuntu -- bash -lc \"bash\"", s.BuildWslArgs());
    }

    [Fact]
    public void BuildWslArgs_ArgsAppendedToShell()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu",
            Command = "claude", Args = "--continue",
        };
        Assert.Contains("bash -lc \"claude --continue\"", s.BuildWslArgs());
    }

    [Fact]
    public void BuildWslArgs_EmptyDistro_ThrowsInvalidOperationException()
    {
        var s = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "", Command = "bash" };
        Assert.Throws<InvalidOperationException>(() => s.BuildWslArgs());
    }

    [Fact]
    public void FullCommandLine_Wsl_StartsWithWslExe()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu",
            Command = "claude",
        };
        Assert.StartsWith("wsl.exe ", s.FullCommandLine);
    }

    [Fact]
    public void DefaultDisplayName_WslWithFolder_IsDistroAndLeaf()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu",
            WslWorkingFolder = "/home/alice/proj",
        };
        Assert.Equal("Ubuntu: proj", s.DefaultDisplayName);
    }

    [Fact]
    public void AccentKey_Wsl_DistinctFromLocal()
    {
        var wsl = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu", WslWorkingFolder = "/proj",
        };
        var local = new ShellSession { WorkingFolder = "/proj" };
        Assert.NotEqual(wsl.AccentKey, local.AccentKey);
    }
}

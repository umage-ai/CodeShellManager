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
}

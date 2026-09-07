using System;
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
    public void IsRemote_SetTrue_SetsKindSsh()
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
        Assert.Equal("-d Ubuntu -u alice --cd /home/alice/proj -e bash -lc \"claude\"",
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
        Assert.Equal("-d Debian --cd /srv -e bash -lc \"bash\"", s.BuildWslArgs());
    }

    [Fact]
    public void BuildWslArgs_NoWorkingFolder_OmitsCdFlag()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu", Command = "bash",
        };
        Assert.Equal("-d Ubuntu -e bash -lc \"bash\"", s.BuildWslArgs());
    }

    [Fact]
    public void BuildWslArgs_UsesExecFlag_NotBareDashDashSeparator()
    {
        // Fix for wsl.exe pre-expanding our payload: "--" runs the trailing command through
        // the distro's default login shell first (a second, unwanted expansion pass), while
        // "-e"/"--exec" runs it directly. Guard both that -e is present and that no bare "--"
        // token slipped back in (a substring check alone wouldn't catch "--cd").
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu", Command = "claude",
        };
        string args = s.BuildWslArgs();
        Assert.Contains(" -e ", args);
        Assert.DoesNotContain(args.Split(' '), token => token == "--");
    }

    [Fact]
    public void BuildWslArgs_ResolvedWslShellSet_UsedAsExecShell()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "docker-desktop", Command = "ls",
            ResolvedWslShell = "sh",
        };
        Assert.Equal("-d docker-desktop -e sh -lc \"ls\"", s.BuildWslArgs());
    }

    [Fact]
    public void BuildWslArgs_ResolvedWslShellUnset_DefaultsToBash()
    {
        var s = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Ubuntu", Command = "ls" };
        Assert.Equal("-d Ubuntu -e bash -lc \"ls\"", s.BuildWslArgs());
    }

    [Fact]
    public void BuildWslArgs_ResolvedWslShellSet_EmptyCommand_InnerPayloadUsesResolvedShellToo()
    {
        // The hardcoded "bash" fallback for a blank Command must become the resolved shell
        // too — otherwise an empty command box on a sh-only distro still emits "bash" as the
        // *inner* payload (bash -lc "bash"), which fails identically to the outer bug.
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "docker-desktop", Command = "",
            ResolvedWslShell = "sh",
        };
        Assert.Equal("-d docker-desktop -e sh -lc \"sh\"", s.BuildWslArgs());
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

    [Theory]
    [InlineData("Ubuntu", "Ubuntu")]
    [InlineData("", "\"\"")]
    [InlineData("/home/alice/proj", "/home/alice/proj")]
    [InlineData("/home/alice/my proj", "\"/home/alice/my proj\"")]
    [InlineData("with\"quote", "\"with\\\"quote\"")]
    public void QuoteForCmd_QuotesWhenNeeded(string input, string expected)
    {
        Assert.Equal(expected, ShellSession.QuoteForCmd(input));
    }

    [Fact]
    public void BuildWslArgs_LinuxPathWithSpaces_QuotesCdValue()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu",
            WslWorkingFolder = "/home/alice/my proj", Command = "claude",
        };
        Assert.Equal("-d Ubuntu --cd \"/home/alice/my proj\" -e bash -lc \"claude\"",
            s.BuildWslArgs());
    }

    [Fact]
    public void LaunchValidationError_Local_IsNull() =>
        Assert.Null(new ShellSession { Kind = SessionKind.Local, Command = "claude" }.LaunchValidationError);

    [Fact]
    public void LaunchValidationError_SshBlankHost_Reports() =>
        Assert.Contains("host", new ShellSession { Kind = SessionKind.Ssh }.LaunchValidationError!, StringComparison.OrdinalIgnoreCase);

    [Fact]
    public void LaunchValidationError_WslBlankDistro_Reports() =>
        Assert.Contains("distro", new ShellSession { Kind = SessionKind.Wsl }.LaunchValidationError!, StringComparison.OrdinalIgnoreCase);

    [Fact]
    public void LaunchValidationError_WslWithDistro_IsNull() =>
        Assert.Null(new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Ubuntu" }.LaunchValidationError);

    [Fact]
    public void FolderShort_LocalWorkingFolderWithEmbeddedNul_DoesNotThrow()
    {
        // DirectoryInfo(...).Name throws ArgumentException on a path containing an embedded
        // NUL — reachable from state.json during sidebar construction on the restore path.
        // Path.GetFileName (what DefaultDisplayName already uses) tolerates it.
        var s = new ShellSession { Kind = SessionKind.Local, WorkingFolder = "C:\\src\\web\u0000oops" };
        var ex = Record.Exception(() => s.FolderShort);
        Assert.Null(ex);
    }
}

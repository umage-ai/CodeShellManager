using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class RunInstanceTests
{
    [Fact]
    public void SingleQuoteEscape_PlainString_WrapsInQuotes()
    {
        Assert.Equal("'dotnet test'", RunInstance.SingleQuoteEscape("dotnet test"));
    }

    [Fact]
    public void SingleQuoteEscape_ContainsSingleQuote_EscapesIt()
    {
        // can't → 'can'\''t'
        Assert.Equal(@"'can'\''t'", RunInstance.SingleQuoteEscape("can't"));
    }

    [Fact]
    public void SingleQuoteEscape_Empty_ReturnsEmptyPair()
    {
        Assert.Equal("''", RunInstance.SingleQuoteEscape(""));
    }

    [Fact]
    public void BuildLocalCmd_WrapsForCmd()
    {
        Assert.Equal("/c \"dotnet test --filter X\"", RunInstance.BuildLocalCmd("dotnet test --filter X"));
    }

    [Fact]
    public void BuildSshArgs_LocalFolder_BuildsExpectedShape()
    {
        var p = new ShellSession
        {
            IsRemote = true, SshUser = "alice", SshHost = "dev.example.com",
            SshPort = 22, SshRemoteFolder = "/proj",
        };
        string args = RunInstance.BuildSshArgs(p, "cargo test");
        Assert.Equal("-t alice@dev.example.com \"cd '/proj' && bash -c 'cargo test'\"", args);
    }

    [Fact]
    public void BuildSshArgs_NonDefaultPort_IncludesPortFlag()
    {
        var p = new ShellSession
        {
            IsRemote = true, SshUser = "bob", SshHost = "h", SshPort = 2222, SshRemoteFolder = "",
        };
        string args = RunInstance.BuildSshArgs(p, "ls");
        Assert.StartsWith("-p 2222 ", args);
    }

    [Fact]
    public void BuildSshArgs_CommandLineWithApostrophe_IsEscaped()
    {
        var p = new ShellSession
        {
            IsRemote = true, SshUser = "u", SshHost = "h", SshPort = 22, SshRemoteFolder = "/p",
        };
        string args = RunInstance.BuildSshArgs(p, "echo it's me");
        Assert.Contains(@"bash -c 'echo it'\''s me'", args);
    }

    [Fact]
    public void BuildPwshArgs_RoundTripsCommandLineViaBase64()
    {
        // -EncodedCommand expects UTF-16 LE base64. Round-trip a known string with
        // tricky chars ($env: would otherwise be eaten by cmd.exe parsing) to
        // confirm the wrapping is preserved verbatim.
        const string cmd = "Write-Host \"hi $env:USERNAME!\" | Out-Default";
        string args = RunInstance.BuildPwshArgs(cmd);

        Assert.StartsWith("-NonInteractive -NoLogo -ExecutionPolicy Bypass -EncodedCommand ", args);
        string b64 = args.Substring(args.LastIndexOf(' ') + 1);
        string decoded = System.Text.Encoding.Unicode.GetString(System.Convert.FromBase64String(b64));
        Assert.Equal(cmd, decoded);
    }

    [Fact]
    public void BuildWslArgs_HappyPath_BuildsExpectedShape()
    {
        var p = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Ubuntu", WslUser = "alice",
            WslWorkingFolder = "/home/alice/proj",
        };
        string args = RunInstance.BuildWslArgs(p, "cargo test");
        Assert.Equal("-d Ubuntu -u alice --cd /home/alice/proj -- bash -lc 'cargo test'", args);
    }

    [Fact]
    public void BuildWslArgs_NoUserOrFolder_OmitsFlags()
    {
        var p = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Debian" };
        string args = RunInstance.BuildWslArgs(p, "ls");
        Assert.Equal("-d Debian -- bash -lc 'ls'", args);
    }

    [Fact]
    public void BuildWslArgs_CommandLineWithApostrophe_IsEscaped()
    {
        var p = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Ubuntu" };
        string args = RunInstance.BuildWslArgs(p, "echo it's me");
        Assert.Contains(@"bash -lc 'echo it'\''s me'", args);
    }
}

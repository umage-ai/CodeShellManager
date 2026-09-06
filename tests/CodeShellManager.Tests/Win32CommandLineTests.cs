using System;
using System.Runtime.InteropServices;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Round-trips our quoting through the real Win32 tokenizer. wsl.exe is started via
/// CreateProcess with no outer shell, so what CommandLineToArgvW produces is exactly what
/// wsl.exe (and then bash -lc) receives. Hand-written expectations were how the original
/// backslash bug slipped through.
/// </summary>
public class Win32CommandLineTests
{
    [DllImport("shell32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern IntPtr CommandLineToArgvW(string lpCmdLine, out int pNumArgs);

    [DllImport("kernel32.dll")]
    private static extern IntPtr LocalFree(IntPtr hMem);

    internal static string[] Split(string commandLine)
    {
        // Prefix a dummy program name: CommandLineToArgvW parses argv[0] with different rules.
        IntPtr argv = CommandLineToArgvW("x.exe " + commandLine, out int argc);
        if (argv == IntPtr.Zero) throw new InvalidOperationException("CommandLineToArgvW failed");
        try
        {
            var result = new string[argc - 1];
            for (int i = 1; i < argc; i++)
                result[i - 1] = Marshal.PtrToStringUni(Marshal.ReadIntPtr(argv, i * IntPtr.Size))!;
            return result;
        }
        finally { LocalFree(argv); }
    }

    [Theory]
    [InlineData("plain")]
    [InlineData("has space")]
    [InlineData("say \"hi\"")]
    [InlineData("trailing\\")]
    [InlineData("back\\\"slash-quote")]
    [InlineData("two\\\\\"bs")]
    [InlineData("sed -i 's/\\\"//g' f.txt")]
    [InlineData("grep -r \"\\\"foo\\\"\" .")]
    [InlineData("cp -r /src /dst\\")]
    [InlineData("")]
    public void QuoteForCmd_RoundTripsThroughCommandLineToArgvW(string value)
    {
        string[] argv = Split(ShellSession.QuoteForCmd(value, force: true));
        Assert.Single(argv);
        Assert.Equal(value, argv[0]);
    }

    [Theory]
    [InlineData("cargo test")]
    [InlineData("echo \"hi\"")]
    [InlineData("sed -i 's/\\\"//g' f.txt")]
    [InlineData("cp -r /src /dst\\")]
    [InlineData("printf '%s\\n' \"$HOME\"")]
    public void RunInstanceBuildWslArgs_BashPayloadArrivesIntact(string commandLine)
    {
        var p = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Ubuntu", WslWorkingFolder = "/home/a b" };
        string[] argv = Split(RunInstance.BuildWslArgs(p, commandLine));
        Assert.Equal(new[] { "-d", "Ubuntu", "--cd", "/home/a b", "--", "bash", "-lc", commandLine }, argv);
    }

    [Fact]
    public void ShellSessionBuildWslArgs_DistroWithSpaceAndUser_Tokenizes()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "My Distro", WslUser = "alice",
            WslWorkingFolder = "/home/alice", Command = "claude", Args = "--prompt \"fix the \\\"foo\\\" bug\"",
        };
        string[] argv = Split(s.BuildWslArgs());
        Assert.Equal(new[] { "-d", "My Distro", "-u", "alice", "--cd", "/home/alice", "--", "bash", "-lc",
            "claude --prompt \"fix the \\\"foo\\\" bug\"" }, argv);
    }

    [Fact]
    public void RunInstanceBuildWslArgs_BlankDistro_Throws()
    {
        var p = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "" };
        Assert.Throws<InvalidOperationException>(() => RunInstance.BuildWslArgs(p, "ls"));
    }
}

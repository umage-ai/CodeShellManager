using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

public class PseudoTerminalBuildCmdLineTests
{
    [Theory]
    [InlineData("pwsh")]
    [InlineData("pwsh.exe")]
    [InlineData("PWSH.EXE")]
    [InlineData("powershell")]
    [InlineData("powershell.exe")]
    [InlineData("cmd")]
    [InlineData("cmd.exe")]
    [InlineData("ssh")]
    [InlineData("bash")]
    [InlineData("wsl")]
    [InlineData("zsh")]
    [InlineData("sh")]
    [InlineData("nu")]
    [InlineData("fish")]
    public void PassthroughShells_AreNotWrapped(string shell)
    {
        var result = PseudoTerminal.BuildCmdLine(shell, $"{shell} --version", "pwsh.exe");
        Assert.Equal($"{shell} --version", result);
    }

    [Fact]
    public void NonShellCommand_IsWrappedInPwsh_WhenPwshIsTheWrapper()
    {
        var result = PseudoTerminal.BuildCmdLine("claudelocal", "claudelocal", "pwsh.exe");
        Assert.Equal("pwsh.exe -NoExit -Command claudelocal", result);
    }

    [Fact]
    public void NonShellCommand_FallsBackToWindowsPowerShell_WhenPwshUnavailable()
    {
        var result = PseudoTerminal.BuildCmdLine("claudelocal", "claudelocal", "powershell.exe");
        Assert.Equal("powershell.exe -NoExit -Command claudelocal", result);
    }

    [Fact]
    public void NonShellCommand_WithArguments_PreservesArgs()
    {
        var result = PseudoTerminal.BuildCmdLine("claude", "claude --resume abc123", "pwsh.exe");
        Assert.Equal("pwsh.exe -NoExit -Command claude --resume abc123", result);
    }

    [Fact]
    public void PowerShellFunctionName_IsWrapped_SoProfileLoadsAndDefinesIt()
    {
        // Regression: bare function names like 'claudelocal' / 'claudework' must be
        // run through the pwsh wrapper so the user's PowerShell 7 profile loads
        // before the function call resolves.
        var result = PseudoTerminal.BuildCmdLine("claudework", "claudework", "pwsh.exe");
        Assert.Equal("pwsh.exe -NoExit -Command claudework", result);
    }
}

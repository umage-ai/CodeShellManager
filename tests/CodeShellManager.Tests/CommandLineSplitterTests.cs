using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class CommandLineSplitterTests
{
    [Fact]
    public void Split_UnquotedExeWithArgs_ReturnsExeAndRest()
    {
        var (exe, args) = CommandLineSplitter.Split("cmd.exe /k foo");
        Assert.Equal("cmd.exe", exe);
        Assert.Equal("/k foo", args);
    }

    [Fact]
    public void Split_QuotedExeWithSpaces_StripsQuotes()
    {
        var (exe, args) = CommandLineSplitter.Split("\"C:\\Program Files\\app.exe\" -x");
        Assert.Equal("C:\\Program Files\\app.exe", exe);
        Assert.Equal("-x", args);
    }

    [Fact]
    public void Split_ExeOnly_ReturnsEmptyArgs()
    {
        var (exe, args) = CommandLineSplitter.Split("pwsh");
        Assert.Equal("pwsh", exe);
        Assert.Equal("", args);
    }

    [Fact]
    public void Split_WslWithFlags_ReturnsExeAndArgs()
    {
        var (exe, args) = CommandLineSplitter.Split("wsl.exe -d Ubuntu");
        Assert.Equal("wsl.exe", exe);
        Assert.Equal("-d Ubuntu", args);
    }

    [Fact]
    public void Split_ArgsContainQuotes_PreservesArgsVerbatim()
    {
        var (exe, args) = CommandLineSplitter.Split("bash -c \"echo hello\"");
        Assert.Equal("bash", exe);
        Assert.Equal("-c \"echo hello\"", args);
    }

    [Fact]
    public void Split_EmptyOrWhitespace_ReturnsEmpty()
    {
        var (exe, args) = CommandLineSplitter.Split("   ");
        Assert.Equal("", exe);
        Assert.Equal("", args);
    }

    [Fact]
    public void Split_LeadingWhitespace_Trims()
    {
        var (exe, args) = CommandLineSplitter.Split("  cmd /c echo hi");
        Assert.Equal("cmd", exe);
        Assert.Equal("/c echo hi", args);
    }
}

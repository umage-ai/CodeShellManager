using CodeShellManager.Views;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Headless coverage for the bits of <see cref="NewSessionDialog"/> that don't
/// require a window (parsing helpers). UI-level behavior lives in UITests.
/// </summary>
public class NewSessionDialogTests
{
    [Theory]
    [InlineData(@"\\wsl$\Ubuntu\home\alice\proj", "Ubuntu", "/home/alice/proj")]
    [InlineData(@"\\wsl.localhost\Debian\srv\app", "Debian", "/srv/app")]
    [InlineData(@"\\wsl$\Ubuntu", "Ubuntu", "")]
    [InlineData(@"\\wsl$\Ubuntu\", "Ubuntu", "")]
    [InlineData(@"C:\proj", "", "")]
    [InlineData("", "", "")]
    public void ParseWslUncPath_KnownShapes(string unc, string expectedDistro, string expectedLinux)
    {
        var (distro, linuxPath) = NewSessionDialog.ParseWslUncPath(unc);
        Assert.Equal(expectedDistro, distro);
        Assert.Equal(expectedLinux, linuxPath);
    }

    [Fact]
    public void ParseWslUncPath_ForwardSlashes_Normalized()
    {
        var (distro, linuxPath) = NewSessionDialog.ParseWslUncPath(@"//wsl$/Ubuntu/home/alice");
        Assert.Equal("Ubuntu", distro);
        Assert.Equal("/home/alice", linuxPath);
    }
}

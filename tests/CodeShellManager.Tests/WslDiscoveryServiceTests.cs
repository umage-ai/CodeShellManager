using System.Linq;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class WslDiscoveryServiceTests
{
    // Sample copied from `wsl -l -v` on a host with two distros installed. The
    // leading whitespace in front of "NAME" and the spacing are intentional —
    // wsl pads columns with spaces, never tabs.
    private const string SampleOutput =
        "  NAME                   STATE           VERSION\n" +
        "* Ubuntu                 Running         2\n" +
        "  Debian                 Stopped         2\n";

    [Fact]
    public void Parse_TwoDistros_ReturnsBoth()
    {
        var result = WslDiscoveryService.Parse(SampleOutput);
        Assert.Equal(2, result.Count);
    }

    [Fact]
    public void Parse_MarksDefaultDistro()
    {
        var result = WslDiscoveryService.Parse(SampleOutput);
        Assert.Single(result, d => d.IsDefault);
        Assert.Equal("Ubuntu", result[0].Name); // default sorted first
    }

    [Fact]
    public void Parse_ParsesVersionAndState()
    {
        var result = WslDiscoveryService.Parse(SampleOutput);
        var ubuntu = result.Single(d => d.Name == "Ubuntu");
        Assert.Equal(2, ubuntu.Version);
        Assert.Equal("Running", ubuntu.State);
    }

    [Fact]
    public void Parse_EmptyInput_ReturnsEmpty()
    {
        Assert.Empty(WslDiscoveryService.Parse(""));
        Assert.Empty(WslDiscoveryService.Parse("   \n"));
    }

    [Fact]
    public void Parse_HeaderOnly_ReturnsEmpty()
    {
        Assert.Empty(WslDiscoveryService.Parse("  NAME                   STATE           VERSION\n"));
    }

    [Fact]
    public void Parse_NonDefaultThenDefault_OrdersDefaultFirst()
    {
        const string reversed =
            "  NAME       STATE           VERSION\n" +
            "  Debian     Stopped         2\n" +
            "* Ubuntu     Running         2\n";
        var result = WslDiscoveryService.Parse(reversed);
        Assert.Equal("Ubuntu", result[0].Name);
        Assert.True(result[0].IsDefault);
    }

    [Fact]
    public void ToUncPath_HappyPath()
    {
        Assert.Equal(@"\\wsl$\Ubuntu\home\alice\proj",
            WslDiscoveryService.ToUncPath("Ubuntu", "/home/alice/proj"));
    }

    [Fact]
    public void ToUncPath_NoLinuxPath_ReturnsDistroRoot()
    {
        Assert.Equal(@"\\wsl$\Ubuntu",
            WslDiscoveryService.ToUncPath("Ubuntu", ""));
    }

    [Fact]
    public void ToUncPath_NoDistro_ReturnsEmpty()
    {
        Assert.Equal("", WslDiscoveryService.ToUncPath("", "/home/x"));
    }

    [Fact]
    public void Parse_DistroNameWithSpace_ParsesNameCorrectly()
    {
        // `wsl --import "My Distro" ...` produces a row where NAME spans two tokens.
        // Old parser took just the first token; the from-the-end approach takes
        // the trailing two columns as STATE/VERSION and joins the rest as NAME.
        const string raw =
            "  NAME             STATE           VERSION\n" +
            "* My Distro        Running         2\n";
        var result = WslDiscoveryService.Parse(raw);
        Assert.Single(result);
        Assert.Equal("My Distro", result[0].Name);
        Assert.Equal("Running", result[0].State);
        Assert.Equal(2, result[0].Version);
        Assert.True(result[0].IsDefault);
    }
}

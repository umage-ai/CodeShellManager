using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class BuiltInTerminalSchemesTests
{
    [Theory]
    [InlineData("Campbell")]
    [InlineData("Campbell Powershell")]
    [InlineData("Vintage")]
    [InlineData("One Half Dark")]
    [InlineData("One Half Light")]
    [InlineData("Solarized Dark")]
    [InlineData("Solarized Light")]
    [InlineData("Tango Dark")]
    [InlineData("Tango Light")]
    public void Lookup_BuiltInName_ReturnsScheme(string name)
        => Assert.NotNull(BuiltInTerminalSchemes.Lookup(name));

    [Theory]
    [InlineData("nonsense")]
    [InlineData("")]
    public void Lookup_UnknownName_ReturnsNull(string name)
        => Assert.Null(BuiltInTerminalSchemes.Lookup(name));

    [Fact]
    public void Lookup_BuiltInScheme_HasBackgroundField()
    {
        var scheme = BuiltInTerminalSchemes.Lookup("Campbell")!.Value;
        Assert.True(scheme.TryGetProperty("background", out var bg));
        Assert.StartsWith("#", bg.GetString());
    }
}

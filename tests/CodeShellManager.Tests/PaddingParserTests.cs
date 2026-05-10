using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class PaddingParserTests
{
    [Theory]
    [InlineData("8", "8px")]
    [InlineData("0", "0px")]
    [InlineData("8, 12", "8px 12px")]
    [InlineData("4, 8, 4, 8", "4px 8px 4px 8px")]
    [InlineData("  6  ,7  ", "6px 7px")]
    [InlineData("8,8,8,8", "8px 8px 8px 8px")]
    public void Parse_ValidShorthand_ReturnsCss(string input, string expected)
        => Assert.Equal(expected, PaddingParser.Parse(input));

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("abc")]
    [InlineData("8, 8, 8")]              // unsupported 3-value form
    [InlineData("8, 8, 8, 8, 8")]        // too many values
    public void Parse_InvalidOrUnsupported_ReturnsNull(string input)
        => Assert.Null(PaddingParser.Parse(input));
}

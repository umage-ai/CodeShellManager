using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class CursorShapeMapperTests
{
    [Theory]
    [InlineData("bar", "bar", null)]
    [InlineData("filledBox", "block", null)]
    [InlineData("vintage", "block", null)]
    [InlineData("emptyBox", "block", false)]
    [InlineData("underscore", "underline", null)]
    [InlineData("doubleUnderscore", "underline", null)]
    public void Map_KnownShapes_ReturnsExpected(string wtShape, string expectedStyle, bool? expectedBlink)
    {
        var (style, blink) = CursorShapeMapper.Map(wtShape);
        Assert.Equal(expectedStyle, style);
        Assert.Equal(expectedBlink, blink);
    }

    [Theory]
    [InlineData("")]
    [InlineData(null)]
    [InlineData("nonsense")]
    public void Map_UnknownOrEmpty_ReturnsNullStyle(string? wtShape)
    {
        var (style, blink) = CursorShapeMapper.Map(wtShape);
        Assert.Null(style);
        Assert.Null(blink);
    }
}

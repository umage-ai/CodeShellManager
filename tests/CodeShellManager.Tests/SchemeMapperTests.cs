using System.Text.Json;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class SchemeMapperTests
{
    private static JsonElement Parse(string json)
        => JsonDocument.Parse(json).RootElement.Clone();

    [Fact]
    public void Map_BasicScheme_ProducesXtermKeys()
    {
        var scheme = Parse("""
            {
              "name": "Demo",
              "background": "#0C0C0C",
              "foreground": "#CCCCCC",
              "cursorColor": "#FFFFFF",
              "selectionBackground": "#264F78",
              "black": "#000000", "red": "#C50F1F", "green": "#13A10E",
              "yellow": "#C19C00", "blue": "#0037DA", "purple": "#881798",
              "cyan": "#3A96DD", "white": "#CCCCCC",
              "brightBlack": "#767676", "brightRed": "#E74856",
              "brightGreen": "#16C60C", "brightYellow": "#F9F1A5",
              "brightBlue": "#3B78FF", "brightPurple": "#B4009E",
              "brightCyan": "#61D6D6", "brightWhite": "#F2F2F2"
            }
            """);

        string json = SchemeMapper.ToXtermThemeJson(scheme, opacity: 1.0)!;
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;

        Assert.Equal("#0C0C0C", root.GetProperty("background").GetString());
        Assert.Equal("#CCCCCC", root.GetProperty("foreground").GetString());
        Assert.Equal("#FFFFFF", root.GetProperty("cursor").GetString());
        Assert.Equal("#264F78", root.GetProperty("selectionBackground").GetString());
        Assert.Equal("#881798", root.GetProperty("magenta").GetString());
        Assert.Equal("#B4009E", root.GetProperty("brightMagenta").GetString());
        Assert.False(root.TryGetProperty("purple", out _));
        Assert.False(root.TryGetProperty("brightPurple", out _));
    }

    [Fact]
    public void Map_MissingCursor_OmitsKey()
    {
        var scheme = Parse("""{ "name":"x", "background":"#000", "foreground":"#FFF" }""");
        string json = SchemeMapper.ToXtermThemeJson(scheme, 1.0)!;
        using var doc = JsonDocument.Parse(json);
        Assert.False(doc.RootElement.TryGetProperty("cursor", out _));
    }

    [Fact]
    public void Map_Opacity_RewritesBackgroundAsRgba()
    {
        var scheme = Parse("""{ "name":"x", "background":"#0C0C0C", "foreground":"#FFFFFF" }""");
        string json = SchemeMapper.ToXtermThemeJson(scheme, opacity: 0.8)!;
        using var doc = JsonDocument.Parse(json);
        Assert.Equal("rgba(12, 12, 12, 0.8)", doc.RootElement.GetProperty("background").GetString());
    }

    [Fact]
    public void Map_Null_ReturnsNull()
        => Assert.Null(SchemeMapper.ToXtermThemeJson(null, 1.0));
}

using System;
using System.Collections.Generic;
using System.Linq;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class ColorServiceTests
{
    // The 12-color palette baked into ColorService. Kept here verbatim so a palette
    // change in the source forces a deliberate update here too.
    private static readonly string[] ExpectedPalette =
    [
        "#FF6B6B", "#FF9E42", "#FFD166", "#AEDE68",
        "#51CF66", "#38D9A9", "#66D9E8", "#4DABF7",
        "#748FFC", "#9775FA", "#F783AC", "#FF6B95",
    ];

    [Fact]
    public void GetHexColor_IsDeterministic_SameInputProducesSameColor()
    {
        const string key = @"C:\projects\my-app";
        var first = ColorService.GetHexColor(key);
        var second = ColorService.GetHexColor(key);
        Assert.Equal(first, second);
    }

    [Theory]
    [InlineData(@"C:\projects\alpha")]
    [InlineData(@"C:\projects\beta")]
    [InlineData("user@host.example.com")]
    [InlineData("")]
    public void GetHexColor_IsDeterministic_AcrossMultipleCalls(string key)
    {
        // Call several times to make sure there's no hidden state.
        var results = Enumerable.Range(0, 5)
            .Select(_ => ColorService.GetHexColor(key))
            .Distinct()
            .ToList();
        Assert.Single(results);
    }

    [Fact]
    public void GetHexColor_DifferentKeys_MapToDifferentColors_OnSmallSample()
    {
        // Not strictly guaranteed by any hash function, but this small handpicked
        // sample is known to spread across distinct palette entries for FNV-1a.
        var a = ColorService.GetHexColor(@"C:\proj\a");
        var b = ColorService.GetHexColor(@"C:\proj\b");
        Assert.NotEqual(a, b);
    }

    [Theory]
    [InlineData(@"C:\projects\my-app")]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("user@host:22")]
    [InlineData(@"\\unc\share\path")]
    public void GetHexColor_ReturnsValidHexFormat(string key)
    {
        var hex = ColorService.GetHexColor(key);
        Assert.NotNull(hex);
        Assert.Equal(7, hex.Length);
        Assert.StartsWith("#", hex);
        Assert.Matches("^#[0-9A-Fa-f]{6}$", hex);
    }

    [Fact]
    public void GetHexColor_ReturnedValue_IsAlwaysFromPalette()
    {
        var paletteSet = new HashSet<string>(ExpectedPalette, StringComparer.OrdinalIgnoreCase);
        for (int i = 0; i < 200; i++)
        {
            var key = $"C:\\repo\\project-{i}";
            var hex = ColorService.GetHexColor(key);
            Assert.Contains(hex, paletteSet);
        }
    }

    [Fact]
    public void GetHexColor_DistributesAcrossAllPaletteColors_OverLargeSample()
    {
        // Over ~100 distinct keys, FNV-1a should hit every one of the 12 palette
        // entries at least once. If this becomes flaky, widen the sample — don't
        // weaken the assertion, since uneven distribution is a real regression signal.
        var counts = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        foreach (var color in ExpectedPalette) counts[color] = 0;

        for (int i = 0; i < 120; i++)
        {
            var key = $"C:\\repo\\project-{i}";
            counts[ColorService.GetHexColor(key)]++;
        }

        Assert.All(ExpectedPalette, color =>
            Assert.True(counts[color] > 0, $"Palette color {color} was never produced over 120 sample keys."));
    }

    [Fact]
    public void GetHexColor_IsCaseInsensitive()
    {
        // Implementation lower-cases the input via ToLowerInvariant() before hashing,
        // so paths that differ only by case must map to the same palette color.
        var upper = ColorService.GetHexColor(@"C:\Foo\Bar");
        var lower = ColorService.GetHexColor(@"c:\foo\bar");
        Assert.Equal(upper, lower);
    }

    [Fact]
    public void GetHexColor_TrimsTrailingSlashesAndBackslashes()
    {
        // Implementation calls TrimEnd('/', '\\') so all four variants must collapse
        // to the same color.
        var bare = ColorService.GetHexColor(@"C:\projects\app");
        var trailingBackslash = ColorService.GetHexColor(@"C:\projects\app\");
        var trailingForward = ColorService.GetHexColor(@"C:\projects\app/");
        var trailingMixed = ColorService.GetHexColor(@"C:\projects\app/\");

        Assert.Equal(bare, trailingBackslash);
        Assert.Equal(bare, trailingForward);
        Assert.Equal(bare, trailingMixed);
    }

    [Fact]
    public void GetHexColor_DoesNotTrimInternalSlashes()
    {
        // Only trailing slashes are stripped. A path with internal separators must
        // produce a different color than the same path with those separators removed.
        var withSeparators = ColorService.GetHexColor(@"C:\projects\app");
        var collapsed = ColorService.GetHexColor("Cprojectsapp");
        Assert.NotEqual(withSeparators, collapsed);
    }

    [Fact]
    public void GetHexColor_EmptyString_ReturnsDeterministicPaletteColor()
    {
        // Locks current behavior: empty string is a valid input — FNV-1a of "" is the
        // initial offset basis, so this maps to a specific palette slot. Don't tolerate
        // a silent change to this without a test failure.
        var hex = ColorService.GetHexColor(string.Empty);
        Assert.Contains(hex, ExpectedPalette);
        // FNV-1a offset basis 2166136261 % 12 == 1 → Palette[1] == "#FF9E42"
        Assert.Equal("#FF9E42", hex);
    }

    [Fact]
    public void GetHexColor_WhitespaceString_ReturnsValidPaletteColor()
    {
        // TrimEnd only strips '/' and '\\', so whitespace is preserved and hashed.
        // Document this rather than fix it — callers feed real folder paths.
        var hex = ColorService.GetHexColor("   ");
        Assert.Contains(hex, ExpectedPalette);
    }

    [Fact]
    public void GetHexColor_Null_Throws()
    {
        // Implementation does folderPath.ToLowerInvariant() without a null check,
        // which throws NullReferenceException. Lock that contract — callers should
        // never pass null. If a guard is added later, update this test deliberately.
        Assert.Throws<NullReferenceException>(() => ColorService.GetHexColor(null!));
    }

    [Fact]
    public void GetColor_RoundTripsHexValue()
    {
        const string key = @"C:\projects\round-trip";
        var hex = ColorService.GetHexColor(key);
        var color = ColorService.GetColor(key);

        var expected = (System.Windows.Media.Color)
            System.Windows.Media.ColorConverter.ConvertFromString(hex)!;

        Assert.Equal(expected, color);
    }

    [Fact]
    public void GetBrush_ProducesBrushWithMatchingColor()
    {
        const string key = "user@example.com";
        var brush = ColorService.GetBrush(key);
        var color = ColorService.GetColor(key);

        Assert.NotNull(brush);
        Assert.Equal(color, brush.Color);
    }
}

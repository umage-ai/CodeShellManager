using System.Linq;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for <see cref="ShellIntegrationPayload"/> — the WPF-free rules behind OSC 9001.
/// The values arrive from whatever program is printing to the terminal (a remote host,
/// a `cat` of a file, a hook), so every field is treated as untrusted input.
/// </summary>
public class ShellIntegrationPayloadTests
{
    // ---- color ---------------------------------------------------------------------------

    [Theory]
    [InlineData("#abc", "#abc")]
    [InlineData("#a6e3a1", "#a6e3a1")]
    [InlineData("#A6E3A1", "#A6E3A1")]
    [InlineData("#a6e3a180", "#80a6e3a1")]   // #rrggbbaa (integrator) → #aarrggbb (WPF)
    public void TryNormalizeColor_ValidHex_ReturnsWpfForm(string input, string expected)
    {
        Assert.True(ShellIntegrationPayload.TryNormalizeColor(input, out var wpf));
        Assert.Equal(expected, wpf);
    }

    [Theory]
    [InlineData("")]
    [InlineData("red")]
    [InlineData("rgb(1,2,3)")]
    [InlineData("a6e3a1")]        // missing '#'
    [InlineData("#a6e3a")]        // 5 digits
    [InlineData("#a6e3a1f")]      // 7 digits
    [InlineData("#gggggg")]       // not hex
    [InlineData("#a6e3a1;title=x")]
    public void TryNormalizeColor_Invalid_ReturnsFalse(string input)
    {
        Assert.False(ShellIntegrationPayload.TryNormalizeColor(input, out var wpf));
        Assert.Null(wpf);
    }

    // ---- git-dirty -----------------------------------------------------------------------

    [Theory]
    [InlineData("1", true)]
    [InlineData("true", true)]
    [InlineData("TRUE", true)]
    [InlineData("0", false)]
    [InlineData("false", false)]
    [InlineData("", false)]
    [InlineData("yes", false)]    // only 1/true count as dirty
    public void ParseDirty(string input, bool expected)
        => Assert.Equal(expected, ShellIntegrationPayload.ParseDirty(input));

    // ---- title ---------------------------------------------------------------------------

    [Theory]
    [InlineData("my-repo", "my-repo")]
    [InlineData("  padded  ", "padded")]
    [InlineData("tab\there", "tabhere")]                  // control chars stripped
    [InlineData("esc\u001b[31mred", "esc[31mred")]        // ESC stripped, printable remainder kept
    [InlineData("multi\r\nline", "multiline")]
    [InlineData("ünïcödé ⎇ ok", "ünïcödé ⎇ ok")]          // non-ASCII printable is fine
    public void SanitizeTitle_Printable_TrimsAndStripsControls(string input, string expected)
        => Assert.Equal(expected, ShellIntegrationPayload.SanitizeTitle(input));

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("\u0007\u001b")]  // nothing printable left
    public void SanitizeTitle_NothingUsable_ReturnsNull(string input)
        => Assert.Null(ShellIntegrationPayload.SanitizeTitle(input));

    [Fact]
    public void SanitizeTitle_LongTitle_IsCappedAtMaxTitleLength()
    {
        string input = new('x', ShellIntegrationPayload.MaxTitleLength + 50);
        string? result = ShellIntegrationPayload.SanitizeTitle(input);
        Assert.NotNull(result);
        Assert.Equal(ShellIntegrationPayload.MaxTitleLength, result!.Length);
    }

    [Fact]
    public void SanitizeTitle_CapDoesNotSplitSurrogatePair()
    {
        // Fill up to one char short of the cap, then a 2-char emoji straddling the boundary.
        string input = new string('x', ShellIntegrationPayload.MaxTitleLength - 1) + "😀";
        string? result = ShellIntegrationPayload.SanitizeTitle(input);
        Assert.NotNull(result);
        Assert.False(char.IsHighSurrogate(result![^1]), "cap must not leave a dangling high surrogate");
    }

    // ---- git-branch ----------------------------------------------------------------------

    [Theory]
    [InlineData("main", "main")]
    [InlineData(" feat/x ", "feat/x")]
    [InlineData("", null)]
    [InlineData("   ", null)]
    [InlineData("a\u001bb", "ab")]
    public void SanitizeBranch(string input, string? expected)
        => Assert.Equal(expected, ShellIntegrationPayload.SanitizeBranch(input));

    [Fact]
    public void SanitizeBranch_LongBranch_IsCapped()
    {
        // Found in the v0.8.0 pre-release review: titles were capped, branches were not.
        // The value comes from whatever printed to the terminal and is rendered into a
        // sidebar row, so an unbounded branch let any program wedge the UI.
        string huge = new('b', 100_000);

        string? result = ShellIntegrationPayload.SanitizeBranch(huge);

        Assert.NotNull(result);
        Assert.Equal(ShellIntegrationPayload.MaxBranchLength, result!.Length);
    }

    [Fact]
    public void SanitizeBranch_RealBranchNames_AreNeverTruncated()
    {
        // The cap bounds a hostile emitter; it must not police legitimate refs.
        string realistic = "feature/some-quite-long-but-entirely-reasonable-branch-name-v2";

        Assert.Equal(realistic, ShellIntegrationPayload.SanitizeBranch(realistic));
    }

    [Fact]
    public void SanitizeBranch_CapDoesNotSplitASurrogatePair()
    {
        // Same surrogate-safety the title path already had — cutting mid-pair would emit a
        // lone surrogate into the UI string.
        string emoji = string.Concat(Enumerable.Repeat("😀", 500));

        string? result = ShellIntegrationPayload.SanitizeBranch(emoji);

        Assert.NotNull(result);
        Assert.False(char.IsHighSurrogate(result![^1]),
            "a trailing high surrogate means the cap split a character");
    }
}

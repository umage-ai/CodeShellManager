using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// <see cref="AlertDetector.StripAnsi"/> is what the prompt regexes actually see. OSC strings
/// (title changes, hyperlinks, CSM's own OSC 9001) may end in either BEL or ST (<c>ESC \</c>);
/// both forms must be removed, or their payload leaks into prompt matching.
/// </summary>
public class AlertDetectorStripAnsiTests
{
    private static readonly string Esc = ((char)0x1b).ToString();
    private static readonly string Bel = ((char)0x07).ToString();
    private static readonly string St = Esc + "\\";

    [Fact]
    public void StripAnsi_OscTerminatedWithBel_IsRemoved()
        => Assert.Equal("before after",
            AlertDetector.StripAnsi("before " + Esc + "]0;window title" + Bel + "after"));

    [Fact]
    public void StripAnsi_OscTerminatedWithSt_IsRemoved()
        => Assert.Equal("before after",
            AlertDetector.StripAnsi("before " + Esc + "]9001;color=#a6e3a1;title=demo?" + St + "after"));

    [Fact]
    public void StripAnsi_StTerminatedOsc_DoesNotSwallowFollowingOutputUpToLaterBel()
    {
        // With a BEL-only regex the lazy match runs from the first OSC through the real text
        // to the BEL that ends the *second* OSC — deleting "Do you want to proceed?" from what
        // the prompt regex sees.
        string raw = Esc + "]9001;git-branch=main" + St
                   + "Do you want to proceed?"
                   + Esc + "]0;window title" + Bel;
        Assert.Equal("Do you want to proceed?", AlertDetector.StripAnsi(raw));
    }

    [Fact]
    public void StripAnsi_CsiWithPrivateMarker_IsRemoved()
        => Assert.Equal("x", AlertDetector.StripAnsi(Esc + "[?25hx"));
}

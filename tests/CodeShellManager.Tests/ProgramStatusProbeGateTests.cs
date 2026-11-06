using System;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// <see cref="ProgramStatusProbeGate"/>: an OSC 7501 probe reply is written to the program's
/// stdin, and under the inbox ConPTY it always arrives late (after conhost's own DA1 answer),
/// so it is off by default and, when enabled, gated by win32-input-mode, rate and a cap.
/// </summary>
public class ProgramStatusProbeGateTests
{
    private static readonly DateTime T0 = new(2026, 11, 6, 12, 0, 0, DateTimeKind.Utc);

    private static ProgramStatusProbeGate Win32Gate()
    {
        var g = new ProgramStatusProbeGate();
        g.ObserveOutput("\x1b[?9001h\x1b[?1004h\x1b[?25l");   // conhost's opening bytes
        return g;
    }

    [Fact]
    public void Disabled_setting_never_replies()
    {
        var g = Win32Gate();
        Assert.False(g.TryAcquire(enabled: false, T0));
    }

    [Fact]
    public void No_reply_until_the_ConPTY_asked_for_win32_input_mode()
    {
        var g = new ProgramStatusProbeGate();
        Assert.False(g.TryAcquire(enabled: true, T0));
        g.ObserveOutput("plain output, no mode change");
        Assert.False(g.TryAcquire(enabled: true, T0));
        g.ObserveOutput("\x1b[?9001h");
        Assert.True(g.TryAcquire(enabled: true, T0));
    }

    [Fact]
    public void Win32_input_mode_off_stops_replies_and_last_toggle_in_a_chunk_wins()
    {
        var g = Win32Gate();
        g.ObserveOutput("\x1b[?9001l");
        Assert.False(g.Win32InputMode);
        g.ObserveOutput("\x1b[?9001l...\x1b[?9001h");
        Assert.True(g.Win32InputMode);
        g.ObserveOutput("\x1b[?9001h...\x1b[?9001l");
        Assert.False(g.Win32InputMode);
        Assert.False(g.TryAcquire(enabled: true, T0));
    }

    [Fact]
    public void At_most_one_reply_per_interval()
    {
        var g = Win32Gate();
        Assert.True(g.TryAcquire(true, T0));
        Assert.False(g.TryAcquire(true, T0.AddSeconds(1)));
        Assert.False(g.TryAcquire(true, T0 + ProgramStatusProbeGate.MinInterval - TimeSpan.FromMilliseconds(1)));
        Assert.True(g.TryAcquire(true, T0 + ProgramStatusProbeGate.MinInterval));
    }

    [Fact]
    public void Lifetime_cap_per_session()
    {
        var g = Win32Gate();
        int granted = 0;
        for (int i = 0; i < 20; i++)
            if (g.TryAcquire(true, T0.AddMinutes(i))) granted++;
        Assert.Equal(ProgramStatusProbeGate.MaxRepliesPerSession, granted);
    }
}

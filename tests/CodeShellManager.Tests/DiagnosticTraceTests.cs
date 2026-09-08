using System;
using System.IO;
using CodeShellManager.Diagnostics;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// The tracer for issue #70 exists to measure a latency problem, so the one failure mode
/// that matters is it silently writing nothing — a traced session at the real workload is
/// expensive to arrange, and an empty log is indistinguishable from "the bug didn't happen".
/// </summary>
[Collection("DiagnosticTrace")]
public class DiagnosticTraceTests : IDisposable
{
    private readonly string _path = Path.Combine(
        Path.GetTempPath(), $"csm-trace-{Guid.NewGuid():N}.log");

    public DiagnosticTraceTests() => DiagnosticTrace.ResetForTests(_path);

    public void Dispose()
    {
        try { if (File.Exists(_path)) File.Delete(_path); } catch { }
    }

    [Fact]
    public void Write_then_drain_puts_the_line_in_the_file()
    {
        DiagnosticTrace.Write("DEBUG-tt", "abc12345", "OUTPUT flush len=42 dispatcher-latency=1730ms");

        Assert.Equal(1, DiagnosticTrace.DrainOnce());

        string content = File.ReadAllText(_path);
        Assert.Contains("[DEBUG-tt]", content);
        Assert.Contains("abc12345", content);
        Assert.Contains("dispatcher-latency=1730ms", content);
    }

    [Fact]
    public void Lines_are_written_in_order()
    {
        for (int i = 0; i < 50; i++)
            DiagnosticTrace.Write("DEBUG-tt", "s", $"line={i}");

        DiagnosticTrace.DrainOnce();

        string[] lines = File.ReadAllLines(_path);
        Assert.Equal(50, lines.Length);
        Assert.Contains("line=0", lines[0]);
        Assert.Contains("line=49", lines[49]);
    }

    [Fact]
    public void Nothing_queued_writes_no_file()
    {
        Assert.Equal(0, DiagnosticTrace.DrainOnce());
        Assert.False(File.Exists(_path));
    }

    [Fact]
    public void Overflow_is_reported_in_band_rather_than_leaving_a_silent_gap()
    {
        // A dropped line in a latency log reads as a stall that never happened, so the
        // drop count has to appear in the log itself.
        for (int i = 0; i < 20050; i++)
            DiagnosticTrace.Write("DEBUG-tt", "s", $"line={i}");

        DiagnosticTrace.DrainOnce();

        string content = File.ReadAllText(_path);
        Assert.Contains("TRACE-OVERFLOW dropped=", content);
    }

    [Fact]
    public void Missing_session_id_falls_back_to_the_existing_question_mark_convention()
    {
        // TerminalBridge.Trace has always written "?" for an unset session id; unattributed
        // lines (the heartbeat, overflow) pass "-" explicitly. Both must stay greppable.
        DiagnosticTrace.Write("DEBUG-tt", null, "UI-STALL late=1200ms");
        DiagnosticTrace.Write("DEBUG-tt", "-", "UI-HEARTBEAT window=10s stalls=0 worst=0ms");

        DiagnosticTrace.DrainOnce();

        string content = File.ReadAllText(_path);
        Assert.Contains("[DEBUG-tt] ? UI-STALL late=1200ms", content);
        Assert.Contains("[DEBUG-tt] - UI-HEARTBEAT", content);
    }
}

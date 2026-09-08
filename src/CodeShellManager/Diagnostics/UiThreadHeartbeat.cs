using System;
using System.Windows.Threading;
using CodeShellManager.Models;

namespace CodeShellManager.Diagnostics;

/// <summary>
/// Measures UI-thread responsiveness independently of any terminal session (issue #70).
///
/// A per-bridge <c>dispatcher-latency</c> figure cannot on its own distinguish "the whole UI
/// thread is saturated" from "this one bridge's batch was queued behind a big paint" — and
/// for background sessions it cannot distinguish either of those from ordinary
/// <see cref="DispatcherPriority.Background"/> yielding, which is working as designed.
///
/// This ticks at a fixed interval at <see cref="DispatcherPriority.Normal"/> and records how
/// late each tick actually ran. Overshoot here is UI-thread saturation, full stop, with no
/// session attribution needed. Correlating a typing stall against this timeline says whether
/// the pump was blocked at that moment or whether the delay lives somewhere else entirely.
/// </summary>
public sealed class UiThreadHeartbeat
{
    private const int IntervalMs = 250;

    // Only overshoot beyond this is logged. Timer resolution and ordinary paints produce a
    // steady dribble of a few ms; logging those would bury the events that matter.
    private const int ReportThresholdMs = 100;

    private readonly AppSettings _settings;
    private readonly DispatcherTimer _timer;
    private long _expectedNextMs;
    private long _worstMs;
    private int _overCount;
    private long _lastSummaryMs;

    public UiThreadHeartbeat(AppSettings settings)
    {
        _settings = settings;
        _timer = new DispatcherTimer(DispatcherPriority.Normal)
        {
            Interval = TimeSpan.FromMilliseconds(IntervalMs)
        };
        _timer.Tick += OnTick;
    }

    /// <summary>
    /// Starts ticking only when tracing is on. Call again whenever the setting changes.
    ///
    /// The timer used to run unconditionally and check the flag inside the tick, which put
    /// four Normal-priority dispatcher items per second on the UI thread forever — in the
    /// app whose headline bug (issue #70) was UI-thread saturation. Small, but it is exactly
    /// the kind of always-on cost this diagnostic exists to find.
    /// </summary>
    public void SyncToSettings()
    {
        if (_settings.DebugTerminalTrace == true) Start();
        else Stop();
    }

    public void Start()
    {
        if (_timer.IsEnabled) return;
        _expectedNextMs = Environment.TickCount64 + IntervalMs;
        _lastSummaryMs = Environment.TickCount64;
        _overCount = 0;
        _worstMs = 0;
        _timer.Start();
    }

    public void Stop() => _timer.Stop();

    private void OnTick(object? sender, EventArgs e)
    {
        long now = Environment.TickCount64;
        long late = now - _expectedNextMs;
        _expectedNextMs = now + IntervalMs;

        if (_settings.DebugTerminalTrace != true) return;

        if (late >= ReportThresholdMs)
        {
            _overCount++;
            if (late > _worstMs) _worstMs = late;
            DiagnosticTrace.Write("DEBUG-tt", "-", $"UI-STALL late={late}ms");
        }

        // A periodic summary so a log with no stall lines is still positive evidence that
        // the pump was healthy, rather than ambiguous with "tracing wasn't on".
        if (now - _lastSummaryMs >= 10000)
        {
            DiagnosticTrace.Write("DEBUG-tt", "-",
                $"UI-HEARTBEAT window=10s stalls={_overCount} worst={_worstMs}ms");
            _lastSummaryMs = now;
            _overCount = 0;
            _worstMs = 0;
        }
    }
}

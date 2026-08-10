using System;
using System.Text;

namespace CodeShellManager.Terminal;

/// <summary>
/// Collapses many small PTY output chunks into a single scheduled post (issue #70).
///
/// Without this, every PTY chunk from every session became its own dispatcher work item.
/// Chatty background sessions (Claude spinners, status-line repaints) flooded the shared
/// UI queue, and both the foreground terminal's rendering *and* its keystrokes — which
/// arrive on the same UI thread via WebView2's WebMessageReceived — had to wait behind
/// that backlog. See issue #70 for the full trace.
///
/// The scheduler and emitter are injected so the state machine is testable with no WPF
/// dispatcher and no WebView2 present.
/// </summary>
public sealed class OutputCoalescer
{
    private readonly object _lock = new();
    private readonly StringBuilder _buffer = new();
    private readonly Action<Action> _schedule;
    private readonly Action<string> _emit;

    // True between scheduling a flush and that flush draining the buffer. While set,
    // further appends piggyback on the already-queued flush instead of adding to the queue.
    private bool _flushPending;

    /// <param name="schedule">Marshals the flush onto the UI thread (Dispatcher.BeginInvoke in production).</param>
    /// <param name="emit">Delivers one coalesced payload (PostWebMessageAsString in production).</param>
    public OutputCoalescer(Action<Action> schedule, Action<string> emit)
    {
        _schedule = schedule;
        _emit = emit;
    }

    /// <summary>Buffers a PTY chunk. Safe to call from any thread. Never blocks on the UI thread.</summary>
    public void Append(string data)
    {
        if (string.IsNullOrEmpty(data)) return;

        bool scheduleNow = false;
        lock (_lock)
        {
            _buffer.Append(data);
            if (!_flushPending)
            {
                _flushPending = true;
                scheduleNow = true;
            }
        }

        // Outside the lock: the scheduler may run the flush inline on this thread.
        if (scheduleNow) _schedule(Flush);
    }

    private void Flush()
    {
        string payload;
        lock (_lock)
        {
            payload = _buffer.ToString();
            _buffer.Clear();
            // Cleared *before* emitting so a chunk arriving during the emit below
            // schedules a fresh flush rather than being silently dropped.
            _flushPending = false;
        }

        // Emit outside the lock — it re-enters WebView2 and must not hold up Append.
        if (payload.Length > 0) _emit(payload);
    }
}

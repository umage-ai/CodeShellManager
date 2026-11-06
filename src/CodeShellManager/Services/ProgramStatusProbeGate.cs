using System;

namespace CodeShellManager.Services;

/// <summary>
/// Decides whether one session may answer an OSC 7501 feature query. WPF-free and
/// thread-safe: <see cref="ObserveOutput"/> runs on the PTY read thread,
/// <see cref="TryAcquire"/> on the UI thread. Tested in <c>ProgramStatusProbeGateTests</c>.
/// <para>
/// Every reply is written to the program's stdin, and under the inbox ConPTY it always
/// arrives after conhost's own DA1 answer — i.e. as late, unsolicited input starting with
/// ESC, which a TUI may read as an Escape keypress. So it is gated four ways: the setting
/// must be on; this session's ConPTY must have asked for win32-input-mode
/// (<c>ESC [ ? 9001 h</c>, the only encoding conhost does not drop); at most one reply per
/// <see cref="MinInterval"/>; and at most <see cref="MaxRepliesPerSession"/> ever.
/// </para>
/// </summary>
public sealed class ProgramStatusProbeGate
{
    public static readonly TimeSpan MinInterval = TimeSpan.FromSeconds(5);
    public const int MaxRepliesPerSession = 3;

    private const string Win32InputOn = "\x1b[?9001h";
    private const string Win32InputOff = "\x1b[?9001l";

    private readonly object _lock = new();
    private bool _win32InputMode;
    private int _replies;
    private DateTime _lastReplyUtc = DateTime.MinValue;

    public bool Win32InputMode { get { lock (_lock) return _win32InputMode; } }

    /// <summary>
    /// Tracks win32-input-mode from PTY output; the last of <c>?9001h</c>/<c>?9001l</c> in a
    /// chunk wins. Cheap on the hot path: one ordinal IndexOf for the common prefix. A
    /// sequence split across two chunks is missed — conhost emits it as the very first bytes
    /// of a session, in one write.
    /// </summary>
    public void ObserveOutput(string chunk)
    {
        if (chunk.IndexOf("\x1b[?9001", StringComparison.Ordinal) < 0) return;
        int on = chunk.LastIndexOf(Win32InputOn, StringComparison.Ordinal);
        int off = chunk.LastIndexOf(Win32InputOff, StringComparison.Ordinal);
        if (on < 0 && off < 0) return;
        lock (_lock) _win32InputMode = on > off;
    }

    /// <summary>True (and consumes a reply) when a reply may be sent now.</summary>
    public bool TryAcquire(bool enabled, DateTime utcNow)
    {
        if (!enabled) return false;
        lock (_lock)
        {
            if (!_win32InputMode) return false;
            if (_replies >= MaxRepliesPerSession) return false;
            if (utcNow - _lastReplyUtc < MinInterval) return false;
            _replies++;
            _lastReplyUtc = utcNow;
            return true;
        }
    }
}

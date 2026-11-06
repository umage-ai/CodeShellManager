using System;

namespace CodeShellManager.Services;

/// <summary>
/// Decides whether — and with which bytes — one session answers an OSC 7501 feature query.
/// WPF-free and thread-safe: <see cref="ObserveOutput"/> runs on the PTY read thread,
/// <see cref="TryAcquire"/> on the UI thread. Tested in <c>ProgramStatusProbeGateTests</c>.
/// <para>
/// Two ConPTYs, two answers (see CLAUDE.md "Bundled ConPTY" / "Program status"):
/// </para>
/// <list type="bullet">
/// <item><b>Bundled</b> (OpenConsole): OSC written to the input reaches the program intact,
/// and DA1 is forwarded to xterm.js, so the raw reply arrives in order, before the program's
/// DA1 sentinel. Answered by default.</item>
/// <item><b>Inbox</b> (conhost): OSC input is dropped (only win32-input-mode key events get
/// through) and conhost answers DA1 itself, first — so any reply arrives late, as unsolicited
/// input starting with ESC. Off by default; when forced on, only in win32-input-mode
/// (<c>ESC [ ? 9001 h</c> seen on output).</item>
/// </list>
/// <para>
/// Either way at most one reply per <see cref="MinInterval"/> and
/// <see cref="MaxRepliesPerSession"/> in all.
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
    /// Whether answering is enabled for a session: <paramref name="setting"/> is
    /// <c>AppSettings.AnswerProgramStatusProbe</c> (null = automatic → only on the bundled ConPTY).
    /// </summary>
    public static bool IsEnabled(bool? setting, bool bundledConPty) => setting ?? bundledConPty;

    /// <summary>The bytes to write: raw on the bundled ConPTY, key-event encoded on inbox.</summary>
    public static string ReplyFor(bool bundledConPty) => bundledConPty
        ? ProgramStatusPayload.ProbeReply
        : ProgramStatusPayload.ProbeReplyForConPty;

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

    /// <summary>
    /// True (and consumes a reply) when a reply may be sent now.
    /// <paramref name="bundledConPty"/> lifts the win32-input-mode requirement: the raw reply
    /// needs no key-event encoding there.
    /// </summary>
    public bool TryAcquire(bool enabled, DateTime utcNow, bool bundledConPty = false)
    {
        if (!enabled) return false;
        lock (_lock)
        {
            if (!bundledConPty && !_win32InputMode) return false;
            if (_replies >= MaxRepliesPerSession) return false;
            if (utcNow - _lastReplyUtc < MinInterval) return false;
            _replies++;
            _lastReplyUtc = utcNow;
            return true;
        }
    }
}

using System;
using System.Text;

namespace CodeShellManager.Terminal;

/// <summary>
/// Backs <see cref="PseudoTerminal.DataReceived"/> so output read BEFORE anyone subscribes is
/// kept and handed to the first subscriber, in order, instead of being dropped.
/// <para>
/// Why it exists: a session's PTY is started first and attached to its bridge afterwards
/// (<c>MainWindow.LaunchSessionAsync</c> → <c>pty.Start</c> → <c>bridge.AttachPty</c>), and the
/// reader thread starts at the end of <c>Start</c>. The bundled ConPTY's very first bytes are
/// <c>ESC[1t ESC[c ESC[?1004h ESC[?9001h</c>. Losing <c>ESC[c</c> means xterm.js never answers
/// OpenConsole's own DA1 query — and OpenConsole then consumes the NEXT DA1 answer, the one
/// meant for the program (Claude's probe sentinel), as its own. Losing <c>?9001h</c> confuses
/// the probe gate. See "Bundled ConPTY" in CLAUDE.md.
/// </para>
/// <para>
/// Ordering: the backlog is flushed to the first subscriber INSIDE the lock, and
/// <see cref="Deliver"/> takes the same lock to pick a handler, so no later chunk can be
/// delivered before the backlog. Live chunks are invoked OUTSIDE the lock, as before, so a
/// handler that blocks cannot stall a subscriber on another thread. Only the first
/// subscriber gets the backlog; after that the buffer is gone for good (output with no
/// subscriber is then dropped, exactly as an ordinary event would). WPF-free; tested in
/// <c>EarlyOutputBufferTests</c>.
/// </para>
/// </summary>
internal sealed class EarlyOutputBuffer
{
    /// <summary>Cap on what is held for a subscriber that never comes.</summary>
    public const int MaxBufferedChars = 1 << 20;

    private readonly object _lock = new();
    private Action<string>? _handlers;
    private StringBuilder? _pending = new();

    public void Add(Action<string>? handler)
    {
        if (handler == null) return;
        lock (_lock)
        {
            _handlers += handler;
            var backlog = _pending;
            _pending = null;
            if (backlog is { Length: > 0 }) handler(backlog.ToString());
        }
    }

    public void Remove(Action<string>? handler)
    {
        if (handler == null) return;
        lock (_lock) _handlers -= handler;
    }

    /// <summary>Called by the reader thread with each decoded chunk.</summary>
    public void Deliver(string text)
    {
        Action<string>? handlers;
        lock (_lock)
        {
            handlers = _handlers;
            if (handlers == null)
            {
                if (_pending != null && _pending.Length + text.Length <= MaxBufferedChars)
                    _pending.Append(text);
                return;
            }
        }
        handlers(text);
    }
}

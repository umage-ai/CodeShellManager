using System;
using System.Threading;
using System.Threading.Tasks;

namespace CodeShellManager.Terminal;

/// <summary>
/// Turns a burst of signals from any thread into at most one action per interval.
///
/// Run-command output used to repaint its UI once per 4KB PTY chunk, each time through a
/// synchronous <c>Dispatcher.Invoke</c> — a chatty dev server queued UI work as fast as it
/// printed, and its reader thread stalled until each repaint finished. Signals that arrive
/// while a run is already pending piggyback on it; the pending flag is cleared just before
/// the action runs, so output landing *during* the action schedules exactly one more.
///
/// The scheduler is injected so the state machine is testable with no WPF dispatcher.
/// </summary>
public sealed class ThrottledSignal
{
    private readonly TimeSpan _delay;
    private readonly Action<Action> _post;
    private readonly Action _action;
    private int _pending;

    /// <param name="delay">How long to wait after the first signal of a burst.</param>
    /// <param name="post">Marshals the action onto its thread (Dispatcher.BeginInvoke in production).</param>
    /// <param name="action">The work to run once per burst.</param>
    public ThrottledSignal(TimeSpan delay, Action<Action> post, Action action)
    {
        _delay = delay;
        _post = post;
        _action = action;
    }

    /// <summary>Safe to call from any thread. Never blocks.</summary>
    public void Signal()
    {
        if (Interlocked.Exchange(ref _pending, 1) == 1) return;
        _ = Task.Delay(_delay).ContinueWith(_ => _post(Run), TaskScheduler.Default);
    }

    private void Run()
    {
        Volatile.Write(ref _pending, 0);
        _action();
    }
}

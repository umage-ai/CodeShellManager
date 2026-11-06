using System;
using System.Threading;
using System.Threading.Tasks;
using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Run-command output used to repaint its drawer once per 4KB PTY chunk through a
/// synchronous Dispatcher.Invoke. <see cref="ThrottledSignal"/> collapses a burst into one
/// action, and must never lose the last chunk of a burst.
/// </summary>
public class ThrottledSignalTests
{
    private static readonly TimeSpan Delay = TimeSpan.FromMilliseconds(50);

    [Fact]
    public async Task A_burst_of_signals_runs_the_action_once()
    {
        int runs = 0;
        var signal = new ThrottledSignal(Delay, a => a(), () => Interlocked.Increment(ref runs));

        for (int i = 0; i < 1000; i++) signal.Signal();
        await Task.Delay(500);

        Assert.Equal(1, runs);
    }

    [Fact]
    public async Task A_signal_after_the_action_ran_schedules_another()
    {
        int runs = 0;
        var signal = new ThrottledSignal(Delay, a => a(), () => Interlocked.Increment(ref runs));

        signal.Signal();
        await Task.Delay(500);
        signal.Signal();
        await Task.Delay(500);

        Assert.Equal(2, runs);
    }

    [Fact]
    public async Task A_signal_during_the_action_is_not_lost()
    {
        // Output that lands while the drawer is being repainted must get its own repaint,
        // or the tail of a run's output would stay invisible until the next chunk.
        int runs = 0;
        ThrottledSignal? signal = null;
        signal = new ThrottledSignal(Delay, a => a(), () =>
        {
            if (Interlocked.Increment(ref runs) == 1) signal!.Signal();
        });

        signal.Signal();
        await Task.Delay(500);

        Assert.Equal(2, runs);
    }

    [Fact]
    public void Signal_never_runs_the_action_inline()
    {
        int runs = 0;
        var signal = new ThrottledSignal(Delay, a => a(), () => runs++);

        signal.Signal();

        Assert.Equal(0, runs);
    }
}

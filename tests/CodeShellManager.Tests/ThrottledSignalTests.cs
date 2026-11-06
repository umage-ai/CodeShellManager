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

    // Wait for an EVENT, never for a fixed amount of wall clock. These used to sleep 500ms and
    // then assert, which failed on a loaded CI runner (main, after #160 merged: "Expected: 1,
    // Actual: 0") — xunit runs test classes in parallel, and a 50ms Task.Delay continuation on
    // a starved thread pool can land well past 500ms. The deadline below is only a ceiling for
    // a genuinely broken implementation; a healthy one returns in ~50ms.
    private static readonly TimeSpan Deadline = TimeSpan.FromSeconds(10);

    // How long to keep watching after the expected count is reached, to catch an EXTRA run.
    // Several times Delay, so a wrongly scheduled second run would have fired by then.
    private static readonly TimeSpan Settle = TimeSpan.FromMilliseconds(300);

    private static async Task WaitForRuns(Func<int> runs, int atLeast)
    {
        var until = DateTime.UtcNow + Deadline;
        while (runs() < atLeast && DateTime.UtcNow < until)
            await Task.Delay(10);
        Assert.True(runs() >= atLeast, $"expected at least {atLeast} run(s) within {Deadline.TotalSeconds}s, saw {runs()}");
    }

    [Fact]
    public async Task A_burst_of_signals_runs_the_action_once()
    {
        int runs = 0;
        var signal = new ThrottledSignal(Delay, a => a(), () => Interlocked.Increment(ref runs));

        for (int i = 0; i < 1000; i++) signal.Signal();
        await WaitForRuns(() => Volatile.Read(ref runs), 1);
        await Task.Delay(Settle);

        Assert.Equal(1, Volatile.Read(ref runs));
    }

    [Fact]
    public async Task A_signal_after_the_action_ran_schedules_another()
    {
        int runs = 0;
        var signal = new ThrottledSignal(Delay, a => a(), () => Interlocked.Increment(ref runs));

        signal.Signal();
        // Must wait for the first run itself: signalling again while it is still pending
        // piggybacks on it, which is the burst case above, not this one.
        await WaitForRuns(() => Volatile.Read(ref runs), 1);
        signal.Signal();
        await WaitForRuns(() => Volatile.Read(ref runs), 2);
        await Task.Delay(Settle);

        Assert.Equal(2, Volatile.Read(ref runs));
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
        await WaitForRuns(() => Volatile.Read(ref runs), 2);
        await Task.Delay(Settle);

        Assert.Equal(2, Volatile.Read(ref runs));
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

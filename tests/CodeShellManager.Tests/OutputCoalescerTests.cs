using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Covers the coalescing state machine that collapses N PTY output chunks into a single
/// dispatcher post (issue #70). The scheduler and emitter are injected so these run
/// headlessly with no WPF dispatcher and no WebView2.
/// </summary>
public class OutputCoalescerTests
{
    /// <summary>Captures scheduled flushes so the test controls when they run.</summary>
    private sealed class ManualScheduler
    {
        private readonly List<Action> _queued = new();
        public int ScheduleCount { get; private set; }
        public List<string> Emitted { get; } = new();

        public void Schedule(Action flush)
        {
            ScheduleCount++;
            _queued.Add(flush);
        }

        public void Emit(string payload) => Emitted.Add(payload);

        /// <summary>Runs every flush queued so far (mimics the dispatcher draining).</summary>
        public void Drain()
        {
            var batch = _queued.ToList();
            _queued.Clear();
            foreach (var f in batch) f();
        }
    }

    private static (OutputCoalescer, ManualScheduler) Build()
    {
        var s = new ManualScheduler();
        return (new OutputCoalescer(s.Schedule, s.Emit), s);
    }

    [Fact]
    public void SingleAppend_SchedulesOneFlush_AndEmitsThatData()
    {
        var (c, s) = Build();

        c.Append("hello");

        Assert.Equal(1, s.ScheduleCount);
        Assert.Empty(s.Emitted); // nothing emitted until the dispatcher runs the flush
        s.Drain();
        Assert.Equal(new[] { "hello" }, s.Emitted);
    }

    [Fact]
    public void ManyAppendsBeforeFlush_ScheduleOnlyOneFlush()
    {
        var (c, s) = Build();

        for (int i = 0; i < 50; i++) c.Append("x");

        Assert.Equal(1, s.ScheduleCount);
    }

    [Fact]
    public void ManyAppendsBeforeFlush_EmitOnceWithConcatenatedDataInOrder()
    {
        var (c, s) = Build();

        c.Append("a");
        c.Append("b");
        c.Append("c");
        s.Drain();

        Assert.Equal(new[] { "abc" }, s.Emitted);
    }

    [Fact]
    public void AppendAfterFlush_SchedulesANewFlush()
    {
        var (c, s) = Build();

        c.Append("first");
        s.Drain();
        c.Append("second");
        s.Drain();

        Assert.Equal(2, s.ScheduleCount);
        Assert.Equal(new[] { "first", "second" }, s.Emitted);
    }

    [Fact]
    public void FlushWithNothingBuffered_DoesNotEmit()
    {
        var (c, s) = Build();

        c.Append("only");
        s.Drain();      // drains "only"
        s.Drain();      // no-op: queue is empty, nothing new buffered

        Assert.Equal(new[] { "only" }, s.Emitted);
    }

    [Fact]
    public void DataAppendedDuringFlush_IsNotLost()
    {
        var s = new ManualScheduler();
        OutputCoalescer? c = null;
        bool reentered = false;
        // Emit re-enters Append, simulating a PTY chunk landing while the flush runs.
        c = new OutputCoalescer(s.Schedule, payload =>
        {
            s.Emit(payload);
            if (!reentered)
            {
                reentered = true;
                c!.Append("late");
            }
        });

        c.Append("early");
        s.Drain();   // emits "early", during which "late" is appended
        s.Drain();   // must flush "late"

        Assert.Equal(new[] { "early", "late" }, s.Emitted);
    }

    [Fact]
    public void ConcurrentAppends_LoseNoData()
    {
        var (c, s) = Build();
        const int threads = 8, perThread = 500;

        Parallel.For(0, threads, _ =>
        {
            for (int i = 0; i < perThread; i++) c.Append("z");
        });
        // Drain repeatedly: appends racing with a flush may leave a second flush pending.
        for (int i = 0; i < 5; i++) s.Drain();

        Assert.Equal(threads * perThread, string.Concat(s.Emitted).Length);
    }
}

using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// <see cref="EarlyOutputBuffer"/> backs <c>PseudoTerminal.DataReceived</c>. A session's PTY is
/// started before its bridge subscribes, and the bundled ConPTY's first bytes
/// (<c>ESC[1t ESC[c ESC[?1004h ESC[?9001h</c>) must reach xterm.js — losing <c>ESC[c</c> makes
/// OpenConsole swallow the program's DA1 answer later.
/// </summary>
public class EarlyOutputBufferTests
{
    private const string Opening = "\u001b[1t\u001b[c\u001b[?1004h\u001b[?9001h";

    [Fact]
    public void Output_before_the_first_subscriber_is_delivered_to_it_in_order()
    {
        var buf = new EarlyOutputBuffer();
        buf.Deliver(Opening);
        buf.Deliver("prompt> ");

        var got = new List<string>();
        buf.Add(got.Add);
        buf.Deliver("after");

        Assert.Equal(new[] { Opening + "prompt> ", "after" }, got);
    }

    [Fact]
    public void Nothing_is_replayed_to_a_later_subscriber()
    {
        var buf = new EarlyOutputBuffer();
        buf.Deliver("early");
        var first = new List<string>();
        var second = new List<string>();
        buf.Add(first.Add);
        buf.Add(second.Add);
        buf.Deliver("live");

        Assert.Equal(new[] { "early", "live" }, first);
        Assert.Equal(new[] { "live" }, second);
    }

    [Fact]
    public void A_subscriber_attached_after_unsubscribe_gets_no_stale_backlog()
    {
        var buf = new EarlyOutputBuffer();
        var a = new List<string>();
        buf.Add(a.Add);
        buf.Remove(a.Add);
        buf.Deliver("unheard");                          // no subscriber: dropped, as an event would
        var b = new List<string>();
        buf.Add(b.Add);
        Assert.Empty(b);
    }

    [Fact]
    public void The_backlog_is_capped()
    {
        var buf = new EarlyOutputBuffer();
        string big = new string('x', EarlyOutputBuffer.MaxBufferedChars);
        buf.Deliver(big);
        buf.Deliver("overflow");                          // would exceed the cap: dropped
        var got = new List<string>();
        buf.Add(got.Add);
        Assert.Single(got);
        Assert.Equal(EarlyOutputBuffer.MaxBufferedChars, got[0].Length);
    }

    [Fact]
    public void Concurrent_reader_and_subscribe_never_reorder_or_lose_chunks()
    {
        // The reader thread delivers numbered chunks while another thread subscribes mid-way.
        // Whatever the interleaving, the subscriber must see 0..N-1 exactly once, in order.
        for (int round = 0; round < 50; round++)
        {
            var buf = new EarlyOutputBuffer();
            const int n = 2000;
            var sb = new System.Text.StringBuilder();
            var started = new ManualResetEventSlim();
            var reader = Task.Run(() =>
            {
                for (int i = 0; i < n; i++)
                {
                    if (i == 10) started.Set();
                    buf.Deliver(i + ";");
                }
            });
            started.Wait();
            buf.Add(s => { lock (sb) sb.Append(s); });
            reader.Wait();

            var parts = sb.ToString().TrimEnd(';').Split(';');
            Assert.Equal(n, parts.Length);
            for (int i = 0; i < n; i++) Assert.Equal(i.ToString(), parts[i]);
        }
    }
}

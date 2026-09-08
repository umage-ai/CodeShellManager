using System;
using System.Collections.Concurrent;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace CodeShellManager.Diagnostics;

/// <summary>
/// Buffered, non-blocking writer for <c>[DEBUG-tt]</c> diagnostic lines (issue #70).
///
/// The original tracer called <see cref="Directory.CreateDirectory"/> plus a synchronous
/// <see cref="File.AppendAllText"/> on every single trace call — on the PTY read thread for
/// output, and on the UI thread for the flush. That is fine for a two-session repro and
/// actively harmful at the ~25-session workload this issue is about: tracing the stall would
/// have added a file open/append/close to the very thread whose latency is being measured,
/// and the run would have measured the instrument.
///
/// Callers enqueue a preformatted line and return immediately. A single background drain
/// writes batches to disk. Timestamps are taken at <see cref="Write"/> time, not at drain
/// time, so deferring the I/O does not distort the timings being recorded.
/// </summary>
public static class DiagnosticTrace
{
    // Bounded so a runaway session can't turn a diagnostic into an OOM. Dropped lines are
    // counted and reported in-band, because a silent gap in a latency log is worse than
    // no log at all — it reads as a stall that never happened.
    private const int MaxQueued = 20000;
    private const int DrainIntervalMs = 250;

    private static readonly ConcurrentQueue<string> Queue = new();
    private static int _queued;
    private static int _dropped;
    private static int _started;
    private static string? _path;

    /// <summary>
    /// Mirrors AppSettings.DebugTerminalTrace for code that has no access to settings —
    /// notably <c>GitService</c>, which is deliberately WPF-free and cannot reach the VM.
    /// </summary>
    public static bool Enabled;

    /// <summary>
    /// Managed id of the WPF UI thread, stamped at startup. Lets a WPF-free service report
    /// whether it is running on the UI thread without referencing a Dispatcher.
    /// </summary>
    public static int UiThreadId;

    /// <summary>True when the caller is executing on the UI thread.</summary>
    public static bool OnUiThread => Environment.CurrentManagedThreadId == UiThreadId;

    /// <summary>Absolute path of the log being written. Resolved once, on first use.</summary>
    public static string Path => _path ??= System.IO.Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "CodeShellManager", "crash.log");

    /// <summary>
    /// Points the writer at a scratch file and empties any queued state. Tests only —
    /// the drain loop is a process-lifetime singleton, so tests drive <see cref="DrainOnce"/>
    /// directly instead of racing it.
    /// </summary>
    internal static void ResetForTests(string path)
    {
        _path = path;
        _started = 1; // suppress the background loop; tests pump DrainOnce themselves
        while (Queue.TryDequeue(out _)) { }
        Volatile.Write(ref _queued, 0);
        Volatile.Write(ref _dropped, 0);
    }

    /// <summary>
    /// Queues one line. Safe from any thread, never touches the disk on the caller's thread.
    /// The caller is expected to have already checked its trace flag.
    /// </summary>
    public static void Write(string tag, string? sessionId, string message)
    {
        if (Volatile.Read(ref _queued) >= MaxQueued)
        {
            Interlocked.Increment(ref _dropped);
            return;
        }

        Interlocked.Increment(ref _queued);
        Queue.Enqueue($"[{DateTime.Now:HH:mm:ss.fff}] [{tag}] {sessionId ?? "?"} {message}");
        EnsureDrainStarted();
    }

    private static void EnsureDrainStarted()
    {
        if (Interlocked.CompareExchange(ref _started, 1, 0) != 0) return;

        try { Directory.CreateDirectory(System.IO.Path.GetDirectoryName(Path)!); }
        catch { }

        // Long-running, so it gets its own thread rather than starving a pool thread that
        // the PTY read path also wants.
        _ = Task.Factory.StartNew(DrainLoop, TaskCreationOptions.LongRunning);
    }

    private static void DrainLoop()
    {
        while (true)
        {
            Thread.Sleep(DrainIntervalMs);
            DrainOnce();
        }
    }

    /// <summary>Writes everything queued so far as one append. Returns the line count.</summary>
    internal static int DrainOnce()
    {
        var sb = new StringBuilder();
        int lines = 0;

        while (Queue.TryDequeue(out string? line))
        {
            Interlocked.Decrement(ref _queued);
            sb.Append(line).Append('\n');
            lines++;
        }

        int dropped = Interlocked.Exchange(ref _dropped, 0);
        if (dropped > 0)
        {
            sb.Append($"[{DateTime.Now:HH:mm:ss.fff}] [DEBUG-tt] - " +
                      $"TRACE-OVERFLOW dropped={dropped} lines\n");
            lines++;
        }

        if (sb.Length == 0) return 0;

        try { File.AppendAllText(Path, sb.ToString()); }
        catch { }
        return lines;
    }
}

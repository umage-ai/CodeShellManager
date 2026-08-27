using System;
using System.Threading;
using System.Threading.Tasks;

namespace CodeShellManager.Services;

/// <summary>
/// Serialises every use of the shared <c>output.db</c> connection (issue #102).
///
/// One <see cref="Microsoft.Data.Sqlite.SqliteConnection"/> is opened in MainWindow and
/// handed to <see cref="SearchService"/> AND to every <see cref="Terminal.OutputIndexer"/>
/// — one per session, each draining its own channel on a worker thread. That connection is
/// not thread-safe: it tracks live commands in an internal list, and concurrent
/// create/dispose from different threads corrupts it. The visible symptom was an
/// ArgumentOutOfRangeException thrown from inside <c>SqliteCommand.Dispose</c> during
/// session restore, surfacing as an unobserved task exception long after the fact.
///
/// Usage — one line at the top of anything that touches the connection:
/// <code>
///     using var _ = await DbGate.AcquireAsync();
/// </code>
///
/// A single global gate is deliberate. The alternative — a connection per indexer plus WAL
/// — buys real write concurrency, but the writes here are tiny and the indexer already
/// batches through a channel, so there is little to win and considerably more to get wrong.
/// Correctness first; revisit only if profiling says this is a bottleneck.
/// </summary>
internal static class DbGate
{
    private static readonly SemaphoreSlim Gate = new(1, 1);

    /// <summary>
    /// Waits for exclusive use of the shared connection. Dispose the result to release —
    /// <c>using var _ = await DbGate.AcquireAsync();</c> at the top of the method.
    /// </summary>
    internal static async Task<IDisposable> AcquireAsync()
    {
        await Gate.WaitAsync().ConfigureAwait(false);
        return new Releaser();
    }

    private sealed class Releaser : IDisposable
    {
        private int _released;

        // Guarded so a double-dispose can't inflate the semaphore count and let two
        // callers in at once — which would silently reintroduce the exact race.
        public void Dispose()
        {
            if (Interlocked.Exchange(ref _released, 1) == 0) Gate.Release();
        }
    }
}

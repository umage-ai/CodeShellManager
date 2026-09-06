using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using CodeShellManager.Services;
using Microsoft.Data.Sqlite;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Regression cover for issue #102 — one SqliteConnection shared by SearchService and
/// every OutputIndexer, with no synchronisation.
///
/// SqliteConnection tracks live commands in an internal list, so concurrent
/// create/dispose from different threads corrupts it. In the wild that surfaced as an
/// ArgumentOutOfRangeException thrown from inside SqliteCommand.Dispose during session
/// restore — and because the call site discarded the task, it only appeared later as an
/// unobserved task exception, detached from the code that caused it.
///
/// These drive the same shape: many concurrent writers on one connection.
/// </summary>
public class DbConcurrencyTests : IDisposable
{
    private readonly string _dbPath;
    private readonly SqliteConnection _db;
    private readonly SearchService _svc;

    public DbConcurrencyTests()
    {
        _dbPath = Path.Combine(Path.GetTempPath(), $"csm-dbconc-{Guid.NewGuid():N}.db");
        _db = new SqliteConnection($"Data Source={_dbPath}");
        _db.Open();
        SearchService.InitializeSchemaAsync(_db).GetAwaiter().GetResult();
        _svc = new SearchService(_db);
    }

    public void Dispose()
    {
        try { _db.Close(); } catch { }
        try { _db.Dispose(); } catch { }
        try { SqliteConnection.ClearAllPools(); } catch { }
        try { if (File.Exists(_dbPath)) File.Delete(_dbPath); } catch { }
    }

    [Fact]
    public async Task ConcurrentSessionStarts_AllRecorded_NoConnectionCorruption()
    {
        // Mirrors restore: every launching session fires RecordSessionStartAsync at once.
        //
        // Task.Run is load-bearing. Simply starting the tasks in a loop and awaiting them
        // does NOT reproduce the race — the continuations largely run on one thread and
        // the corruption never happens. The real fault comes from OutputIndexer channel
        // workers on separate thread-pool threads, so the test has to force that too.
        const int writers = 60;
        var tasks = Enumerable.Range(0, writers)
            .Select(i => Task.Run(() => _svc.RecordSessionStartAsync($"claude{i % 4}")))
            .ToList();

        await Task.WhenAll(tasks).WaitAsync(TimeSpan.FromSeconds(60));

        var stats = await _svc.GetUsageStatsAsync();
        Assert.Equal(writers, stats.Sum(s => s.Sessions));
    }

    [Fact]
    public async Task MixedReadersAndWriters_DoNotThrow()
    {
        // SearchService reads racing SearchService writes on the same connection — the
        // other half of #102, where a user searches while sessions are starting.
        var work = new List<Task>();
        for (int i = 0; i < 25; i++)
        {
            work.Add(Task.Run(() => _svc.RecordSessionStartAsync("claude")));
            work.Add(Task.Run(() => _svc.SearchAsync("anything")));
            work.Add(Task.Run(() => _svc.SaveNoteAsync($@"C:\proj{i}", $"note {i}")));
            work.Add(Task.Run(() => _svc.GetUsageStatsAsync()));
        }

        // The assertion is that nothing throws — a corrupted connection surfaces as
        // ArgumentOutOfRange or NullReference from deep inside Microsoft.Data.Sqlite.
        await Task.WhenAll(work).WaitAsync(TimeSpan.FromSeconds(60));
    }

    [Fact]
    public async Task NotesSurviveConcurrentWrites()
    {
        // Writes must not just avoid throwing — they must land.
        var folders = Enumerable.Range(0, 30).Select(i => $@"C:\p{i}").ToList();
        await Task.WhenAll(folders.Select(f => Task.Run(() => _svc.SaveNoteAsync(f, $"content-{f}"))))
            .WaitAsync(TimeSpan.FromSeconds(60));

        foreach (var f in folders)
            Assert.Equal($"content-{f}", await _svc.GetNoteAsync(f));
    }

    [Fact]
    public async Task Gate_UnderLoad_NeverAdmitsTwoHoldersAtOnce()
    {
        // The deterministic one. The three SQLite tests above are smoke cover — the
        // corruption they describe is timing-dependent and does NOT reliably reproduce on
        // demand, so they would not have caught #102 on their own. This asserts the
        // invariant the fix actually provides: mutual exclusion. It fails immediately if
        // the gate is widened or a release is duplicated.
        int concurrent = 0, maxSeen = 0;
        var lockObj = new object();

        var workers = Enumerable.Range(0, 40).Select(_ => Task.Run(async () =>
        {
            for (int i = 0; i < 25; i++)
            {
                using var _ = await DbGate.AcquireAsync();
                int now = System.Threading.Interlocked.Increment(ref concurrent);
                lock (lockObj) { if (now > maxSeen) maxSeen = now; }
                await Task.Yield();                       // force a real interleaving point
                System.Threading.Interlocked.Decrement(ref concurrent);
            }
        })).ToList();

        await Task.WhenAll(workers).WaitAsync(TimeSpan.FromSeconds(60));

        Assert.Equal(1, maxSeen);
    }

    [Fact]
    public async Task Gate_DoubleDispose_DoesNotLetTwoCallersIn()
    {
        // A releaser that released twice would inflate the semaphore count and silently
        // reintroduce the race this whole change exists to remove.
        var first = await DbGate.AcquireAsync();
        first.Dispose();
        first.Dispose();

        var second = await DbGate.AcquireAsync();
        var third = DbGate.AcquireAsync();

        Assert.False(third.IsCompleted, "gate let a second caller in while it was held");

        second.Dispose();
        (await third.WaitAsync(TimeSpan.FromSeconds(5))).Dispose();
    }
}

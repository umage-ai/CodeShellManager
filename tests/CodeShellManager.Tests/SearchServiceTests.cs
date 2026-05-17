using System;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using CodeShellManager.Services;
using Microsoft.Data.Sqlite;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for <see cref="SearchService"/>. Each test gets its own temporary SQLite
/// file (<see cref="SearchService"/> takes a <see cref="SqliteConnection"/> directly,
/// so we open a fresh file-backed connection, run <see cref="SearchService.InitializeSchemaAsync"/>,
/// seed rows via the same INSERT statement <c>OutputIndexer</c> uses, and delete the
/// file on dispose). File-backed (not <c>:memory:</c>) so all `_db.CreateCommand()`
/// calls inside the service see the same data — though SearchService holds one
/// connection so :memory: would technically work too.
/// </summary>
public class SearchServiceTests : IDisposable
{
    private readonly string _dbPath;
    private readonly SqliteConnection _db;
    private readonly SearchService _svc;

    public SearchServiceTests()
    {
        _dbPath = Path.Combine(Path.GetTempPath(), $"csm-search-{Guid.NewGuid():N}.db");
        _db = new SqliteConnection($"Data Source={_dbPath}");
        _db.Open();
        SearchService.InitializeSchemaAsync(_db).GetAwaiter().GetResult();
        _svc = new SearchService(_db);
    }

    public void Dispose()
    {
        try { _db.Close(); } catch { }
        try { _db.Dispose(); } catch { }
        // SQLite holds an exclusive lock on Windows until the connection is fully
        // released and pooling is cleared.
        try { SqliteConnection.ClearAllPools(); } catch { }
        try
        {
            if (File.Exists(_dbPath)) File.Delete(_dbPath);
        }
        catch { /* best-effort cleanup */ }
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    private async Task SeedOutputAsync(string sessionId, string sessionName, string line, long? tsMs = null)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = """
            INSERT INTO session_output (session_id, session_name, ts, line)
            VALUES ($sid, $sname, $ts, $line)
            """;
        cmd.Parameters.AddWithValue("$sid", sessionId);
        cmd.Parameters.AddWithValue("$sname", sessionName);
        cmd.Parameters.AddWithValue("$ts", tsMs ?? DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
        cmd.Parameters.AddWithValue("$line", line);
        await cmd.ExecuteNonQueryAsync();
    }

    // ── SearchAsync — output (FTS5) ──────────────────────────────────────────

    [Fact]
    public async Task SearchAsync_EmptyDatabase_ReturnsNoResults()
    {
        var results = await _svc.SearchAsync("anything");
        Assert.Empty(results);
    }

    [Fact]
    public async Task SearchAsync_BlankQuery_ReturnsNoResults()
    {
        await SeedOutputAsync("s1", "Session 1", "hello world");
        var results = await _svc.SearchAsync("   ");
        Assert.Empty(results);
    }

    [Fact]
    public async Task SearchAsync_UniqueToken_ReturnsOneResultWithSessionIdAndSnippet()
    {
        await SeedOutputAsync("sess-A", "Alpha", "the quick brown fox");
        await SeedOutputAsync("sess-B", "Beta", "lazy dog jumps over");
        await SeedOutputAsync("sess-C", "Gamma", "nothing relevant here");

        var results = await _svc.SearchAsync("quick");

        Assert.Single(results);
        var hit = results[0];
        Assert.Equal("sess-A", hit.SessionId);
        Assert.Equal("Alpha", hit.SessionName);
        Assert.False(string.IsNullOrEmpty(hit.Snippet));
        Assert.Equal(SearchResultType.Output, hit.Type);
        // snippet uses '[' / ']' markers around the matched token
        Assert.Contains("[quick]", hit.Snippet, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public async Task SearchAsync_MultiWordQuery_AppliesImplicitAnd()
    {
        await SeedOutputAsync("s1", "S1", "foo bar baz");
        await SeedOutputAsync("s2", "S2", "foo only");
        await SeedOutputAsync("s3", "S3", "bar only");

        var results = await _svc.SearchAsync("foo bar");

        // FTS5 default is implicit AND — only s1 has both tokens
        Assert.Single(results);
        Assert.Equal("s1", results[0].SessionId);
    }

    [Fact]
    public async Task SearchAsync_NoMatch_ReturnsEmpty()
    {
        await SeedOutputAsync("s1", "S1", "hello world");
        var results = await _svc.SearchAsync("zzznotfoundzzz");
        Assert.Empty(results);
    }

    [Fact]
    public async Task SearchAsync_RespectsLimit()
    {
        for (int i = 0; i < 5; i++)
            await SeedOutputAsync($"s{i}", $"S{i}", $"matchtoken row {i}");

        var results = await _svc.SearchAsync("matchtoken", limit: 3);

        Assert.True(results.Count <= 3, $"expected <= 3 results, got {results.Count}");
        Assert.Equal(3, results.Count);
    }

    [Fact]
    public async Task SearchAsync_SnippetContainsMatchedToken()
    {
        await SeedOutputAsync("s1", "S1", "before middle needle middle after");
        var results = await _svc.SearchAsync("needle");
        Assert.Single(results);
        // snippet wraps the matched token in brackets
        Assert.Contains("needle", results[0].Snippet);
    }

    [Fact]
    public async Task SearchAsync_OrdersByTimestampDescending()
    {
        long t0 = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        await SeedOutputAsync("old", "Old", "needle old", tsMs: t0 - 60_000);
        await SeedOutputAsync("new", "New", "needle new", tsMs: t0);
        await SeedOutputAsync("mid", "Mid", "needle mid", tsMs: t0 - 30_000);

        var results = await _svc.SearchAsync("needle");

        Assert.Equal(3, results.Count);
        Assert.Equal("new", results[0].SessionId);
        Assert.Equal("mid", results[1].SessionId);
        Assert.Equal("old", results[2].SessionId);
    }

    [Fact]
    public async Task SearchAsync_MalformedFtsQuery_DoesNotThrow()
    {
        await SeedOutputAsync("s1", "S1", "anything");
        // unbalanced quote → FTS5 syntax error inside SearchAsync's try/catch
        var results = await _svc.SearchAsync("\"unterminated");
        Assert.NotNull(results);
        // FTS error is swallowed → output search yields nothing; notes search runs too but finds nothing
        Assert.Empty(results);
    }

    // ── SearchAsync — project notes ──────────────────────────────────────────

    [Fact]
    public async Task SearchAsync_MatchesProjectNotes()
    {
        await _svc.SaveNoteAsync(@"C:\repos\myproject", "remember to fix the unique-thing later");

        var results = await _svc.SearchAsync("unique-thing");

        var note = Assert.Single(results, r => r.Type == SearchResultType.Note);
        Assert.Equal("myproject", note.SessionName);
        Assert.Equal(@"C:\repos\myproject", note.FolderPath);
        Assert.Contains("[unique-thing]", note.Snippet);
        Assert.True(note.IsNote);
        Assert.Equal("note", note.TypeLabel);
    }

    // ── Project notes ────────────────────────────────────────────────────────

    [Fact]
    public async Task GetNoteAsync_MissingFolder_ReturnsNull()
    {
        var note = await _svc.GetNoteAsync(@"C:\does\not\exist");
        Assert.Null(note);
    }

    [Fact]
    public async Task SaveNoteAsync_ThenGet_RoundTrips()
    {
        await _svc.SaveNoteAsync(@"C:\repos\foo", "hello note");
        var note = await _svc.GetNoteAsync(@"C:\repos\foo");
        Assert.Equal("hello note", note);
    }

    [Fact]
    public async Task SaveNoteAsync_Upserts()
    {
        await _svc.SaveNoteAsync(@"C:\repos\foo", "first");
        await _svc.SaveNoteAsync(@"C:\repos\foo", "second");
        var note = await _svc.GetNoteAsync(@"C:\repos\foo");
        Assert.Equal("second", note);
    }

    // ── Session history ─────────────────────────────────────────────────────

    [Fact]
    public async Task GetSessionHistoryAsync_MissingId_ReturnsNull()
    {
        var entry = await _svc.GetSessionHistoryAsync("nope");
        Assert.Null(entry);
    }

    [Fact]
    public async Task RecordSessionHistoryAsync_ThenGet_RoundTrips()
    {
        await _svc.RecordSessionHistoryAsync(
            sessionId: "sid-1",
            sessionName: "My Session",
            workingFolder: @"C:\repos\proj",
            command: "claude",
            args: "--resume xyz",
            groupId: "grp-1");

        var entry = await _svc.GetSessionHistoryAsync("sid-1");

        Assert.NotNull(entry);
        Assert.Equal("sid-1", entry!.SessionId);
        Assert.Equal("My Session", entry.SessionName);
        Assert.Equal(@"C:\repos\proj", entry.WorkingFolder);
        Assert.Equal("claude", entry.Command);
        Assert.Equal("--resume xyz", entry.Args);
        Assert.Equal("grp-1", entry.GroupId);
    }

    [Fact]
    public async Task GetLatestSessionHistoryForFolderAsync_ReturnsMostRecent()
    {
        // Seed timestamps directly rather than relying on Task.Delay between RecordSessionHistoryAsync
        // calls — Windows' default 15.6ms timer resolution can collide millisecond unix timestamps
        // on slow CI workers, making "most recent" nondeterministic.
        await SeedSessionHistoryAsync("sid-old", "Old", @"C:\proj", "claude", tsMs: 1000);
        await SeedSessionHistoryAsync("sid-new", "New", @"C:\proj", "claude", tsMs: 2000);

        var entry = await _svc.GetLatestSessionHistoryForFolderAsync(@"C:\proj");

        Assert.NotNull(entry);
        Assert.Equal("sid-new", entry!.SessionId);
    }

    private async Task SeedSessionHistoryAsync(string sessionId, string sessionName, string workingFolder, string command, long tsMs)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = """
            INSERT INTO session_history
                (session_id, session_name, working_folder, command, args, group_id, exited_at)
            VALUES ($sid, $name, $folder, $cmd, '', '', $ts)
            """;
        cmd.Parameters.AddWithValue("$sid", sessionId);
        cmd.Parameters.AddWithValue("$name", sessionName);
        cmd.Parameters.AddWithValue("$folder", workingFolder);
        cmd.Parameters.AddWithValue("$cmd", command);
        cmd.Parameters.AddWithValue("$ts", tsMs);
        await cmd.ExecuteNonQueryAsync();
    }

    [Fact]
    public async Task GetLatestSessionHistoryForFolderAsync_NoMatch_ReturnsNull()
    {
        await _svc.RecordSessionHistoryAsync("sid", "S", @"C:\a", "claude", "", "");
        var entry = await _svc.GetLatestSessionHistoryForFolderAsync(@"C:\different");
        Assert.Null(entry);
    }

    // ── Storage management ──────────────────────────────────────────────────

    [Fact]
    public async Task DeleteSessionLogsAsync_RemovesOnlyMatchingSession()
    {
        await SeedOutputAsync("keep", "Keep", "alpha line");
        await SeedOutputAsync("drop", "Drop", "alpha line");

        await _svc.DeleteSessionLogsAsync("drop");
        var results = await _svc.SearchAsync("alpha");

        Assert.Single(results);
        Assert.Equal("keep", results[0].SessionId);
    }

    [Fact]
    public async Task PruneOldOutputAsync_NonPositiveRetention_NoOp()
    {
        await SeedOutputAsync("s1", "S1", "alpha");
        Assert.Equal(0, await _svc.PruneOldOutputAsync(0));
        Assert.Equal(0, await _svc.PruneOldOutputAsync(-5));
        var results = await _svc.SearchAsync("alpha");
        Assert.Single(results);
    }

    [Fact]
    public async Task PruneOldOutputAsync_DeletesRowsOlderThanCutoff()
    {
        long now = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        long oldTs = now - TimeSpan.FromDays(30).Ticks / TimeSpan.TicksPerMillisecond;

        await SeedOutputAsync("recent", "R", "alpha recent", tsMs: now);
        await SeedOutputAsync("old", "O", "alpha old", tsMs: oldTs);

        int deleted = await _svc.PruneOldOutputAsync(retentionDays: 7);

        Assert.Equal(1, deleted);
        var results = await _svc.SearchAsync("alpha");
        Assert.Single(results);
        Assert.Equal("recent", results[0].SessionId);
    }

    [Fact]
    public async Task ClearAllOutputAsync_WipesEverything()
    {
        await SeedOutputAsync("s1", "S1", "alpha");
        await SeedOutputAsync("s2", "S2", "beta");

        await _svc.ClearAllOutputAsync();
        var resultsA = await _svc.SearchAsync("alpha");
        var resultsB = await _svc.SearchAsync("beta");

        Assert.Empty(resultsA);
        Assert.Empty(resultsB);
    }

    [Fact]
    public async Task GetDatabaseSizeBytesAsync_ReturnsPositiveSize()
    {
        await SeedOutputAsync("s1", "S1", "some line");
        long size = await _svc.GetDatabaseSizeBytesAsync();
        Assert.True(size > 0, $"expected size > 0, got {size}");
    }

    // ── Usage stats ─────────────────────────────────────────────────────────

    [Theory]
    [InlineData("claude --continue", "claude")]
    [InlineData("CLAUDE", "claude")]
    [InlineData("pwsh", "pwsh")]
    [InlineData("pwsh -NoLogo", "pwsh")]
    [InlineData("", "(unknown)")]
    [InlineData(null, "(unknown)")]
    [InlineData("   ", "(unknown)")]
    public void NormalizeCommandName_BehavesAsExpected(string? input, string expected)
    {
        Assert.Equal(expected, SearchService.NormalizeCommandName(input));
    }

    [Fact]
    public async Task RecordSessionStartAsync_IncrementsSessionCount()
    {
        await _svc.RecordSessionStartAsync("claude");
        await _svc.RecordSessionStartAsync("claude --resume abc");
        await _svc.RecordSessionStartAsync("pwsh");

        var stats = await _svc.GetUsageStatsAsync();

        var claude = stats.Single(s => s.Command == "claude");
        var pwsh = stats.Single(s => s.Command == "pwsh");
        Assert.Equal(2, claude.Sessions);
        Assert.Equal(1, pwsh.Sessions);
    }

    [Fact]
    public async Task RecordSessionDurationAsync_AccumulatesSeconds()
    {
        await _svc.RecordSessionStartAsync("claude");
        await _svc.RecordSessionDurationAsync("claude", 120);
        await _svc.RecordSessionDurationAsync("claude --resume", 60);

        var stats = await _svc.GetUsageStatsAsync();
        var claude = stats.Single(s => s.Command == "claude");
        Assert.Equal(180, claude.TotalSeconds);
    }

    [Fact]
    public async Task RecordSessionDurationAsync_NonPositive_NoOp()
    {
        await _svc.RecordSessionStartAsync("claude");
        await _svc.RecordSessionDurationAsync("claude", 0);
        await _svc.RecordSessionDurationAsync("claude", -10);

        var stats = await _svc.GetUsageStatsAsync();
        var claude = stats.Single(s => s.Command == "claude");
        Assert.Equal(0, claude.TotalSeconds);
    }

    [Fact]
    public async Task GetUsageStatsAsync_EmptyDb_ReturnsEmptyList()
    {
        var stats = await _svc.GetUsageStatsAsync();
        Assert.Empty(stats);
    }

    [Fact]
    public async Task GetUsageStatsAsync_OrdersBySessionsDescending()
    {
        // claude: 3 starts; pwsh: 1 start; bash: 2 starts
        await _svc.RecordSessionStartAsync("claude");
        await _svc.RecordSessionStartAsync("claude");
        await _svc.RecordSessionStartAsync("claude");
        await _svc.RecordSessionStartAsync("pwsh");
        await _svc.RecordSessionStartAsync("bash");
        await _svc.RecordSessionStartAsync("bash");

        var stats = await _svc.GetUsageStatsAsync();

        Assert.Equal(3, stats.Count);
        Assert.Equal("claude", stats[0].Command);
        Assert.Equal(3, stats[0].Sessions);
        Assert.Equal("bash", stats[1].Command);
        Assert.Equal(2, stats[1].Sessions);
        Assert.Equal("pwsh", stats[2].Command);
        Assert.Equal(1, stats[2].Sessions);
    }
}

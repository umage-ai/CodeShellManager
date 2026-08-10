using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Claude Code stores conversations under CLAUDE_CONFIG_DIR when that env var is set,
/// falling back to ~/.claude. Reading the wrong root yields a session id from a stale
/// store, and `claude --resume &lt;id&gt;` then fails with "No conversation found with
/// session ID". Timestamps are set explicitly rather than via Task.Delay — Windows'
/// ~15.6ms timer granularity makes wall-clock ordering flaky.
/// </summary>
public class ClaudeSessionServiceTests : IDisposable
{
    private readonly string _root;

    public ClaudeSessionServiceTests()
    {
        _root = Path.Combine(Path.GetTempPath(), "csm-claude-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_root);
    }

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    /// <summary>Creates &lt;claudeHome&gt;/projects/&lt;dirName&gt; and returns it.</summary>
    private string MakeProjectDir(string claudeHome, string dirName)
    {
        string p = Path.Combine(claudeHome, "projects", dirName);
        Directory.CreateDirectory(p);
        return p;
    }

    private static void WriteSession(string projectDir, string sessionId, DateTime lastWriteUtc)
    {
        string f = Path.Combine(projectDir, sessionId + ".jsonl");
        File.WriteAllText(f, "{}\n");
        File.SetLastWriteTimeUtc(f, lastWriteUtc);
    }

    // ── ResolveClaudeHome ────────────────────────────────────────────────────

    [Fact]
    public void ResolveClaudeHome_UsesConfigDir_WhenSet()
    {
        string result = ClaudeSessionService.ResolveClaudeHome(
            @"C:\Users\someone\.claude-work", @"C:\Users\someone");

        Assert.Equal(@"C:\Users\someone\.claude-work", result);
    }

    [Fact]
    public void ResolveClaudeHome_FallsBackToDotClaude_WhenConfigDirNull()
    {
        string result = ClaudeSessionService.ResolveClaudeHome(null, @"C:\Users\someone");

        Assert.Equal(Path.Combine(@"C:\Users\someone", ".claude"), result);
    }

    [Fact]
    public void ResolveClaudeHome_FallsBackToDotClaude_WhenConfigDirBlank()
    {
        string result = ClaudeSessionService.ResolveClaudeHome("   ", @"C:\Users\someone");

        Assert.Equal(Path.Combine(@"C:\Users\someone", ".claude"), result);
    }

    // ── GetLastSessionId ─────────────────────────────────────────────────────

    [Fact]
    public void GetLastSessionId_ReturnsNewestSessionByWriteTime()
    {
        string dir = MakeProjectDir(_root, "C--Github-Foo");
        WriteSession(dir, "11111111-1111-1111-1111-111111111111", new DateTime(2026, 6, 10, 0, 0, 0, DateTimeKind.Utc));
        WriteSession(dir, "22222222-2222-2222-2222-222222222222", new DateTime(2026, 8, 10, 0, 0, 0, DateTimeKind.Utc));

        string? id = ClaudeSessionService.GetLastSessionId(@"C:\Github\Foo", _root);

        Assert.Equal("22222222-2222-2222-2222-222222222222", id);
    }

    [Fact]
    public void GetLastSessionId_ReadsFromTheGivenClaudeHome_NotAHardcodedOne()
    {
        // The regression: a stale ~/.claude alongside a live CLAUDE_CONFIG_DIR. Only the
        // session in the home we were handed may be returned.
        string stale = Path.Combine(_root, "stale");
        string live = Path.Combine(_root, "live");
        WriteSession(MakeProjectDir(stale, "C--Github-Foo"), "5ta1e000-0000-0000-0000-000000000000",
            new DateTime(2026, 6, 10, 0, 0, 0, DateTimeKind.Utc));
        WriteSession(MakeProjectDir(live, "C--Github-Foo"), "11ve0000-0000-0000-0000-000000000000",
            new DateTime(2026, 8, 10, 0, 0, 0, DateTimeKind.Utc));

        string? id = ClaudeSessionService.GetLastSessionId(@"C:\Github\Foo", live);

        Assert.Equal("11ve0000-0000-0000-0000-000000000000", id);
    }

    [Fact]
    public void GetLastSessionId_ReturnsNull_WhenProjectDirDoesNotExist()
    {
        string? id = ClaudeSessionService.GetLastSessionId(@"C:\Github\NeverUsed", _root);

        Assert.Null(id);
    }

    [Fact]
    public void GetLastSessionId_ReturnsNull_WhenProjectDirHasNoSessions()
    {
        MakeProjectDir(_root, "C--Github-Empty");

        string? id = ClaudeSessionService.GetLastSessionId(@"C:\Github\Empty", _root);

        Assert.Null(id);
    }

    [Fact]
    public void GetLastSessionId_IgnoresSubdirectoriesAndNonSessionFiles()
    {
        // Real layout has a <session-id>/ directory (subagent transcripts) next to the
        // <session-id>.jsonl, plus a memory/ dir. Neither may be mistaken for a session.
        string dir = MakeProjectDir(_root, "C--Github-Foo");
        WriteSession(dir, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", new DateTime(2026, 6, 1, 0, 0, 0, DateTimeKind.Utc));
        Directory.CreateDirectory(Path.Combine(dir, "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb.jsonl"));
        Directory.CreateDirectory(Path.Combine(dir, "memory"));
        File.WriteAllText(Path.Combine(dir, "notes.txt"), "x");

        string? id = ClaudeSessionService.GetLastSessionId(@"C:\Github\Foo", _root);

        Assert.Equal("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", id);
    }

    [Fact]
    public void GetLastSessionId_MapsDriveAndSeparatorsToProjectDirName()
    {
        string dir = MakeProjectDir(_root, "C--Github-umage-CodeShellManager");
        WriteSession(dir, "cccccccc-cccc-cccc-cccc-cccccccccccc", new DateTime(2026, 8, 1, 0, 0, 0, DateTimeKind.Utc));

        string? id = ClaudeSessionService.GetLastSessionId(@"C:\Github\umage\CodeShellManager", _root);

        Assert.Equal("cccccccc-cccc-cccc-cccc-cccccccccccc", id);
    }
}

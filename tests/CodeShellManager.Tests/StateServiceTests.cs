using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for <see cref="StateService"/> durability (issue #88).
///
/// The state file is rewritten on ~33 different UI actions, so an interrupted write
/// is a realistic way to lose the whole workspace. These cover the atomic swap, the
/// one-generation backup, recovery from a torn primary, and the null-list tolerance
/// that <c>ImportExportService</c> can otherwise trip over.
///
/// Each test points <c>CSM_STATE_PATH</c> at its own temp file and cleans up after.
/// </summary>
public class StateServiceTests : IDisposable
{
    private readonly string _path;
    private readonly StateService _svc = new();

    public StateServiceTests()
    {
        _path = Path.Combine(Path.GetTempPath(), $"csm-state-{Guid.NewGuid():N}.json");
        Environment.SetEnvironmentVariable("CSM_STATE_PATH", _path);
    }

    public void Dispose()
    {
        Environment.SetEnvironmentVariable("CSM_STATE_PATH", null);
        foreach (var p in new[] { _path, _path + ".bak", _path + ".tmp" })
            if (File.Exists(p)) File.Delete(p);
    }

    private static AppState WithSessions(params string[] ids)
    {
        var s = new AppState();
        foreach (var id in ids)
            s.Sessions.Add(new ShellSession { Id = id, WorkingFolder = $@"C:\{id}", Command = "claude" });
        return s;
    }

    // ── atomic write + backup ────────────────────────────────────────────────

    [Fact]
    public async Task Save_FirstWrite_CreatesFileAndNoBackup()
    {
        await _svc.SaveAsync(WithSessions("a"));

        Assert.True(File.Exists(_path));
        Assert.False(File.Exists(_path + ".bak"));   // nothing to back up yet
        Assert.False(File.Exists(_path + ".tmp"));   // scratch file must not linger
    }

    [Fact]
    public async Task Save_SecondWrite_PreviousContentGoesToBackup()
    {
        await _svc.SaveAsync(WithSessions("first"));
        await _svc.SaveAsync(WithSessions("second"));

        Assert.Contains("second", await File.ReadAllTextAsync(_path));
        Assert.True(File.Exists(_path + ".bak"));
        Assert.Contains("first", await File.ReadAllTextAsync(_path + ".bak"));
        Assert.False(File.Exists(_path + ".tmp"));
    }

    [Fact]
    public async Task Save_RoundTripsSessions()
    {
        await _svc.SaveAsync(WithSessions("x", "y", "z"));
        var loaded = await _svc.LoadAsync();

        Assert.Equal(3, loaded.Sessions.Count);
        Assert.Equal(new[] { "x", "y", "z" }, loaded.Sessions.ConvertAll(s => s.Id));
    }

    [Fact]
    public async Task Save_ManyConcurrentSaves_LeaveAValidFileAndNoTempFile()
    {
        // 29 of the ~32 SaveStateAsync call sites are fire-and-forget, so overlapping
        // saves are routine. They share one temp file, so without serialization one
        // save can swap another's half-written content into the live file. This also
        // catches a semaphore that is acquired but never released — that would hang
        // here rather than fail.
        var saves = new List<Task>();
        for (int i = 0; i < 40; i++)
        {
            var s = WithSessions($"s{i}");
            saves.Add(_svc.SaveAsync(s));
        }

        await Task.WhenAll(saves).WaitAsync(TimeSpan.FromSeconds(30));

        Assert.False(File.Exists(_path + ".tmp"));

        // Whichever save landed last, the file must be complete and parseable.
        var loaded = await _svc.LoadAsync();
        Assert.Single(loaded.Sessions);
        Assert.StartsWith("s", loaded.Sessions[0].Id);
    }

    // ── recovery ─────────────────────────────────────────────────────────────

    [Fact]
    public async Task Load_TornPrimary_RecoversFromBackup()
    {
        await _svc.SaveAsync(WithSessions("good"));   // becomes the .bak
        await _svc.SaveAsync(WithSessions("newer"));

        // Simulate a write interrupted partway through.
        await File.WriteAllTextAsync(_path, "{\"Sessions\":[{\"Id\":\"tru");

        var loaded = await _svc.LoadAsync();

        Assert.Single(loaded.Sessions);
        Assert.Equal("good", loaded.Sessions[0].Id);
    }

    [Fact]
    public async Task Load_TornPrimaryAndNoBackup_ReturnsEmptyRatherThanThrowing()
    {
        await File.WriteAllTextAsync(_path, "{\"Sessions\":[{\"Id\":\"tru");

        var loaded = await _svc.LoadAsync();

        Assert.NotNull(loaded);
        Assert.Empty(loaded.Sessions);
    }

    [Fact]
    public async Task Load_MissingFile_ReturnsEmpty()
    {
        var loaded = await _svc.LoadAsync();

        Assert.NotNull(loaded);
        Assert.Empty(loaded.Sessions);
        Assert.Empty(loaded.Groups);
    }

    // ── null tolerance (ImportExportService can hand us anything) ────────────

    [Fact]
    public async Task Load_ExplicitNullLists_AreCoercedToEmpty()
    {
        await File.WriteAllTextAsync(_path,
            """{"Sessions":null,"Groups":null,"RecentlyClosed":null,"GroupLayouts":null,"LastLayout":"Single"}""");

        var loaded = await _svc.LoadAsync();

        // Property initialisers only apply when the key is absent, not when it is
        // explicitly null — so these must be coerced or every caller NREs.
        Assert.NotNull(loaded.Sessions);
        Assert.NotNull(loaded.Groups);
        Assert.NotNull(loaded.RecentlyClosed);
        Assert.NotNull(loaded.GroupLayouts);
    }

    [Fact]
    public async Task Load_ValidJsonWrongShape_ReturnsEmpty()
    {
        await File.WriteAllTextAsync(_path, """["not","an","appstate"]""");

        var loaded = await _svc.LoadAsync();

        Assert.NotNull(loaded);
        Assert.Empty(loaded.Sessions);
    }

    // ── back-compat: a pre-0.6.0 file must still load ────────────────────────

    [Fact]
    public async Task Load_PreV060File_DefaultsNewFieldsSafely()
    {
        await File.WriteAllTextAsync(_path, """
            {"Sessions":[{"Id":"a","WorkingFolder":"C:\\p","Command":"claude",
              "RunCommands":[{"Id":"r","Label":"Run","CommandLine":"npm run dev","IsDefault":true}]}],
             "Groups":[],"LastLayout":"SixColumn"}
            """);

        var loaded = await _svc.LoadAsync();

        Assert.Single(loaded.Sessions);
        Assert.Empty(loaded.RecentlyClosed);                       // key absent pre-0.6.0
        var rc = Assert.Single(loaded.Sessions[0].RunCommands);
        Assert.Equal(RunMode.Process, rc.Mode);                    // historical behaviour
        Assert.Null(rc.PostRunUrl);
    }
}

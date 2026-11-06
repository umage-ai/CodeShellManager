using System;
using System.IO;
using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// The bundled-ConPTY loader: when it is tried, that it loads by full path only, and that
/// every failure falls back to the inbox ConPTY instead of leaving the user without a
/// terminal. See "Bundled ConPTY" in CLAUDE.md.
/// </summary>
public class ConPtyApiTests : IDisposable
{
    private readonly string _dir = Path.Combine(Path.GetTempPath(), "csm-conpty-" + Guid.NewGuid().ToString("N"));

    public ConPtyApiTests() => Directory.CreateDirectory(_dir);

    public void Dispose()
    {
        try { Directory.Delete(_dir, recursive: true); } catch { }
    }

    private static Func<string, bool> Present(params string[] names) =>
        path => Array.Exists(names, n => string.Equals(Path.GetFileName(path), n, StringComparison.OrdinalIgnoreCase));

    [Fact]
    public void Decide_tries_bundled_only_when_enabled_and_both_files_exist()
    {
        var d = ConPtyResolver.Decide(true, @"C:\app", Present("conpty.dll", "OpenConsole.exe"));
        Assert.True(d.TryBundled);
    }

    [Fact]
    public void Decide_honours_the_setting()
    {
        var d = ConPtyResolver.Decide(false, @"C:\app", Present("conpty.dll", "OpenConsole.exe"));
        Assert.False(d.TryBundled);
        Assert.Contains("UseBundledConPty", d.Reason);
    }

    [Theory]
    [InlineData("conpty.dll")]        // OpenConsole.exe missing
    [InlineData("OpenConsole.exe")]   // conpty.dll missing
    public void Decide_needs_both_files(string onlyThis)
    {
        var d = ConPtyResolver.Decide(true, @"C:\app", Present(onlyThis));
        Assert.False(d.TryBundled);
        Assert.Contains("not found", d.Reason);
    }

    [Fact]
    public void Decide_checks_the_given_directory_by_full_path()
    {
        string? asked = null;
        ConPtyResolver.Decide(true, @"C:\app", p => { asked ??= p; return false; });
        Assert.Equal(@"C:\app\conpty.dll", asked);
    }

    [Fact]
    public void TryBindBundled_refuses_a_relative_path()
    {
        // A bare name would go through the DLL search order — the hijack this design avoids.
        Assert.Null(ConPtyApi.TryBindBundled("conpty.dll", out var reason));
        Assert.Contains("relative", reason);
    }

    [Fact]
    public void TryBindBundled_fails_cleanly_on_a_missing_or_corrupt_file()
    {
        Assert.Null(ConPtyApi.TryBindBundled(Path.Combine(_dir, "conpty.dll"), out _));

        string garbage = Path.Combine(_dir, "garbage.dll");
        File.WriteAllText(garbage, "not a PE image");
        Assert.Null(ConPtyApi.TryBindBundled(garbage, out var reason));
        Assert.Contains("could not load", reason);
    }

    [Fact]
    public void SelectApi_falls_back_to_inbox_on_every_failure()
    {
        Assert.Equal(ConPtyKind.Inbox, PseudoTerminal.SelectApi(false, AppContext.BaseDirectory).Kind);
        Assert.Equal(ConPtyKind.Inbox, PseudoTerminal.SelectApi(true, _dir).Kind);   // files missing

        // Both files present but not loadable.
        File.WriteAllText(Path.Combine(_dir, "conpty.dll"), "corrupt");
        File.WriteAllText(Path.Combine(_dir, "OpenConsole.exe"), "corrupt");
        Assert.Equal(ConPtyKind.Inbox, PseudoTerminal.SelectApi(true, _dir).Kind);
    }

    [Fact]
    public void The_build_ships_both_files_beside_the_assembly_and_conpty_binds()
    {
        // The app project copies conpty.dll + OpenConsole.exe as loose files; they flow to
        // this test project's output too. Losing either means silently running on the
        // inbox ConPTY again.
        string dll = Path.Combine(AppContext.BaseDirectory, "conpty.dll");
        Assert.True(File.Exists(dll), "conpty.dll missing from build output");
        Assert.True(File.Exists(Path.Combine(AppContext.BaseDirectory, "OpenConsole.exe")),
            "OpenConsole.exe missing from build output");

        var api = ConPtyApi.TryBindBundled(dll, out var reason);
        Assert.True(api != null, reason);
        Assert.Equal(ConPtyKind.Bundled, api!.Kind);
        Assert.Equal(ConPtyKind.Bundled, PseudoTerminal.SelectApi(true, AppContext.BaseDirectory).Kind);
    }

    [Fact]
    public void Inbox_binds_from_System32()
    {
        Assert.Equal(ConPtyKind.Inbox, ConPtyApi.Inbox.Kind);
    }
}

using System.Linq;
using System.Threading.Tasks;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class WslDiscoveryServiceTests
{
    // Sample copied from `wsl -l -v` on a host with two distros installed. The
    // leading whitespace in front of "NAME" and the spacing are intentional —
    // wsl pads columns with spaces, never tabs.
    private const string SampleOutput =
        "  NAME                   STATE           VERSION\n" +
        "* Ubuntu                 Running         2\n" +
        "  Debian                 Stopped         2\n";

    [Fact]
    public void Parse_TwoDistros_ReturnsBoth()
    {
        var result = WslDiscoveryService.Parse(SampleOutput);
        Assert.Equal(2, result.Count);
    }

    [Fact]
    public void Parse_MarksDefaultDistro()
    {
        var result = WslDiscoveryService.Parse(SampleOutput);
        Assert.Single(result, d => d.IsDefault);
        Assert.Equal("Ubuntu", result[0].Name); // default sorted first
    }

    [Fact]
    public void Parse_ParsesVersionAndState()
    {
        var result = WslDiscoveryService.Parse(SampleOutput);
        var ubuntu = result.Single(d => d.Name == "Ubuntu");
        Assert.Equal(2, ubuntu.Version);
        Assert.Equal("Running", ubuntu.State);
    }

    [Fact]
    public void Parse_EmptyInput_ReturnsEmpty()
    {
        Assert.Empty(WslDiscoveryService.Parse(""));
        Assert.Empty(WslDiscoveryService.Parse("   \n"));
    }

    [Fact]
    public void Parse_HeaderOnly_ReturnsEmpty()
    {
        Assert.Empty(WslDiscoveryService.Parse("  NAME                   STATE           VERSION\n"));
    }

    [Fact]
    public void Parse_NonDefaultThenDefault_OrdersDefaultFirst()
    {
        const string reversed =
            "  NAME       STATE           VERSION\n" +
            "  Debian     Stopped         2\n" +
            "* Ubuntu     Running         2\n";
        var result = WslDiscoveryService.Parse(reversed);
        Assert.Equal("Ubuntu", result[0].Name);
        Assert.True(result[0].IsDefault);
    }

    [Fact]
    public void ToUncPath_HappyPath()
    {
        Assert.Equal(@"\\wsl$\Ubuntu\home\alice\proj",
            WslDiscoveryService.ToUncPath("Ubuntu", "/home/alice/proj"));
    }

    [Fact]
    public void ToUncPath_NoLinuxPath_ReturnsDistroRoot()
    {
        Assert.Equal(@"\\wsl$\Ubuntu",
            WslDiscoveryService.ToUncPath("Ubuntu", ""));
    }

    [Fact]
    public void ToUncPath_NoDistro_ReturnsEmpty()
    {
        Assert.Equal("", WslDiscoveryService.ToUncPath("", "/home/x"));
    }

    [Fact]
    public void Parse_DistroNameWithSpace_ParsesNameCorrectly()
    {
        // `wsl --import "My Distro" ...` produces a row where NAME spans two tokens.
        // Old parser took just the first token; the from-the-end approach takes
        // the trailing two columns as STATE/VERSION and joins the rest as NAME.
        const string raw =
            "  NAME             STATE           VERSION\n" +
            "* My Distro        Running         2\n";
        var result = WslDiscoveryService.Parse(raw);
        Assert.Single(result);
        Assert.Equal("My Distro", result[0].Name);
        Assert.Equal("Running", result[0].State);
        Assert.Equal(2, result[0].Version);
        Assert.True(result[0].IsDefault);
    }

    [Theory]
    [InlineData(@"\\wsl$\Ubuntu\home\alice", "Ubuntu", "/home/alice")]
    [InlineData(@"\\wsl.localhost\Debian\srv\app", "Debian", "/srv/app")]
    [InlineData(@"\\WSL$\Ubuntu\", "Ubuntu", "/")]
    [InlineData(@"//wsl$/Ubuntu/home/alice", "Ubuntu", "/home/alice")]
    [InlineData(@"\\wsl$\Ubuntu", "Ubuntu", "/")]
    [InlineData(@"\\wsl$\", null, "")]
    [InlineData(@"\\wsl$\\home\alice", null, "")]
    [InlineData(@"C:\proj", null, "")]
    [InlineData("", null, "")]
    public void TryParseUncPath_KnownShapes(string path, string? distro, string linux)
    {
        var (d, l) = WslDiscoveryService.TryParseUncPath(path);
        Assert.Equal(distro, d);
        Assert.Equal(linux, l);
    }

    [Fact]
    public void ResyncWslWorkingFolder_MismatchedWorkingFolder_ReDerivesFromDistroAndLinuxFolder()
    {
        // Simulates a hand-edited / stale RecentlyClosed entry: WorkingFolder points
        // somewhere unrelated to WslDistro + WslWorkingFolder (see CLAUDE.md "WSL
        // Sessions" — the UNC mirror invariant that ReopenClosedSessionAsync must uphold).
        var session = new ShellSession
        {
            Kind = SessionKind.Wsl,
            WslDistro = "Ubuntu",
            WslWorkingFolder = "/home/alice/proj",
            WorkingFolder = @"C:\Windows",
        };

        WslDiscoveryService.ResyncWslWorkingFolder(session);

        Assert.Equal(@"\\wsl$\Ubuntu\home\alice\proj", session.WorkingFolder);
    }

    [Fact]
    public void ResyncWslWorkingFolder_NonWslSession_LeavesWorkingFolderAlone()
    {
        var session = new ShellSession
        {
            Kind = SessionKind.Local,
            WorkingFolder = @"C:\src\web",
        };

        WslDiscoveryService.ResyncWslWorkingFolder(session);

        Assert.Equal(@"C:\src\web", session.WorkingFolder);
    }

    // ── Docker Desktop internal distros (Parse filter) ─────────────────────────────

    [Fact]
    public void Parse_FiltersDockerDesktopDistro()
    {
        const string raw =
            "  NAME                   STATE           VERSION\n" +
            "* Ubuntu                 Running         2\n" +
            "  docker-desktop         Running         2\n";
        var result = WslDiscoveryService.Parse(raw);
        Assert.Single(result);
        Assert.Equal("Ubuntu", result[0].Name);
    }

    [Fact]
    public void Parse_FiltersDockerDesktopDataDistro_CaseInsensitive()
    {
        // Older Docker Desktop versions also install "docker-desktop-data"; casing is
        // matched loosely since wsl -l -v's own casing isn't something we control.
        const string raw =
            "  NAME                       STATE           VERSION\n" +
            "  DOCKER-DESKTOP-DATA        Running         2\n";
        var result = WslDiscoveryService.Parse(raw);
        Assert.Empty(result);
    }

    [Fact]
    public void Parse_KeepsDistroNameThatOnlyContainsDockerDesktopPhrase()
    {
        // Exact match only — a user-imported distro that merely contains the phrase
        // must still be offered in the picker.
        const string raw =
            "  NAME                       STATE           VERSION\n" +
            "  my-docker-desktop-clone    Running         2\n";
        var result = WslDiscoveryService.Parse(raw);
        Assert.Single(result);
        Assert.Equal("my-docker-desktop-clone", result[0].Name);
    }

    [Fact]
    public void Parse_OnlyDockerDistros_YieldsEmptyList()
    {
        // So the dialog falls through to its existing "No WSL distros found" hint.
        const string raw =
            "  NAME                       STATE           VERSION\n" +
            "* docker-desktop             Running         2\n" +
            "  docker-desktop-data        Stopped         2\n";
        var result = WslDiscoveryService.Parse(raw);
        Assert.Empty(result);
    }

    // ── GetLoginShellAsync ──────────────────────────────────────────────────────────
    // Only the no-spawn short-circuit is testable without a live WSL distro (CI has
    // none); the probe/cache path itself needs wsl.exe and is verified manually — see
    // the fix report.

    [Fact]
    public async Task GetLoginShellAsync_BlankDistro_ReturnsBashWithoutSpawning()
    {
        Assert.Equal("bash", await WslDiscoveryService.GetLoginShellAsync(""));
        Assert.Equal("bash", await WslDiscoveryService.GetLoginShellAsync("   "));
    }
}

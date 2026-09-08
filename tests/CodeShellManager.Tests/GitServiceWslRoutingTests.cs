using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Headless coverage for the WSL routing helpers in GitService. The live
/// wsl.exe dispatch can't run on every test host; these cover the path/arg
/// translation that has to be exactly right for routing to land in the
/// correct place.
/// </summary>
public class GitServiceWslRoutingTests
{
    [Theory]
    [InlineData(@"\\wsl$\Ubuntu\home\alice", "Ubuntu", "/home/alice")]
    [InlineData(@"\\wsl.localhost\Debian\srv\app", "Debian", "/srv/app")]
    [InlineData(@"\\wsl$\Ubuntu", "Ubuntu", "/")]
    [InlineData(@"C:\proj", null, "")]
    [InlineData("", null, "")]
    public void TryParseWslUnc_KnownShapes(string path, string? expectedDistro, string expectedLinux)
    {
        var (distro, linuxPath) = GitService.TryParseWslUnc(path);
        Assert.Equal(expectedDistro, distro);
        Assert.Equal(expectedLinux, linuxPath);
    }

    [Fact]
    public void TranslateUncArgToLinux_MatchingDistro_Substitutes()
    {
        string translated = GitService.TranslateUncArgToLinux(
            @"\\wsl$\Ubuntu\home\alice\proj-foo", "Ubuntu");
        Assert.Equal("/home/alice/proj-foo", translated);
    }

    [Fact]
    public void TranslateUncArgToLinux_DifferentDistro_LeftAlone()
    {
        // We're running git inside Ubuntu — a UNC pointing at Debian is a real
        // mistake and should NOT be silently rewritten to look like a local path.
        string arg = @"\\wsl$\Debian\home\alice\proj";
        Assert.Equal(arg, GitService.TranslateUncArgToLinux(arg, "Ubuntu"));
    }

    [Fact]
    public void TranslateUncArgToLinux_NonPathArgs_Passthrough()
    {
        Assert.Equal("--show-current", GitService.TranslateUncArgToLinux("--show-current", "Ubuntu"));
        Assert.Equal("branch", GitService.TranslateUncArgToLinux("branch", "Ubuntu"));
    }

    [Fact]
    public void TranslateLinuxPathsToUnc_RevParseOutput()
    {
        string raw = "/home/alice/proj/.git\n";
        string translated = GitService.TranslateLinuxPathsToUnc(raw, "Ubuntu");
        Assert.Contains(@"\\wsl$\Ubuntu\home\alice\proj\.git", translated);
    }

    [Fact]
    public void TranslateLinuxPathsToUnc_WorktreeListPorcelain()
    {
        // Real-ish output: only the `worktree /…` lines carry abs paths; the rest
        // (HEAD sha, refs/heads/x) must NOT be mangled.
        string raw = "worktree /home/alice/proj\nHEAD abc123\nbranch refs/heads/main\n";
        string translated = GitService.TranslateLinuxPathsToUnc(raw, "Ubuntu");
        Assert.Contains(@"worktree \\wsl$\Ubuntu\home\alice\proj", translated);
        Assert.Contains("HEAD abc123", translated);
        Assert.Contains("branch refs/heads/main", translated);
    }

    [Fact]
    public void TranslateLinuxPathsToUnc_BranchNameWithSlash_NotMangled()
    {
        // refs/heads/feature/foo starts with 'r', not '/' — must pass through.
        string raw = "feature/wsl-sessions\n";
        Assert.Equal(raw, GitService.TranslateLinuxPathsToUnc(raw, "Ubuntu"));
    }

    [Fact]
    public void TranslateLinuxPathsToUnc_StatusPorcelain_Untouched()
    {
        // Each "M file" / "?? new" line has no leading slash and shouldn't change.
        string raw = "M README.md\n?? new.txt\n";
        Assert.Equal(raw, GitService.TranslateLinuxPathsToUnc(raw, "Ubuntu"));
    }

    [Fact]
    public void TranslateUncArgToLinux_PathWithSpaces_TranslatedWhole()
    {
        // The old whole-command-line regex needed a separate quoted pass for this; a path
        // with a space used to come back half-translated. Per-argument, spaces are just
        // characters in the argument.
        Assert.Equal("/home/alice/my repo",
            GitService.TranslateUncArgToLinux(@"\\wsl$\Ubuntu\home\alice\my repo", "Ubuntu"));
    }

    [Fact]
    public void TranslateUncArgToLinux_DistroRoot_BecomesSlash()
    {
        Assert.Equal("/", GitService.TranslateUncArgToLinux(@"\\wsl$\Ubuntu", "Ubuntu"));
    }

    [Fact]
    public void TranslateLinuxPathsToUnc_PathContainsSpaces_TranslatesWholePath()
    {
        // Regression: the tail used to stop at the first whitespace, so a worktree
        // path with a space got half-translated.
        string raw = "worktree /home/alice/My Projects/repo\n";
        string translated = GitService.TranslateLinuxPathsToUnc(raw, "Ubuntu");
        Assert.Contains(@"\\wsl$\Ubuntu\home\alice\My Projects\repo", translated);
        Assert.DoesNotContain("Projects/repo", translated); // no leftover forward slashes
    }

    [Fact]
    public void TranslateUncArgToLinux_PrefixCollidingDistro_LeftAlone()
    {
        // `Ubuntu` must not match `Ubuntu-22.04` — the default `wsl --install` naming.
        // Previously enforced by a regex lookahead; now it falls out of TryParseWslUnc
        // returning the real distro name and an ordinal-ignore-case comparison.
        string arg = @"\\wsl$\Ubuntu-22.04\home\alice\x";
        Assert.Equal(arg, GitService.TranslateUncArgToLinux(arg, "Ubuntu"));
    }

    [Fact]
    public void TranslateUncArgToLinux_CaseInsensitiveDistroMatch()
    {
        Assert.Equal("/home/alice",
            GitService.TranslateUncArgToLinux(@"\\wsl$\ubuntu\home\alice", "Ubuntu"));
    }
}

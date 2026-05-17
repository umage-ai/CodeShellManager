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
    public void TranslateUncArgsToLinux_MatchingDistro_Substitutes()
    {
        string args = "worktree add \"\\\\wsl$\\Ubuntu\\home\\alice\\proj-foo\" main";
        string translated = GitService.TranslateUncArgsToLinux(args, "Ubuntu");
        Assert.Contains("/home/alice/proj-foo", translated);
        Assert.DoesNotContain(@"\\wsl$\Ubuntu", translated);
    }

    [Fact]
    public void TranslateUncArgsToLinux_DifferentDistro_LeftAlone()
    {
        // We're running git inside Ubuntu — a UNC pointing at Debian is a real
        // mistake and should NOT be silently rewritten to look like a local path.
        string args = @"worktree add \\wsl$\Debian\home\alice\proj main";
        string translated = GitService.TranslateUncArgsToLinux(args, "Ubuntu");
        Assert.Equal(args, translated);
    }

    [Fact]
    public void TranslateUncArgsToLinux_NoUncs_Passthrough()
    {
        string args = "branch --show-current";
        Assert.Equal(args, GitService.TranslateUncArgsToLinux(args, "Ubuntu"));
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
    public void TranslateUncArgsToLinux_QuotedUncWithSpaces_TranslatedWholeAndReQuoted()
    {
        // Regression: the unquoted regex stops at whitespace, so a quoted UNC
        // containing a space (worktree add target) used to be half-translated.
        string args = "worktree add \"\\\\wsl$\\Ubuntu\\home\\alice\\my repo\" main";
        string translated = GitService.TranslateUncArgsToLinux(args, "Ubuntu");
        Assert.Contains("\"/home/alice/my repo\"", translated);
        Assert.DoesNotContain(@"\\wsl$\Ubuntu", translated);
    }

    [Fact]
    public void TranslateUncArgsToLinux_QuotedUncRoot_BecomesQuotedRoot()
    {
        string args = "rev-parse \"\\\\wsl$\\Ubuntu\"";
        string translated = GitService.TranslateUncArgsToLinux(args, "Ubuntu");
        Assert.Equal("rev-parse \"/\"", translated);
    }
}

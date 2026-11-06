using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// The toolbar git menu: status parsing, the wording it shows, the argv it builds, and the
/// operations themselves against a real throwaway repo with a local "remote".
/// </summary>
public class GitStatusParseTests
{
    // Captured verbatim from git 2.54 (`-c core.quotePath=false status --porcelain=v2 --branch`)
    // on a repo with a modified file, a path with a space, a staged rename, a staged non-ASCII
    // path inside a spaced directory, and an untracked file.
    private const string Captured =
        "# branch.oid d168ec5ccba03426d0799f256ccb5f08c53d24b2\n" +
        "# branch.head master\n" +
        "# branch.upstream origin/master\n" +
        "# branch.ab +1 -3\n" +
        "1 .M N... 100644 100644 100644 78981922613b2afb6025042ff6bd878ac1994e85 78981922613b2afb6025042ff6bd878ac1994e85 a.txt\n" +
        "1 .M N... 100644 100644 100644 61780798228d17af2d34fce4cfbdf35556832472 61780798228d17af2d34fce4cfbdf35556832472 my file.txt\n" +
        "2 R. N... 100644 100644 100644 f2ad6c76f0115a6ba5b00456a849810e7ec0af20 f2ad6c76f0115a6ba5b00456a849810e7ec0af20 R100 new.txt\told.txt\n" +
        "1 A. N... 000000 100644 100644 0000000000000000000000000000000000000000 bca70f35318f31dd1d1d1d2d2e64c19b880899ff sub dir/æø.txt\n" +
        "? untracked.txt\n";

    [Fact]
    public void Parses_branch_upstream_and_ahead_behind()
    {
        var s = GitService.ParseStatusV2(Captured);
        Assert.Equal("master", s.Branch);
        Assert.False(s.IsDetached);
        Assert.Equal("origin/master", s.Upstream);
        Assert.Equal(1, s.Ahead);
        Assert.Equal(3, s.Behind);
    }

    [Fact]
    public void Parses_every_change_with_its_full_path()
    {
        var s = GitService.ParseStatusV2(Captured);
        Assert.True(s.IsDirty);
        Assert.Equal(
            new[] { "a.txt", "my file.txt", "old.txt → new.txt", "sub dir/æø.txt", "untracked.txt" },
            s.Changes.Select(c => c.Path));
        Assert.Equal(new[] { " M", " M", "R ", "A ", "??" }, s.Changes.Select(c => c.Code));
    }

    [Fact]
    public void Tolerates_CRLF()
    {
        var s = GitService.ParseStatusV2(Captured.Replace("\n", "\r\n"));
        Assert.Equal("master", s.Branch);
        Assert.Equal(5, s.Changes.Count);
        Assert.DoesNotContain(s.Changes, c => c.Path.EndsWith('\r'));
    }

    [Fact]
    public void Clean_repo_without_upstream()
    {
        var s = GitService.ParseStatusV2("# branch.oid abc\n# branch.head feature/x\n");
        Assert.Equal("feature/x", s.Branch);
        Assert.Null(s.Upstream);
        Assert.Null(s.Ahead);
        Assert.Null(s.Behind);
        Assert.False(s.IsDirty);
    }

    [Fact]
    public void Upstream_whose_remote_branch_is_gone_has_no_counts()
    {
        // git omits branch.ab in this case — seen in the capture above before the push landed.
        var s = GitService.ParseStatusV2("# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n");
        Assert.Equal("origin/main", s.Upstream);
        Assert.Null(s.Ahead);
    }

    [Fact]
    public void Detached_head_reports_no_branch()
    {
        var s = GitService.ParseStatusV2("# branch.oid abc\n# branch.head (detached)\n");
        Assert.True(s.IsDetached);
        Assert.Null(s.Branch);
    }

    [Fact]
    public void Unmerged_entry_is_listed_as_conflict()
    {
        var s = GitService.ParseStatusV2(
            "# branch.head main\n" +
            "u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflicted file.txt\n");
        var c = Assert.Single(s.Changes);
        Assert.Equal("UU", c.Code);
        Assert.Equal("conflicted file.txt", c.Path);
    }

    [Fact]
    public void Unrecognised_lines_are_ignored_not_fatal()
    {
        var s = GitService.ParseStatusV2("# branch.head main\nwhat is this\n1 short\n# branch.ab garbage\n");
        Assert.Equal("main", s.Branch);
        Assert.Empty(s.Changes);
        Assert.Null(s.Ahead);
    }
}

public class GitStatusTextTests
{
    [Theory]
    [InlineData(null, null, "")]
    [InlineData(0, 0, "")]
    [InlineData(2, 0, "↑2")]
    [InlineData(0, 5, "↓5")]
    [InlineData(2, 5, "↑2 ↓5")]
    public void Sync(int? ahead, int? behind, string expected) =>
        Assert.Equal(expected, GitStatusText.Sync(ahead, behind));

    [Fact]
    public void MenuHeader_variants()
    {
        Assert.Equal("main → origin/main · ↑2 ↓5 · 3 changed",
            GitStatusText.MenuHeader("main", "origin/main", 2, 5, 3));
        Assert.Equal("main → origin/main · up to date · clean",
            GitStatusText.MenuHeader("main", "origin/main", 0, 0, 0));
        Assert.Equal("main → origin/main · upstream gone · 1 changed",
            GitStatusText.MenuHeader("main", "origin/main", null, null, 1));
        Assert.Equal("wip · no upstream · clean",
            GitStatusText.MenuHeader("wip", null, null, null, 0));
        Assert.Equal("detached HEAD · no upstream · clean",
            GitStatusText.MenuHeader(null, null, null, null, 0));
    }

    [Fact]
    public void FetchSummary_variants()
    {
        Assert.Equal("Fetched.", GitStatusText.FetchSummary("main", null, null, null));
        Assert.Equal("main is up to date with origin/main.", GitStatusText.FetchSummary("main", "origin/main", 0, 0));
        Assert.Equal("Nothing new — main is 2 ahead of origin/main.", GitStatusText.FetchSummary("main", "origin/main", 2, 0));
        Assert.Equal("1 new commit on origin/main — Pull to fast-forward.", GitStatusText.FetchSummary("main", "origin/main", 0, 1));
        Assert.Equal("4 new commits on origin/main; main has diverged (2 ahead).", GitStatusText.FetchSummary("main", "origin/main", 2, 4));
    }

    [Theory]
    [InlineData("fatal: Not possible to fast-forward, aborting.", "diverged")]
    [InlineData("error: Your local changes to the following files would be overwritten by merge:", "Commit or stash")]
    [InlineData("fatal: could not read Username for 'https://github.com': terminal prompts disabled", "credentials")]
    [InlineData("git@github.com: Permission denied (publickey).", "SSH")]
    [InlineData("There is no tracking information for the current branch.", "no upstream")]
    [InlineData("fatal: unable to access 'https://x/': Could not resolve host: x", "network")]
    [InlineData("fatal: Unable to create '.git/index.lock': File exists.", "Another git command")]
    public void FailureHint_recognises_common_errors(string output, string expectedFragment) =>
        Assert.Contains(expectedFragment, GitStatusText.FailureHint(output));

    [Fact]
    public void FailureHint_is_null_for_unknown_errors() =>
        Assert.Null(GitStatusText.FailureHint("fatal: something nobody anticipated"));

    [Theory]
    [InlineData("Already up to date.", "Already up to date.")]
    [InlineData("Updating 1a2b..3c4d\nFast-forward\n a.txt | 2 +-\n 3 files changed, 10 insertions(+), 2 deletions(-)",
        "Fast-forwarded: 3 files changed, 10 insertions(+), 2 deletions(-)")]
    [InlineData("Updating 1a2b..3c4d\nFast-forward", "Fast-forwarded.")]
    public void SummarizePull(string output, string expected) =>
        Assert.Equal(expected, GitService.SummarizePull(output));
}

public class GitToolsArgvTests
{
    [Theory]
    [InlineData("-x")]          // a branch literally named like an option
    [InlineData("--force")]
    [InlineData("$(id)")]
    [InlineData("a\"b")]
    [InlineData("feature/with space")]
    public void Switch_keeps_the_branch_one_argument_after_the_separator(string branch)
    {
        string cmd = GitService.BuildLocalGitCommandLine(@"C:\repo", GitService.SwitchArgs(branch));
        string[] argv = Win32CommandLineTests.Split("git " + cmd);
        Assert.Equal(new[] { "git", "-C", @"C:\repo", "switch", "--no-guess", "--", branch }, argv);
    }

    [Fact]
    public void WslEnv_gains_the_git_variables_once_and_keeps_existing_entries()
    {
        Assert.Equal("GIT_TERMINAL_PROMPT:GIT_OPTIONAL_LOCKS",
            GitService.MergeWslEnv(null, new[] { "GIT_TERMINAL_PROMPT", "GIT_OPTIONAL_LOCKS" }));
        Assert.Equal("USERPROFILE/p:GIT_TERMINAL_PROMPT",
            GitService.MergeWslEnv("USERPROFILE/p", new[] { "GIT_TERMINAL_PROMPT" }));
        Assert.Equal("GIT_TERMINAL_PROMPT/u",
            GitService.MergeWslEnv("GIT_TERMINAL_PROMPT/u", new[] { "GIT_TERMINAL_PROMPT" }));
    }

    [Fact]
    public void Every_git_never_prompts_and_reads_skip_optional_locks()
    {
        Assert.Contains(GitService.GitEnvironment(readOnly: false), kv => kv.Key == "GIT_TERMINAL_PROMPT" && kv.Value == "0");
        Assert.DoesNotContain(GitService.GitEnvironment(readOnly: false), kv => kv.Key == "GIT_OPTIONAL_LOCKS");
        Assert.Contains(GitService.GitEnvironment(readOnly: true), kv => kv.Key == "GIT_OPTIONAL_LOCKS" && kv.Value == "0");
    }
}

/// <summary>
/// The operations end to end, against a bare repo standing in for the remote and two clones:
/// "upstream" pushes, "work" is what a session would be sitting in.
/// </summary>
public sealed class GitToolsRepoTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "csm-gittools-" + Guid.NewGuid().ToString("N")[..8]);
    private string Remote => Path.Combine(_root, "remote.git");
    private string Upstream => Path.Combine(_root, "upstream");
    private string Work => Path.Combine(_root, "work");

    public GitToolsRepoTests()
    {
        Directory.CreateDirectory(_root);
        Git(_root, "init", "-q", "--bare", "-b", "main", Remote);
        Git(_root, "clone", "-q", Remote, Upstream);
        Configure(Upstream);
        File.WriteAllText(Path.Combine(Upstream, "a.txt"), "one\n");
        Git(Upstream, "add", "a.txt");
        Git(Upstream, "commit", "-q", "-m", "one");
        Git(Upstream, "push", "-q", "origin", "HEAD:main");
        Git(_root, "clone", "-q", Remote, Work);
        Configure(Work);
    }

    private static void Configure(string repo)
    {
        Git(repo, "config", "user.email", "test@example.invalid");
        Git(repo, "config", "user.name", "test");
        Git(repo, "config", "commit.gpgsign", "false");
        Git(repo, "config", "core.autocrlf", "false");
    }

    private void PushUpstreamCommit(string file, string content)
    {
        File.WriteAllText(Path.Combine(Upstream, file), content);
        Git(Upstream, "add", file);
        Git(Upstream, "commit", "-q", "-m", file);
        Git(Upstream, "push", "-q", "origin", "HEAD:main");
    }

    private static string Git(string cwd, params string[] args)
    {
        var psi = new ProcessStartInfo("git")
        {
            WorkingDirectory = cwd,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        // Pinned so a machine-wide core.autocrlf=true can't make a fresh clone look modified.
        psi.ArgumentList.Add("-c");
        psi.ArgumentList.Add("core.autocrlf=false");
        foreach (var a in args) psi.ArgumentList.Add(a);
        using var p = Process.Start(psi)!;
        string o = p.StandardOutput.ReadToEnd();
        string e = p.StandardError.ReadToEnd();
        p.WaitForExit();
        if (p.ExitCode != 0) throw new InvalidOperationException($"git {string.Join(' ', args)}: {e}");
        return o;
    }

    [Fact]
    public async Task Status_reports_branch_upstream_and_counts()
    {
        File.WriteAllText(Path.Combine(Work, "new.txt"), "x");
        var s = await GitService.GetStatusAsync(Work);
        Assert.NotNull(s);
        Assert.Equal("main", s!.Branch);
        Assert.Equal("origin/main", s.Upstream);
        Assert.Equal(0, s.Ahead);
        Assert.Equal(0, s.Behind);
        Assert.Equal("??", Assert.Single(s.Changes).Code);

        var (branch, dirty) = await GitService.GetGitInfoAsync(Work);
        Assert.Equal("main", branch);
        Assert.True(dirty);
    }

    [Fact]
    public async Task Status_of_a_plain_folder_is_null()
    {
        string plain = Path.Combine(_root, "plain");
        Directory.CreateDirectory(plain);
        // A bare folder under %TEMP% might still sit inside some enclosing repo on a dev box;
        // GIT_CEILING_DIRECTORIES would fix that but isn't ours to set. Assert only when git
        // agrees it's not a repo.
        var s = await GitService.GetStatusAsync(plain);
        if (s is not null) return;
        Assert.Equal((null, false), await GitService.GetGitInfoAsync(plain));
    }

    [Fact]
    public async Task Fetch_then_pull_fast_forwards()
    {
        PushUpstreamCommit("b.txt", "two\n");

        var fetch = await GitService.FetchAsync(Work);
        Assert.True(fetch.Ok, fetch.Output);
        var s = await GitService.GetStatusAsync(Work);
        Assert.Equal(1, s!.Behind);

        var pull = await GitService.PullFastForwardAsync(Work);
        Assert.True(pull.Ok, pull.Output);
        Assert.StartsWith("Fast-forwarded", GitService.SummarizePull(pull.Output));
        Assert.True(File.Exists(Path.Combine(Work, "b.txt")));
        Assert.Equal(0, (await GitService.GetStatusAsync(Work))!.Behind);

        var again = await GitService.PullFastForwardAsync(Work);
        Assert.True(again.Ok, again.Output);
        Assert.Equal("Already up to date.", GitService.SummarizePull(again.Output));
    }

    [Fact]
    public async Task Pull_refuses_to_merge_a_diverged_branch()
    {
        PushUpstreamCommit("b.txt", "theirs\n");
        File.WriteAllText(Path.Combine(Work, "c.txt"), "mine\n");
        Git(Work, "add", "c.txt");
        Git(Work, "commit", "-q", "-m", "mine");
        // A user's pull.rebase=true must not turn this into a rebase.
        Git(Work, "config", "pull.rebase", "true");
        string headBefore = Git(Work, "rev-parse", "HEAD").Trim();

        var pull = await GitService.PullFastForwardAsync(Work);

        Assert.False(pull.Ok);
        Assert.Contains("diverged", GitStatusText.FailureHint(pull.Output));
        Assert.Equal(headBefore, Git(Work, "rev-parse", "HEAD").Trim());
    }

    [Fact]
    public async Task Pull_refuses_to_overwrite_local_edits()
    {
        PushUpstreamCommit("a.txt", "theirs\n");
        File.WriteAllText(Path.Combine(Work, "a.txt"), "my uncommitted edit\n");

        var pull = await GitService.PullFastForwardAsync(Work);

        Assert.False(pull.Ok);
        Assert.Contains("Commit or stash", GitStatusText.FailureHint(pull.Output));
        Assert.Equal("my uncommitted edit\n", File.ReadAllText(Path.Combine(Work, "a.txt")));
    }

    [Fact]
    public async Task Switch_handles_a_branch_named_like_an_option()
    {
        Git(Work, "branch", "feature");
        Git(Work, "update-ref", "refs/heads/-x", "HEAD");

        var r1 = await GitService.SwitchBranchAsync(Work, "feature");
        Assert.True(r1.Ok, r1.Output);
        Assert.Equal("feature", Git(Work, "branch", "--show-current").Trim());

        var r2 = await GitService.SwitchBranchAsync(Work, "-x");
        Assert.True(r2.Ok, r2.Output);
        Assert.Equal("-x", Git(Work, "branch", "--show-current").Trim());
    }

    [Fact]
    public async Task Switch_does_not_invent_a_branch_that_exists_only_on_the_remote()
    {
        Git(Upstream, "push", "-q", "origin", "HEAD:remote-only");
        Git(Work, "fetch", "-q");

        var r = await GitService.SwitchBranchAsync(Work, "remote-only");

        Assert.False(r.Ok);
        Assert.Equal("main", Git(Work, "branch", "--show-current").Trim());
    }

    [Fact]
    public async Task Fetch_from_an_unreachable_https_remote_fails_fast_instead_of_prompting()
    {
        // Port 9 (discard) on localhost: refused at once. Without GIT_TERMINAL_PROMPT=0 the
        // interesting case is a remote that asks for credentials; this at least proves a
        // failing fetch returns promptly with git's message rather than hanging to the timeout.
        Git(Work, "remote", "set-url", "origin", "https://127.0.0.1:9/nope.git");
        var sw = Stopwatch.StartNew();

        var r = await GitService.FetchAsync(Work);

        Assert.False(r.Ok);
        Assert.False(r.TimedOut);
        Assert.False(string.IsNullOrWhiteSpace(r.Output));
        Assert.True(sw.Elapsed < TimeSpan.FromSeconds(30), $"took {sw.Elapsed}");
    }

    [Fact]
    public async Task StatusText_is_git_status_prose()
    {
        var r = await GitService.StatusTextAsync(Work);
        Assert.True(r.Ok, r.Output);
        Assert.Contains("On branch main", r.Output);
    }

    public void Dispose()
    {
        try
        {
            // git marks pack files read-only; clear that or Delete throws.
            foreach (var f in Directory.EnumerateFiles(_root, "*", SearchOption.AllDirectories))
                File.SetAttributes(f, FileAttributes.Normal);
            Directory.Delete(_root, recursive: true);
        }
        catch { }
    }
}

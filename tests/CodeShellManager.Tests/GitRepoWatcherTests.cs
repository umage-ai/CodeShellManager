using System;
using System.IO;
using System.Threading;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for the .git watcher that replaces most of the 10s poll (issue #70).
/// </summary>
public class GitRepoWatcherTests : IDisposable
{
    private readonly string _root = Path.Combine(
        Path.GetTempPath(), $"csm-gitwatch-{Guid.NewGuid():N}");

    public GitRepoWatcherTests() => Directory.CreateDirectory(_root);

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    private string MakeRepo(string name)
    {
        string work = Path.Combine(_root, name);
        Directory.CreateDirectory(Path.Combine(work, ".git"));
        File.WriteAllText(Path.Combine(work, ".git", "HEAD"), "ref: refs/heads/main\n");
        return work;
    }

    [Fact]
    public void ResolveGitDir_finds_a_plain_repo()
    {
        string work = MakeRepo("plain");
        Assert.Equal(Path.Combine(work, ".git"), GitRepoWatcher.ResolveGitDir(work));
    }

    [Fact]
    public void ResolveGitDir_walks_up_from_a_subfolder()
    {
        string work = MakeRepo("nested");
        string deep = Path.Combine(work, "src", "a", "b");
        Directory.CreateDirectory(deep);

        Assert.Equal(Path.Combine(work, ".git"), GitRepoWatcher.ResolveGitDir(deep));
    }

    [Fact]
    public void ResolveGitDir_follows_a_worktree_gitdir_file()
    {
        // A linked worktree has .git as a FILE pointing at the per-worktree directory.
        // Watching the main repo's .git instead would report the wrong branch for that
        // session entirely, so this indirection has to be followed.
        string main = MakeRepo("main");
        string wtGitDir = Path.Combine(main, ".git", "worktrees", "feature");
        Directory.CreateDirectory(wtGitDir);
        File.WriteAllText(Path.Combine(wtGitDir, "HEAD"), "ref: refs/heads/feature\n");

        string wt = Path.Combine(_root, "feature-wt");
        Directory.CreateDirectory(wt);
        File.WriteAllText(Path.Combine(wt, ".git"), $"gitdir: {wtGitDir}\n");

        Assert.Equal(wtGitDir, GitRepoWatcher.ResolveGitDir(wt));
    }

    [Fact]
    public void ResolveGitDir_returns_null_outside_a_repo()
    {
        string plain = Path.Combine(_root, "not-a-repo");
        Directory.CreateDirectory(plain);

        // Temp itself must not be inside a repo for this to be meaningful.
        Assert.Null(GitRepoWatcher.ResolveGitDir(plain));
    }

    [Fact]
    public void TryCreate_returns_null_outside_a_repo_rather_than_throwing()
    {
        // Callers treat null as "poll only". A session in a plain folder is valid, not an error.
        string plain = Path.Combine(_root, "plain-folder");
        Directory.CreateDirectory(plain);

        Assert.Null(GitRepoWatcher.TryCreate(plain));
    }

    [Fact]
    public void Writing_HEAD_raises_Changed()
    {
        string work = MakeRepo("head-change");
        using var watcher = GitRepoWatcher.TryCreate(work);
        Assert.NotNull(watcher);

        using var fired = new ManualResetEventSlim(false);
        watcher!.Changed += () => fired.Set();

        File.WriteAllText(Path.Combine(work, ".git", "HEAD"), "ref: refs/heads/other\n");

        Assert.True(fired.Wait(TimeSpan.FromSeconds(10)),
            "a HEAD write must refresh git state — that is the branch-switch case the " +
            "watcher exists to catch without polling");
    }

    [Fact]
    public void Writing_an_unrelated_file_under_git_does_not_raise_Changed()
    {
        // The point of the watcher is to stop spawning git when nothing relevant happened.
        // If object/log churn woke it, it would reintroduce the cost it was built to remove.
        string work = MakeRepo("noise");
        using var watcher = GitRepoWatcher.TryCreate(work);
        Assert.NotNull(watcher);

        using var fired = new ManualResetEventSlim(false);
        watcher!.Changed += () => fired.Set();

        File.WriteAllText(Path.Combine(work, ".git", "COMMIT_EDITMSG"), "wip\n");
        File.WriteAllText(Path.Combine(work, ".git", "config"), "[core]\n");

        Assert.False(fired.Wait(TimeSpan.FromSeconds(2)),
            "only HEAD and index should wake the watcher");
    }

    [Fact]
    public void Rapid_writes_are_debounced_into_one_notification()
    {
        // A single checkout rewrites index and HEAD and produces lock-file churn around
        // both. Without debouncing that is several git spawns for one user action.
        string work = MakeRepo("debounce");
        using var watcher = GitRepoWatcher.TryCreate(work);
        Assert.NotNull(watcher);

        int count = 0;
        watcher!.Changed += () => Interlocked.Increment(ref count);

        for (int i = 0; i < 10; i++)
        {
            File.WriteAllText(Path.Combine(work, ".git", "HEAD"), $"ref: refs/heads/b{i}\n");
            File.WriteAllText(Path.Combine(work, ".git", "index"), new string('x', 16 + i));
        }

        Thread.Sleep(2000);
        Assert.Equal(1, Volatile.Read(ref count));
    }

    [Fact]
    public void Acquire_shares_one_watcher_across_sessions_in_the_same_repo()
    {
        // Several sessions in one repo is the normal case — that is what the worktree
        // sibling feature is for. One FileSystemWatcher per session meant N kernel handles
        // and N buffers on the same directory.
        string work = MakeRepo("shared");
        int before = GitRepoWatcher.SharedCount;

        var a = GitRepoWatcher.Acquire(work);
        var b = GitRepoWatcher.Acquire(work);
        try
        {
            Assert.NotNull(a);
            Assert.Same(a, b);
            Assert.Equal(before + 1, GitRepoWatcher.SharedCount);
        }
        finally
        {
            GitRepoWatcher.Release(a);
            GitRepoWatcher.Release(b);
        }
    }

    [Fact]
    public void Release_keeps_the_watcher_alive_until_the_last_session_lets_go()
    {
        string work = MakeRepo("refcount");
        var a = GitRepoWatcher.Acquire(work);
        var b = GitRepoWatcher.Acquire(work);
        Assert.NotNull(a);

        using var fired = new ManualResetEventSlim(false);
        a!.Changed += () => fired.Set();

        // First session closes. The watcher must survive for the second.
        GitRepoWatcher.Release(a);

        File.WriteAllText(Path.Combine(work, ".git", "HEAD"), "ref: refs/heads/still-live\n");
        Assert.True(fired.Wait(TimeSpan.FromSeconds(10)),
            "releasing one session must not stop notifications for the others");

        GitRepoWatcher.Release(b);
        Assert.DoesNotContain(work, DescribeShared());
    }

    [Fact]
    public void Different_repos_get_different_watchers()
    {
        string one = MakeRepo("repo-one");
        string two = MakeRepo("repo-two");

        var a = GitRepoWatcher.Acquire(one);
        var b = GitRepoWatcher.Acquire(two);
        try
        {
            Assert.NotNull(a);
            Assert.NotNull(b);
            Assert.NotSame(a, b);
        }
        finally
        {
            GitRepoWatcher.Release(a);
            GitRepoWatcher.Release(b);
        }
    }

    [Fact]
    public void Acquire_outside_a_repo_returns_null_and_registers_nothing()
    {
        string plain = Path.Combine(_root, "plain-acquire");
        Directory.CreateDirectory(plain);
        int before = GitRepoWatcher.SharedCount;

        Assert.Null(GitRepoWatcher.Acquire(plain));
        Assert.Equal(before, GitRepoWatcher.SharedCount);
    }

    // The shared map is keyed by .git dir; this just gives the assertion above something
    // readable to fail against.
    private static string DescribeShared() => $"shared={GitRepoWatcher.SharedCount}";

    [Fact]
    public void Dispose_stops_notifications()
    {
        string work = MakeRepo("disposed");
        var watcher = GitRepoWatcher.TryCreate(work);
        Assert.NotNull(watcher);

        int count = 0;
        watcher!.Changed += () => Interlocked.Increment(ref count);
        watcher.Dispose();

        File.WriteAllText(Path.Combine(work, ".git", "HEAD"), "ref: refs/heads/after\n");
        Thread.Sleep(1500);

        Assert.Equal(0, Volatile.Read(ref count));
    }
}

using System;
using System.IO;
using System.Collections.Concurrent;
using System.Threading;
using System.Threading.Tasks;
using CodeShellManager.Diagnostics;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Regression tests for issue #70.
///
/// GitService used to run <c>Process.Start</c> — and every continuation after it — on
/// whatever thread called in. Because the poll chain begins in SessionViewModel's
/// constructor on the WPF UI thread, that put ~94 synchronous process creations per poll
/// cycle onto the UI thread at 47 sessions, freezing typing for seconds at a time.
///
/// These tests pin the property that prevents it: GitService must never depend on, or
/// return to, the caller's SynchronizationContext.
/// </summary>
[Collection("DiagnosticTrace")]
public class GitServiceThreadingTests
{
    /// <summary>
    /// A context that records every Post/Send but never runs the work. If any await in
    /// GitService captures the caller's context, the continuation is handed here, never
    /// executes, and the awaiting task never completes — so the test times out.
    /// </summary>
    private sealed class DeadSynchronizationContext : SynchronizationContext
    {
        public readonly ConcurrentQueue<string> Captured = new();
        public override void Post(SendOrPostCallback d, object? state) => Captured.Enqueue("Post");
        public override void Send(SendOrPostCallback d, object? state) => Captured.Enqueue("Send");
    }

    private static async Task<T> OnDeadContextAsync<T>(Func<Task<T>> work, DeadSynchronizationContext ctx)
    {
        // Run on a dedicated thread that carries the dead context, mirroring the UI thread.
        var tcs = new TaskCompletionSource<Task<T>>();
        var thread = new Thread(() =>
        {
            SynchronizationContext.SetSynchronizationContext(ctx);
            tcs.SetResult(work());
        });
        thread.IsBackground = true;
        thread.Start();

        Task<T> started = await tcs.Task;
        Task completed = await Task.WhenAny(started, Task.Delay(TimeSpan.FromSeconds(20)));

        Assert.True(ReferenceEquals(completed, started),
            "GitService did not complete on a caller whose SynchronizationContext never runs " +
            "work. That means an await captured the context — the exact defect that put git " +
            "process creation on the WPF UI thread (issue #70).");

        return await started;
    }

    [Fact]
    public async Task GetGitInfoAsync_completes_without_the_callers_context_ever_running()
    {
        var ctx = new DeadSynchronizationContext();

        // The repo itself — a real git repo, so the call does real work rather than
        // short-circuiting on the not-a-directory guard.
        //
        // The assertion is COMPLETION, not the branch name. `branch --show-current` prints
        // nothing on a detached HEAD, and actions/checkout leaves exactly that for tag
        // pushes and PR merge refs — asserting a non-empty branch would have failed CI on
        // the two events the test step exists for, aborting the release job behind it.
        string repo = TestRepoPath();

        // Completion IS the assertion — OnDeadContextAsync fails if the call never returns,
        // which is what a captured context would cause.
        await OnDeadContextAsync(() => GitService.GetGitInfoAsync(repo), ctx);
    }

    [Fact]
    public async Task GetRepoRootAsync_completes_without_the_callers_context_ever_running()
    {
        var ctx = new DeadSynchronizationContext();

        string repo = TestRepoPath();
        string? root = await OnDeadContextAsync(() => GitService.GetRepoRootAsync(repo), ctx);

        Assert.False(string.IsNullOrWhiteSpace(root));
    }

    [Fact]
    public async Task Process_creation_never_happens_on_the_thread_designated_as_the_UI_thread()
    {
        // The tests above prove we don't *return* to the caller's context. This proves we
        // don't *start* on its thread either — Process.Start sat before the first await, so
        // it ran inline on the caller regardless of what any await did afterwards.
        //
        // Uses the GIT-SPAWN probe that shipped with the diagnosis, so the test asserts the
        // same signal the live investigation read out of crash.log.
        string log = Path.Combine(Path.GetTempPath(), $"csm-gitspawn-{Guid.NewGuid():N}.log");
        DiagnosticTrace.ResetForTests(log);
        DiagnosticTrace.Enabled = true;
        try
        {
            // Driven from a DEDICATED thread, not the xunit one. xunit runs tests on the
            // thread pool, and Task.Run is free to reuse the calling pool thread — so
            // designating the test's own thread as "the UI thread" made this pass or fail
            // by luck. A dedicated thread is one the pool can never hand back.
            Exception? failure = null;
            var driver = new Thread(() =>
            {
                DiagnosticTrace.UiThreadId = Environment.CurrentManagedThreadId;
                try { GitService.GetGitInfoAsync(TestRepoPath()).GetAwaiter().GetResult(); }
                catch (Exception ex) { failure = ex; }
            });
            driver.IsBackground = true;
            driver.Start();
            Assert.True(driver.Join(TimeSpan.FromSeconds(30)), "git call did not finish in time");
            Assert.Null(failure);

            DiagnosticTrace.DrainOnce();

            string content = File.ReadAllText(log);
            Assert.Contains("GIT-SPAWN", content);
            Assert.DoesNotContain("on-ui=True", content);
        }
        finally
        {
            DiagnosticTrace.Enabled = false;
            DiagnosticTrace.UiThreadId = 0;
            try { File.Delete(log); } catch { }
        }
        await Task.CompletedTask;
    }

    private static string TestRepoPath()
    {
        // Walk up from the test binary to the repo root (the folder containing .git).
        var dir = new System.IO.DirectoryInfo(AppContext.BaseDirectory);
        while (dir != null && !System.IO.Directory.Exists(System.IO.Path.Combine(dir.FullName, ".git")))
            dir = dir.Parent;

        Assert.NotNull(dir);
        return dir!.FullName;
    }
}

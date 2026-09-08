using System;
using System.IO;
using System.Threading;

namespace CodeShellManager.Services;

/// <summary>
/// Raises <see cref="Changed"/> when a repo's <c>HEAD</c> or <c>index</c> is written —
/// i.e. on checkout, commit, merge, rebase, or staging (issue #70).
///
/// This exists so git state can be refreshed on the events that change it instead of by
/// asking every ten seconds. Polling cost is dominated by process creation, not by git:
/// `git --version` measures 42ms against `branch --show-current` at 41ms, so each poll is
/// ~all overhead and there is no cheaper query to switch to. At 47 sessions that was ~94
/// spawns per cycle to learn that nothing had changed.
///
/// Deliberately NOT a substitute for polling: a plain working-tree edit makes
/// `status --porcelain` dirty without touching anything under <c>.git</c>. The watcher
/// covers git operations; a slow poll still covers dirtiness. See
/// <c>SessionViewModel.PollGitInfoAsync</c>.
/// </summary>
public sealed class GitRepoWatcher : IDisposable
{
    // Coalesces the burst a single git command produces — a checkout rewrites index and
    // HEAD, and .NET reports lock/temp churn around both.
    private const int DebounceMs = 400;

    private readonly FileSystemWatcher _watcher;
    private readonly System.Threading.Timer _debounce;
    private bool _disposed;

    /// <summary>Fired on a threadpool thread after the debounce window closes.</summary>
    public event Action? Changed;

    private GitRepoWatcher(string gitDir)
    {
        _debounce = new System.Threading.Timer(_ => { if (!_disposed) Changed?.Invoke(); },
                              null, Timeout.Infinite, Timeout.Infinite);

        _watcher = new FileSystemWatcher(gitDir)
        {
            // HEAD and index are both written as whole files (often via rename-over), so
            // FileName has to be watched as well as LastWrite or a checkout can be missed.
            NotifyFilter = NotifyFilters.LastWrite | NotifyFilters.FileName | NotifyFilters.Size,
            IncludeSubdirectories = false
        };

        _watcher.Changed += OnAny;
        _watcher.Created += OnAny;
        _watcher.Renamed += OnAny;
        _watcher.EnableRaisingEvents = true;
    }

    /// <summary>
    /// Creates a watcher for the repo containing <paramref name="workingFolder"/>, or null
    /// if it isn't in a repo or the platform refuses the watch. Callers treat null as
    /// "poll only" rather than an error — a session in a plain folder is perfectly valid.
    ///
    /// Prefer <see cref="Acquire"/>: several sessions commonly sit in the same repo (that is
    /// the entire point of the worktree-sibling feature) and each would otherwise get its
    /// own FileSystemWatcher on the same directory.
    /// </summary>
    public static GitRepoWatcher? TryCreate(string workingFolder)
    {
        try
        {
            string? gitDir = ResolveGitDir(workingFolder);
            if (gitDir == null || !Directory.Exists(gitDir)) return null;
            return new GitRepoWatcher(gitDir);
        }
        catch
        {
            return null;
        }
    }

    // ── Sharing ───────────────────────────────────────────────────────────────
    // One watcher per .git directory, reference-counted, rather than one per session.
    // At 47 sessions across ~20 repos that is 20 kernel watch handles and 20 buffers
    // instead of 47 of each, and a single git operation wakes one watcher rather than
    // every session that happens to share the repo.

    private static readonly object SharedLock = new();
    private static readonly Dictionary<string, (GitRepoWatcher Watcher, int RefCount)> Shared =
        new(StringComparer.OrdinalIgnoreCase);

    private string? _sharedKey;

    /// <summary>
    /// Returns the shared watcher for this folder's repo, creating it on first use.
    /// Release it with <see cref="Release"/> — never <c>Dispose</c> a shared instance
    /// directly, or the other sessions in that repo stop receiving events.
    /// </summary>
    public static GitRepoWatcher? Acquire(string workingFolder)
    {
        string? gitDir;
        try
        {
            gitDir = ResolveGitDir(workingFolder);
            if (gitDir == null || !Directory.Exists(gitDir)) return null;
            // Normalize before it becomes a dictionary key: C:/repo/.git and C:\repo\.git
            // are the same directory and must not get two watchers.
            gitDir = Path.GetFullPath(gitDir);
        }
        catch { return null; }

        // Construct OUTSIDE the lock. SharedLock is global and this is called from the UI
        // thread; creating a FileSystemWatcher touches the kernel and, on a slow or offline
        // path, can block. Holding a global lock across that would stall every other
        // session's Acquire/Release behind one bad repo.
        GitRepoWatcher? candidate = null;
        lock (SharedLock)
        {
            if (Shared.TryGetValue(gitDir, out var existing))
            {
                Shared[gitDir] = (existing.Watcher, existing.RefCount + 1);
                return existing.Watcher;
            }
        }

        try { candidate = new GitRepoWatcher(gitDir); }
        catch { return null; }

        GitRepoWatcher? loser = null;
        GitRepoWatcher result;
        lock (SharedLock)
        {
            // Someone may have won the race while we were constructing. Keep theirs.
            if (Shared.TryGetValue(gitDir, out var raced))
            {
                Shared[gitDir] = (raced.Watcher, raced.RefCount + 1);
                loser = candidate;
                result = raced.Watcher;
            }
            else
            {
                candidate._sharedKey = gitDir;
                Shared[gitDir] = (candidate, 1);
                result = candidate;
            }
        }

        // Outside the lock: Dispose tears down a kernel watch handle.
        loser?.Dispose();
        return result;
    }

    /// <summary>Drops one reference; disposes the watcher when the last session lets go.</summary>
    public static void Release(GitRepoWatcher? watcher)
    {
        if (watcher is null) return;
        if (watcher._sharedKey is not string key) { watcher.Dispose(); return; }

        GitRepoWatcher? toDispose = null;
        lock (SharedLock)
        {
            if (!Shared.TryGetValue(key, out var entry)) return;

            // Identity check. A stale double-Release must not decrement — or dispose — the
            // *replacement* watcher registered for the same .git dir after this one was
            // torn down, which would silently kill events for a live session.
            //
            // A double-Release of the *same* live watcher would still over-decrement. It is
            // unreachable today because SessionViewModel.Dispose is idempotent and nulls its
            // field, which is the invariant callers must keep: Release exactly once per
            // successful Acquire.
            if (!ReferenceEquals(entry.Watcher, watcher)) return;

            if (entry.RefCount > 1)
            {
                Shared[key] = (entry.Watcher, entry.RefCount - 1);
                return;
            }

            Shared.Remove(key);
            entry.Watcher._sharedKey = null;
            toDispose = entry.Watcher;
        }

        // Outside the lock: Dispose tears down a kernel watch handle, and SharedLock is
        // global — holding it across that stalls every other session's Acquire/Release.
        toDispose?.Dispose();
    }

    /// <summary>Live shared-watcher count. Tests only.</summary>
    internal static int SharedCount { get { lock (SharedLock) return Shared.Count; } }

    /// <summary>
    /// Walks up from <paramref name="startFolder"/> looking for <c>.git</c>. A directory is
    /// the ordinary case; a *file* means a linked worktree, and its <c>gitdir:</c> line
    /// points at the per-worktree directory that actually holds that worktree's HEAD and
    /// index. Watching the main repo's .git for a worktree session would report the wrong
    /// branch entirely, so the indirection has to be followed.
    /// </summary>
    internal static string? ResolveGitDir(string startFolder)
    {
        if (string.IsNullOrWhiteSpace(startFolder)) return null;

        var dir = new DirectoryInfo(startFolder);
        while (dir != null)
        {
            string candidate = Path.Combine(dir.FullName, ".git");

            if (Directory.Exists(candidate)) return candidate;

            if (File.Exists(candidate))
            {
                foreach (string line in File.ReadAllLines(candidate))
                {
                    if (!line.StartsWith("gitdir:", StringComparison.OrdinalIgnoreCase)) continue;
                    string target = line["gitdir:".Length..].Trim();
                    if (target.Length == 0) return null;
                    return Path.IsPathRooted(target)
                        ? Path.GetFullPath(target)
                        : Path.GetFullPath(target, dir.FullName);
                }
                return null;
            }

            dir = dir.Parent;
        }
        return null;
    }

    private void OnAny(object sender, FileSystemEventArgs e)
    {
        if (_disposed) return;

        string name = e.Name ?? "";
        // Anything else under .git — objects, logs, config, packed-refs, lock files — either
        // doesn't change what we display or is already implied by a HEAD/index write.
        if (!name.Equals("HEAD", StringComparison.OrdinalIgnoreCase)
            && !name.Equals("index", StringComparison.OrdinalIgnoreCase))
            return;

        try { _debounce.Change(DebounceMs, Timeout.Infinite); } catch (ObjectDisposedException) { }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        try { _watcher.EnableRaisingEvents = false; } catch { }
        _watcher.Changed -= OnAny;
        _watcher.Created -= OnAny;
        _watcher.Renamed -= OnAny;
        _watcher.Dispose();
        _debounce.Dispose();
    }
}

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

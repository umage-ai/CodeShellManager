using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Threading.Tasks;

namespace CodeShellManager.Services;

public static class GitService
{
    public static async Task<(string? branch, bool isDirty)> GetGitInfoAsync(string folderPath)
    {
        if (string.IsNullOrWhiteSpace(folderPath) || !System.IO.Directory.Exists(folderPath))
            return (null, false);

        try
        {
            string? branch = await RunGitAsync(folderPath, "branch --show-current");
            if (string.IsNullOrWhiteSpace(branch))
                return (null, false);

            string? statusOutput = await RunGitAsync(folderPath, "status --porcelain");
            bool isDirty = !string.IsNullOrWhiteSpace(statusOutput);

            return (branch.Trim(), isDirty);
        }
        catch
        {
            return (null, false);
        }
    }

    /// <summary>
    /// Returns the absolute path to the repo's top-level directory (the path that
    /// contains the .git folder/file), or null if folderPath is not inside a repo.
    /// For a worktree this returns the worktree root, not the main repo root.
    /// </summary>
    public static async Task<string?> GetRepoRootAsync(string folderPath)
    {
        if (string.IsNullOrWhiteSpace(folderPath) || !System.IO.Directory.Exists(folderPath))
            return null;
        try
        {
            string? top = await RunGitAsync(folderPath, "rev-parse --show-toplevel");
            return string.IsNullOrWhiteSpace(top) ? null : top.Trim();
        }
        catch { return null; }
    }

    /// <summary>Returns local branch names in the repo, oldest-first by git's default order.</summary>
    public static async Task<IReadOnlyList<string>> ListBranchesAsync(string folderPath)
    {
        if (string.IsNullOrWhiteSpace(folderPath) || !System.IO.Directory.Exists(folderPath))
            return Array.Empty<string>();
        try
        {
            string? raw = await RunGitAsync(folderPath, "for-each-ref --format=%(refname:short) refs/heads");
            if (string.IsNullOrWhiteSpace(raw)) return Array.Empty<string>();
            var lines = raw.Replace("\r", "").Split('\n', StringSplitOptions.RemoveEmptyEntries);
            return lines;
        }
        catch { return Array.Empty<string>(); }
    }

    /// <summary>
    /// Runs `git worktree add` either with a new branch (-b) or pointing at an
    /// existing ref. Returns (success, errorOutput).
    /// </summary>
    public static async Task<(bool ok, string error)> CreateWorktreeAsync(
        string repoRoot, string targetPath, string branchOrRef, bool createBranch)
    {
        if (string.IsNullOrWhiteSpace(repoRoot) || !System.IO.Directory.Exists(repoRoot))
            return (false, "Repo root does not exist.");
        if (string.IsNullOrWhiteSpace(targetPath))
            return (false, "Worktree path is required.");
        if (string.IsNullOrWhiteSpace(branchOrRef))
            return (false, "Branch is required.");

        string args = createBranch
            ? $"worktree add -b \"{branchOrRef}\" \"{targetPath}\""
            : $"worktree add \"{targetPath}\" \"{branchOrRef}\"";

        var (output, stderr, exit) = await RunGitFullAsync(repoRoot, args, timeoutMs: 30_000);
        if (exit == 0) return (true, "");
        string err = string.IsNullOrWhiteSpace(stderr)
            ? (string.IsNullOrWhiteSpace(output) ? "git worktree add failed." : output)
            : stderr;
        return (false, err.Trim());
    }

    private static async Task<string?> RunGitAsync(string workingDir, string arguments)
    {
        var (stdout, _, exit) = await RunGitFullAsync(workingDir, arguments, timeoutMs: 3000);
        return exit == 0 ? stdout : null;
    }

    private static async Task<(string stdout, string stderr, int exit)> RunGitFullAsync(
        string workingDir, string arguments, int timeoutMs)
    {
        var psi = new ProcessStartInfo("git")
        {
            Arguments = $"-C \"{workingDir}\" {arguments}",
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true
        };

        using var process = Process.Start(psi);
        if (process is null) return ("", "", -1);

        var outTask = process.StandardOutput.ReadToEndAsync();
        var errTask = process.StandardError.ReadToEndAsync();
        var bothTask = Task.WhenAll(outTask, errTask);
        var completed = await Task.WhenAny(bothTask, Task.Delay(timeoutMs));

        if (completed != bothTask)
        {
            try { process.Kill(); } catch { }
        }
        try { await process.WaitForExitAsync(); } catch { }

        string stdout = outTask.IsCompletedSuccessfully ? outTask.Result : "";
        string stderr = errTask.IsCompletedSuccessfully ? errTask.Result : "";
        return (stdout, stderr, process.HasExited ? process.ExitCode : -1);
    }
}

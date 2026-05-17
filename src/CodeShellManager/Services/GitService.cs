using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Text;
using System.Text.RegularExpressions;
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
    /// Returns the canonical "repo identity" path — the parent of the shared .git
    /// directory (`git rev-parse --git-common-dir`). This is identical for every
    /// worktree of the same repo, so it's safe to use as a sibling-detection key.
    /// (`--show-toplevel` would return each worktree's own folder, missing siblings.)
    /// Returns null if folderPath isn't inside a repo. Forward slashes throughout
    /// for stable string comparison on Windows.
    /// </summary>
    public static async Task<string?> GetRepoRootAsync(string folderPath)
    {
        if (string.IsNullOrWhiteSpace(folderPath) || !System.IO.Directory.Exists(folderPath))
            return null;
        try
        {
            string? commonDir = await RunGitAsync(folderPath, "rev-parse --git-common-dir");
            if (string.IsNullOrWhiteSpace(commonDir)) return null;
            string trimmed = commonDir.Trim();

            // git may return a path relative to the cwd (e.g. ".git" for a plain repo)
            // or an absolute path (e.g. "C:/repo/.git" when called from a worktree).
            // Resolve to an absolute path either way.
            string absolute = System.IO.Path.IsPathRooted(trimmed)
                ? trimmed
                : System.IO.Path.GetFullPath(trimmed, folderPath);

            // Strip the trailing ".git" segment to get the repo's working tree root.
            string normalized = absolute.Replace('\\', '/').TrimEnd('/');
            if (normalized.EndsWith("/.git", StringComparison.OrdinalIgnoreCase))
                normalized = normalized[..^"/.git".Length];
            else if (normalized.EndsWith(".git", StringComparison.OrdinalIgnoreCase)
                && !normalized.EndsWith("/.git", StringComparison.OrdinalIgnoreCase))
                normalized = normalized[..^".git".Length].TrimEnd('/');

            return string.IsNullOrEmpty(normalized) ? null : normalized;
        }
        catch { return null; }
    }

    /// <summary>Describes one git worktree as reported by `git worktree list --porcelain`.</summary>
    public record WorktreeInfo(string Path, string? Branch, bool IsBare, bool IsDetached, bool IsLocked, bool IsPrunable);

    /// <summary>
    /// Returns all worktrees (including the main one) of the repo containing
    /// <paramref name="folderPath"/>. Empty if the folder isn't in a repo.
    /// </summary>
    public static async Task<IReadOnlyList<WorktreeInfo>> ListWorktreesAsync(string folderPath)
    {
        if (string.IsNullOrWhiteSpace(folderPath) || !System.IO.Directory.Exists(folderPath))
            return Array.Empty<WorktreeInfo>();
        try
        {
            string? raw = await RunGitAsync(folderPath, "worktree list --porcelain");
            if (string.IsNullOrWhiteSpace(raw)) return Array.Empty<WorktreeInfo>();

            // Output is blank-line separated stanzas:
            //   worktree /path
            //   HEAD <sha>
            //   branch refs/heads/<name>   (or "detached", "bare")
            //   locked [reason]
            //   prunable [reason]
            var results = new List<WorktreeInfo>();
            string? path = null;
            string? branch = null;
            bool isBare = false, isDetached = false, isLocked = false, isPrunable = false;
            void Flush()
            {
                if (!string.IsNullOrEmpty(path))
                    results.Add(new WorktreeInfo(path, branch, isBare, isDetached, isLocked, isPrunable));
                path = null; branch = null; isBare = false; isDetached = false; isLocked = false; isPrunable = false;
            }
            foreach (var line in raw.Replace("\r", "").Split('\n'))
            {
                if (string.IsNullOrEmpty(line)) { Flush(); continue; }
                if (line.StartsWith("worktree ")) path = line.Substring("worktree ".Length).Trim();
                else if (line.StartsWith("branch ")) branch = line.Substring("branch ".Length).Trim()
                    .Replace("refs/heads/", "", StringComparison.Ordinal);
                else if (line == "bare") isBare = true;
                else if (line == "detached") isDetached = true;
                else if (line.StartsWith("locked")) isLocked = true;
                else if (line.StartsWith("prunable")) isPrunable = true;
            }
            Flush();
            return results;
        }
        catch { return Array.Empty<WorktreeInfo>(); }
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
        // WSL working folders (\\wsl$\<distro>\…) get routed through wsl.exe so git
        // runs inside the distro. Git for Windows trips on WSL UNCs (dubious-ownership
        // checks, .git symlink quirks) and reports "not a git repo" for valid repos.
        var (wslDistro, linuxPath) = TryParseWslUnc(workingDir);
        if (wslDistro != null)
            return await RunGitInWslAsync(wslDistro, linuxPath, arguments, timeoutMs);

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

    /// <summary>
    /// Runs <c>wsl.exe -d &lt;distro&gt; -- git -C &lt;linuxPath&gt; &lt;arguments&gt;</c>.
    /// Translates any WSL UNC paths in <paramref name="arguments"/> to Linux form
    /// before invocation (so things like <c>worktree add "\\wsl$\Ubuntu\…"</c> reach
    /// git as a normal Linux path), and translates absolute Linux paths in stdout
    /// back to UNC form so callers receive Windows-shaped paths.
    /// </summary>
    private static async Task<(string stdout, string stderr, int exit)> RunGitInWslAsync(
        string distro, string linuxPath, string arguments, int timeoutMs)
    {
        string translatedArgs = TranslateUncArgsToLinux(arguments, distro);
        // QuoteForCmd handles spaces in both the distro name (rare) and the cwd
        // (Linux paths often have them) without disturbing the simple-name case.
        string cwd = string.IsNullOrEmpty(linuxPath) ? "/" : linuxPath;
        string args = $"-d {Models.ShellSession.QuoteForCmd(distro)} -- git -C {Models.ShellSession.QuoteForCmd(cwd)} {translatedArgs}";

        var psi = new ProcessStartInfo("wsl.exe")
        {
            Arguments = args,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true,
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8,
        };

        using var process = Process.Start(psi);
        if (process is null) return ("", "", -1);

        var outTask = process.StandardOutput.ReadToEndAsync();
        var errTask = process.StandardError.ReadToEndAsync();
        var bothTask = Task.WhenAll(outTask, errTask);
        var completed = await Task.WhenAny(bothTask, Task.Delay(timeoutMs));
        if (completed != bothTask) { try { process.Kill(); } catch { } }
        try { await process.WaitForExitAsync(); } catch { }

        string stdout = outTask.IsCompletedSuccessfully ? outTask.Result : "";
        string stderr = errTask.IsCompletedSuccessfully ? errTask.Result : "";
        stdout = TranslateLinuxPathsToUnc(stdout, distro);
        return (stdout, stderr, process.HasExited ? process.ExitCode : -1);
    }

    /// <summary>
    /// Detects a <c>\\wsl$\&lt;distro&gt;\…</c> or <c>\\wsl.localhost\&lt;distro&gt;\…</c>
    /// path and splits it into (distro, linux-path). Returns (null, "") otherwise.
    /// </summary>
    internal static (string? distro, string linuxPath) TryParseWslUnc(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return (null, "");
        string normalized = path.Replace('/', '\\').TrimEnd('\\');
        string[] prefixes = { @"\\wsl$\", @"\\wsl.localhost\" };
        foreach (var prefix in prefixes)
        {
            if (!normalized.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) continue;
            string rest = normalized[prefix.Length..];
            if (string.IsNullOrEmpty(rest)) return (null, "");
            int slash = rest.IndexOf('\\');
            string distro = slash < 0 ? rest : rest[..slash];
            string linuxRest = slash < 0 ? "" : rest[(slash + 1)..];
            string linuxPath = string.IsNullOrEmpty(linuxRest) ? "/" : "/" + linuxRest.Replace('\\', '/');
            return (distro, linuxPath);
        }
        return (null, "");
    }

    /// <summary>
    /// Replaces WSL UNC tokens in a git arg string with their Linux equivalents.
    /// Only translates UNCs that belong to <paramref name="distro"/> — a UNC for a
    /// different distro is passed through unchanged (so the caller sees the eventual
    /// "no such directory" error rather than silently aiming at the wrong tree).
    /// </summary>
    internal static string TranslateUncArgsToLinux(string arguments, string distro)
    {
        if (string.IsNullOrEmpty(arguments)) return arguments;
        string esc = Regex.Escape(distro);
        string body = $@"\\\\wsl(?:\$|\.localhost)\\{esc}";

        // Pass 1: quoted UNCs ("\\wsl$\<distro>\..."). The tail may contain spaces
        // and runs until the closing quote — without this pass, the unquoted regex
        // below would stop at the first space and produce a half-translated path.
        arguments = Regex.Replace(arguments, $@"""({body}(?:\\[^""]*)?)""", m =>
        {
            var (_, linux) = TryParseWslUnc(m.Groups[1].Value);
            return "\"" + (string.IsNullOrEmpty(linux) ? "/" : linux) + "\"";
        }, RegexOptions.IgnoreCase);

        // Pass 2: unquoted UNCs. The tail runs to whitespace; if a path needed
        // spaces it would have been quoted and handled above.
        arguments = Regex.Replace(arguments, $@"{body}(?:\\[^""\s]*)?", m =>
        {
            var (_, linux) = TryParseWslUnc(m.Value);
            return string.IsNullOrEmpty(linux) ? "/" : linux;
        }, RegexOptions.IgnoreCase);

        return arguments;
    }

    /// <summary>
    /// Replaces absolute Linux paths in <paramref name="text"/> (typically git stdout)
    /// with <c>\\wsl$\&lt;distro&gt;\…</c> equivalents so callers see Windows-shaped
    /// paths. Conservative — only matches tokens at start-of-line or after whitespace
    /// to avoid mangling text that happens to contain a slash.
    /// </summary>
    internal static string TranslateLinuxPathsToUnc(string text, string distro)
    {
        if (string.IsNullOrEmpty(text)) return text;
        // Tail used to stop at any whitespace, which mangled paths containing spaces
        // (`/home/alice/My Projects/proj` came back as `\\wsl$\Ubuntu\home\alice\My`
        // with the rest left as forward-slashed garbage). Our callers (rev-parse,
        // worktree list --porcelain) always emit the path as the full remainder of
        // the line, so widening the tail to "anything but newline / shell-meta" is
        // safe and recovers space-containing paths correctly.
        return Regex.Replace(text, @"(^|[\s=:])(/[^\r\n'""<>|]+)", m =>
        {
            string linuxPath = m.Groups[2].Value;
            string unc = $@"\\wsl$\{distro}" + linuxPath.Replace('/', '\\');
            return m.Groups[1].Value + unc;
        }, RegexOptions.Multiline);
    }
}

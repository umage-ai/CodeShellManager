using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading.Tasks;

namespace CodeShellManager.Services;

public static class GitService
{
    /// <summary>
    /// Branch + dirty, as the sidebar and the git poll have always consumed them. A detached
    /// HEAD reports (null, false), exactly as the old <c>branch --show-current</c> pair did.
    /// </summary>
    public static async Task<(string? branch, bool isDirty)> GetGitInfoAsync(string folderPath)
    {
        var status = await GetStatusAsync(folderPath).ConfigureAwait(false);
        if (status is null || status.IsDetached || string.IsNullOrEmpty(status.Branch))
            return (null, false);
        return (status.Branch, status.IsDirty);
    }

    /// <summary>
    /// Branch, upstream, ahead/behind and changed files in ONE git spawn
    /// (<c>status --porcelain=v2 --branch</c>). The poll used to spend two spawns on
    /// <c>branch --show-current</c> + <c>status --porcelain</c>, and a spawn is ~all of the
    /// cost (see "Never spawn a process on the UI thread"), so this is cheaper as well as
    /// richer. Null when the folder isn't in a repo or git failed.
    ///
    /// Runs with <c>GIT_OPTIONAL_LOCKS=0</c>: this is a background read, and a plain
    /// <c>git status</c> opportunistically takes <c>index.lock</c> to refresh stat data — which
    /// is how a poll makes a git command the user (or Claude) runs at the same moment fail with
    /// "index.lock exists".
    /// </summary>
    public static async Task<GitRepoStatus?> GetStatusAsync(string folderPath)
    {
        if (string.IsNullOrWhiteSpace(folderPath) || !System.IO.Directory.Exists(folderPath))
            return null;
        try
        {
            var (stdout, _, exit) = await RunGitFullAsync(folderPath,
                new[] { "-c", "core.quotePath=false", "status", "--porcelain=v2", "--branch" },
                timeoutMs: 3000, readOnly: true).ConfigureAwait(false);
            return exit == 0 ? ParseStatusV2(stdout) : null;
        }
        catch { return null; }
    }

    /// <summary>
    /// Parses <c>git status --porcelain=v2 --branch</c>. Pure, so it is unit-tested against
    /// captured output rather than a live repo.
    /// </summary>
    internal static GitRepoStatus ParseStatusV2(string output)
    {
        string? head = null, upstream = null;
        int? ahead = null, behind = null;
        var changes = new List<GitFileChange>();

        foreach (string line in (output ?? "").Replace("\r", "").Split('\n'))
        {
            if (line.Length == 0) continue;
            if (line.StartsWith("# branch.head ", StringComparison.Ordinal))
                head = line["# branch.head ".Length..];
            else if (line.StartsWith("# branch.upstream ", StringComparison.Ordinal))
                upstream = line["# branch.upstream ".Length..];
            else if (line.StartsWith("# branch.ab ", StringComparison.Ordinal))
            {
                // "# branch.ab +<ahead> -<behind>"
                var parts = line["# branch.ab ".Length..].Split(' ', StringSplitOptions.RemoveEmptyEntries);
                if (parts.Length == 2
                    && int.TryParse(parts[0].TrimStart('+'), out int a)
                    && int.TryParse(parts[1].TrimStart('-'), out int b))
                { ahead = a; behind = b; }
            }
            else if (line[0] == '#') continue;
            else if (line.StartsWith("? ", StringComparison.Ordinal))
                changes.Add(new GitFileChange("??", line[2..]));
            else if (line.StartsWith("! ", StringComparison.Ordinal))
                continue; // ignored files — only listed with --ignored, never by us
            else
            {
                // 1 XY sub mH mI mW hH hI <path>
                // 2 XY sub mH mI mW hH hI Xscore <path>\t<origPath>
                // u XY sub m1 m2 m3 mW h1 h2 h3 <path>
                int fieldsBeforePath = line[0] switch { '1' => 8, '2' => 9, 'u' => 10, _ => -1 };
                if (fieldsBeforePath < 0) continue;
                var f = line.Split(' ', fieldsBeforePath + 1);
                if (f.Length <= fieldsBeforePath || f[1].Length != 2) continue;
                string path = f[fieldsBeforePath];
                if (line[0] == '2')
                {
                    int tab = path.IndexOf('\t');
                    if (tab >= 0) path = $"{path[(tab + 1)..]} → {path[..tab]}";
                }
                changes.Add(new GitFileChange(line[0] == 'u' ? "UU" : f[1].Replace('.', ' '), path));
            }
        }

        bool detached = head == "(detached)";
        return new GitRepoStatus(detached ? null : head, detached, upstream, ahead, behind, changes);
    }

    // ── User-initiated operations (the toolbar git menu) ─────────────────────────────
    //
    // None of these ever run unprompted, and none of them write history: no commit, no push,
    // no merge beyond a fast-forward. A Claude session is usually editing this same working
    // tree, and a one-click history write from outside its conversation is both a race and
    // a misclick that cannot be taken back.

    /// <summary><c>git fetch</c>. Network-bound, so a long timeout; never prompts.</summary>
    public static Task<GitOpResult> FetchAsync(string folderPath) =>
        RunOpAsync(folderPath, new[] { "fetch" }, timeoutMs: 60_000);

    /// <summary>
    /// <c>git pull --ff-only --no-rebase</c>: moves the branch only when that loses nothing.
    /// <c>--no-rebase</c> is belt-and-braces against a user's <c>pull.rebase=true</c> turning a
    /// one-click "pull" into a rebase of their local commits. git 2.54 already lets
    /// <c>--ff-only</c> win (the diverged-branch test passes without the flag), but how the two
    /// interact has changed across git versions, and the flag costs nothing. If the branch has diverged
    /// or a local edit would be overwritten, git refuses and says why — we report that rather
    /// than stashing or merging on the user's behalf.
    /// </summary>
    public static Task<GitOpResult> PullFastForwardAsync(string folderPath) =>
        RunOpAsync(folderPath, new[] { "pull", "--ff-only", "--no-rebase" }, timeoutMs: 120_000);

    /// <summary>
    /// <c>git switch --no-guess -- &lt;branch&gt;</c>. The name comes from the repo
    /// (<see cref="ListBranchesAsync"/>), and a ref can legally be called <c>-x</c>: <c>--</c>
    /// keeps it a branch name rather than an option (verified against git 2.54 with a branch
    /// literally named <c>-x</c>). <c>--no-guess</c> stops git from silently creating a
    /// tracking branch when the name only exists on a remote.
    /// </summary>
    public static Task<GitOpResult> SwitchBranchAsync(string folderPath, string branch) =>
        RunOpAsync(folderPath, SwitchArgs(branch), timeoutMs: 30_000);

    internal static string[] SwitchArgs(string branch) => new[] { "switch", "--no-guess", "--", branch };

    /// <summary>Human-readable <c>git status</c> for the git panel.</summary>
    public static Task<GitOpResult> StatusTextAsync(string folderPath) =>
        RunOpAsync(folderPath, new[] { "-c", "color.status=false", "status" }, timeoutMs: 10_000, readOnly: true);

    /// <summary>
    /// <c>git worktree remove -- &lt;path&gt;</c>, deliberately WITHOUT <c>--force</c>: git then
    /// refuses a worktree with modified or untracked files, and a locked one, which is the
    /// guard that makes this safe to offer in a menu. Ignored files (build output) are deleted.
    /// </summary>
    /// <param name="runFrom">
    /// A folder of the same repo OUTSIDE the worktree being removed — the main worktree. On
    /// Windows a process whose cwd is inside the folder locks it, and git then fails half-way
    /// with "failed to delete … Permission denied" (verified against git 2.54 by running the
    /// remove from inside the worktree itself). The same is true of every terminal open there,
    /// which is why the caller closes those sessions first.
    /// </param>
    public static Task<GitOpResult> RemoveWorktreeAsync(string runFrom, string worktreePath) =>
        RunOpAsync(runFrom, RemoveWorktreeArgs(worktreePath), timeoutMs: 60_000);

    internal static string[] RemoveWorktreeArgs(string worktreePath) =>
        new[] { "worktree", "remove", "--", worktreePath };

    /// <summary>
    /// <c>git branch -d -- &lt;branch&gt;</c> — lower-case <c>-d</c>, so git refuses a branch
    /// that is not fully merged. <c>--</c> because the name comes from the repo.
    /// </summary>
    public static Task<GitOpResult> DeleteMergedBranchAsync(string runFrom, string branch) =>
        RunOpAsync(runFrom, DeleteMergedBranchArgs(branch), timeoutMs: 30_000);

    internal static string[] DeleteMergedBranchArgs(string branch) =>
        new[] { "branch", "-d", "--", branch };

    /// <summary>
    /// <c>git worktree prune</c>: forgets worktrees whose folder no longer exists. Only ever
    /// removes git's own bookkeeping under <c>.git/worktrees</c>, never a working tree.
    /// </summary>
    public static Task<GitOpResult> PruneWorktreesAsync(string folderPath) =>
        RunOpAsync(folderPath, new[] { "worktree", "prune" }, timeoutMs: 30_000);

    private static Task<GitOpResult> RunOpAsync(
        string folderPath, string[] args, int timeoutMs, bool readOnly = false) =>
        // Task.Run so the Directory.Exists below can't run on the caller's thread: these are
        // called from the UI thread, and for a WSL session the folder is a \\wsl$ path, where
        // even an existence check can block on the distro. See "Never spawn a process on the
        // UI thread" in CLAUDE.md.
        Task.Run(() => RunOpCoreAsync(folderPath, args, timeoutMs, readOnly));

    private static async Task<GitOpResult> RunOpCoreAsync(
        string folderPath, string[] args, int timeoutMs, bool readOnly)
    {
        if (string.IsNullOrWhiteSpace(folderPath) || !System.IO.Directory.Exists(folderPath))
            return new GitOpResult(false, "Folder does not exist.", TimedOut: false);
        try
        {
            var (stdout, stderr, exit) = await RunGitFullAsync(folderPath, args, timeoutMs, readOnly)
                .ConfigureAwait(false);
            bool timedOut = exit == TimedOutExit;
            string text = timedOut
                ? $"git {args.First(a => !a.StartsWith('-') && !a.Contains('='))} did not finish within " +
                  $"{timeoutMs / 1000}s and was stopped. If the remote needs a password or key " +
                  "passphrase, run the command in a terminal once."
                : CombineOutput(stdout, stderr);
            return new GitOpResult(exit == 0, text, timedOut);
        }
        catch (Exception ex) { return new GitOpResult(false, ex.Message, TimedOut: false); }
    }

    /// <summary>git writes progress and most of its prose to stderr, even on success.</summary>
    internal static string CombineOutput(string stdout, string stderr) =>
        string.Join("\n", new[] { stdout, stderr }
            .Select(s => (s ?? "").Replace("\r\n", "\n").Trim())
            .Where(s => s.Length > 0));

    /// <summary>
    /// One line for a toast after a successful pull: "Already up to date." or the diffstat
    /// summary ("3 files changed, 10 insertions(+)").
    /// </summary>
    internal static string SummarizePull(string output)
    {
        var lines = (output ?? "").Replace("\r", "").Split('\n')
            .Select(l => l.Trim()).Where(l => l.Length > 0).ToList();
        if (lines.Any(l => l.StartsWith("Already up to date", StringComparison.OrdinalIgnoreCase)))
            return "Already up to date.";
        var stat = lines.LastOrDefault(l => Regex.IsMatch(l, @"^\d+ files? changed"));
        if (stat != null) return $"Fast-forwarded: {stat}";
        return lines.Any(l => l.Equals("Fast-forward", StringComparison.OrdinalIgnoreCase))
            ? "Fast-forwarded." : "Pull finished.";
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
            string? commonDir = await RunGitAsync(folderPath, "rev-parse", "--git-common-dir").ConfigureAwait(false);
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
            string? raw = await RunGitAsync(folderPath, "worktree", "list", "--porcelain").ConfigureAwait(false);
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
            string? raw = await RunGitAsync(folderPath, "for-each-ref", "--format=%(refname:short)", "refs/heads").ConfigureAwait(false);
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

        // argv, not an interpolated string. branchOrRef comes from ListBranchesAsync — i.e.
        // from the cloned repo — and git ref names legally contain `$ ( ) ; & |` and
        // backticks. Interpolated into a command line that reached a shell, that was remote
        // code execution from opening a hostile repo; interpolated into the local path it
        // still injected extra git argv via a `"`.
        // `--` terminates option parsing. git uses permuting parse_options, so without it a
        // ref legitimately named `--force` or `--detach` sitting in refs/heads is consumed
        // as an option rather than as the commit-ish. Not execution, but it is a repo
        // deciding which git flags we run.
        string[] args = createBranch
            ? new[] { "worktree", "add", "-b", branchOrRef, "--", targetPath }
            : new[] { "worktree", "add", "--", targetPath, branchOrRef };

        var (output, stderr, exit) = await RunGitFullAsync(repoRoot, args, timeoutMs: 30_000).ConfigureAwait(false);
        if (exit == 0) return (true, "");
        string err = string.IsNullOrWhiteSpace(stderr)
            ? (string.IsNullOrWhiteSpace(output) ? "git worktree add failed." : output)
            : stderr;
        return (false, err.Trim());
    }

    private static async Task<string?> RunGitAsync(string workingDir, params string[] args)
    {
        var (stdout, _, exit) = await RunGitFullAsync(workingDir, args, timeoutMs: 3000).ConfigureAwait(false);
        return exit == 0 ? stdout : null;
    }

    /// <summary>Exit code reported when a git run was killed for exceeding its timeout.</summary>
    internal const int TimedOutExit = -2;

    /// <summary>
    /// Environment for every git we spawn.
    ///
    /// <c>GIT_TERMINAL_PROMPT=0</c>: git runs with no console, so a credential prompt for an
    /// HTTPS remote would wait for input that can never arrive until the timeout kills it. With
    /// it, git fails at once with "terminal prompts disabled", which we can report. (A GUI
    /// credential helper such as Git Credential Manager still works — it doesn't use the
    /// terminal.)
    ///
    /// <c>GIT_OPTIONAL_LOCKS=0</c> for read-only calls: see <see cref="GetStatusAsync"/>.
    /// </summary>
    internal static IReadOnlyList<KeyValuePair<string, string>> GitEnvironment(bool readOnly)
    {
        var env = new List<KeyValuePair<string, string>> { new("GIT_TERMINAL_PROMPT", "0") };
        if (readOnly) env.Add(new("GIT_OPTIONAL_LOCKS", "0"));
        return env;
    }

    /// <summary>
    /// WSLENV with <paramref name="names"/> added. Windows environment variables only cross
    /// into a distro when WSLENV lists them, so without this the git inside WSL never saw
    /// GIT_TERMINAL_PROMPT. Existing entries (and their <c>/p</c>-style flags) are kept.
    /// </summary>
    internal static string MergeWslEnv(string? existing, IEnumerable<string> names)
    {
        var list = (existing ?? "").Split(':', StringSplitOptions.RemoveEmptyEntries).ToList();
        foreach (string n in names)
            if (!list.Any(e => e.Split('/')[0] == n)) list.Add(n);
        return string.Join(":", list);
    }

    private static void ApplyEnvironment(ProcessStartInfo psi, bool readOnly, bool forWsl)
    {
        var env = GitEnvironment(readOnly);
        foreach (var (k, v) in env) psi.Environment[k] = v;
        if (forWsl)
        {
            psi.Environment.TryGetValue("WSLENV", out var existing);
            psi.Environment["WSLENV"] = MergeWslEnv(existing, env.Select(e => e.Key));
        }
    }

    /// <summary>
    /// Joins argv into a Windows command line, quoting each element with the MSVCRT rules.
    ///
    /// Every git invocation is built this way rather than by string interpolation. The old
    /// shape — <c>$"-C \"{workingDir}\" {arguments}"</c> — meant any caller interpolating a
    /// branch name or path into <c>arguments</c> was one <c>"</c> away from injecting extra
    /// argv, and on the WSL path (which passed through a login shell) one <c>$(…)</c> away
    /// from arbitrary command execution. Argv in, argv out: there is no string for a
    /// metacharacter to be a metacharacter in.
    /// </summary>
    private static string JoinArgv(IEnumerable<string> args) =>
        string.Join(" ", args.Select(a => Models.ShellSession.QuoteForCmd(a)));

    /// <summary>
    /// Builds the wsl.exe command line for one git invocation inside a distro. Extracted so
    /// it can be round-tripped through the real Win32 tokenizer in tests — see
    /// <c>GitServiceInjectionTests</c>.
    /// </summary>
    /// <summary>
    /// ASCII Record Separator, printed by the login shell immediately before it execs git.
    /// Everything up to and including it is profile noise, not git output.
    /// </summary>
    internal const string WslOutputSentinel = "\u001eCSM-GIT\u001e";

    /// <summary>
    /// The fixed script handed to <c>sh -lc</c>. Contains no interpolated data — that is
    /// the entire basis of the injection safety, so it is a constant, not a format string.
    ///
    /// The sentinel exists because <c>-l</c> sources <c>/etc/profile</c> and
    /// <c>~/.profile</c> first, and a profile that echoes prepends its output to git's.
    /// Callers parse that output: a banner would become the "branch name", and — worse —
    /// would make <c>status --porcelain</c> non-empty, pinning every WSL repo to "dirty"
    /// forever. The old <c>--</c> form had the identical exposure; WSL sessions are new in
    /// this release, so this would have been its debut rather than a regression.
    /// </summary>
    internal const string WslGitScript = "printf '\\036CSM-GIT\\036'; exec \"$0\" \"$@\"";

    /// <summary>
    /// Strips profile output emitted before the sentinel. No sentinel means the command
    /// never reached the exec (wsl.exe itself failed, distro missing), so the text is an
    /// error message and is returned untouched for the caller to log.
    /// </summary>
    internal static string StripWslProfileNoise(string stdout)
    {
        if (string.IsNullOrEmpty(stdout)) return stdout;

        // First occurrence: the marker is distinctive enough that a collision from either
        // the profile or git's own output is implausible, so the first one is ours.
        int i = stdout.IndexOf(WslOutputSentinel, StringComparison.Ordinal);
        return i < 0 ? stdout : stdout[(i + WslOutputSentinel.Length)..];
    }

    internal static string BuildWslGitCommandLine(
        string distro, string cwd, IReadOnlyList<string> gitArgs)
    {
        // `-e sh -lc 'exec "$0" "$@"' git …` rather than a bare `-e git`.
        //
        // Plain `-e git` is injection-safe but changed behaviour: it execs git directly, so
        // the login shell never runs and PATH is the bare default. Anyone whose git comes
        // from nix, linuxbrew or asdf — i.e. PATH set in a shell profile — would silently
        // lose WSL git entirely, and the symptom would be "not a git repo" rather than
        // anything pointing at PATH. The old `--` form ran a login shell, so that PATH was
        // previously present.
        //
        // This restores it without reopening the hole: the script text is a fixed literal
        // and every untrusted value arrives as a positional parameter. `"$0"`/`"$@"` expand
        // to those parameters verbatim — the shell does not re-parse them — so a `$(…)` in
        // a branch name is data, not code.
        var argv = new List<string>
        {
            "-d", distro, "-e", "sh", "-lc", WslGitScript, "git", "-C", cwd
        };
        foreach (string a in gitArgs) argv.Add(TranslateUncArgToLinux(a, distro));
        return JoinArgv(argv);
    }

    /// <summary>Builds the local git command line. Extracted for the same reason.</summary>
    internal static string BuildLocalGitCommandLine(
        string workingDir, IReadOnlyList<string> gitArgs) =>
        JoinArgv(new[] { "-C", workingDir }.Concat(gitArgs));

    /// <summary>
    /// Runs one git command. The body is wrapped in <see cref="Task.Run"/> deliberately —
    /// see the note on <c>Process.Start</c> below (issue #70).
    /// </summary>
    private static Task<(string stdout, string stderr, int exit)> RunGitFullAsync(
        string workingDir, IReadOnlyList<string> args, int timeoutMs, bool readOnly = false)
        => Task.Run(() => RunGitCoreAsync(workingDir, args, timeoutMs, readOnly));

    private static async Task<(string stdout, string stderr, int exit)> RunGitCoreAsync(
        string workingDir, IReadOnlyList<string> args, int timeoutMs, bool readOnly)
    {
        // WSL working folders (\\wsl$\<distro>\…) get routed through wsl.exe so git
        // runs inside the distro. Git for Windows trips on WSL UNCs (dubious-ownership
        // checks, .git symlink quirks) and reports "not a git repo" for valid repos.
        var (wslDistro, linuxPath) = TryParseWslUnc(workingDir);
        if (wslDistro != null)
            return await RunGitInWslAsync(wslDistro, linuxPath, args, timeoutMs, readOnly).ConfigureAwait(false);

        var psi = new ProcessStartInfo("git")
        {
            Arguments = BuildLocalGitCommandLine(workingDir, args),
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true
        };
        ApplyEnvironment(psi, readOnly, forWsl: false);

        // Process.Start is synchronous and sits BEFORE this method's first await, so without
        // the Task.Run above it ran on whatever thread called in. Every caller chain here
        // starts in SessionViewModel's constructor on the UI thread, so its
        // SynchronizationContext was captured, every continuation returned there, and
        // process creation landed on the UI thread — 94 spawns per 10s poll at 47 sessions.
        //
        // Measured on an idle machine: Process.Start alone is ~15ms (9-20ms), so that was
        // ~1.4s of hard UI-thread block per cycle before contention, which is what produced
        // the multi-second typing freezes traced in issue #70. Note the cost is almost
        // entirely process creation, not git: `git --version` measures 42ms against
        // `branch --show-current` at 41ms, so there is no faster query to switch to.
        //
        // Every await below is ConfigureAwait(false) so no continuation can climb back onto
        // the UI thread even if a future caller invokes this from there directly.
        bool onUi = Diagnostics.DiagnosticTrace.OnUiThread;
        long spawnStart = Environment.TickCount64;

        using var process = Process.Start(psi);

        if (Diagnostics.DiagnosticTrace.Enabled)
            Diagnostics.DiagnosticTrace.Write("DEBUG-tt", "git",
                $"GIT-SPAWN on-ui={onUi} spawn={Environment.TickCount64 - spawnStart}ms " +
                $"args='{string.Join(" ", args)}'");

        if (process is null) return ("", "", -1);

        var outTask = process.StandardOutput.ReadToEndAsync();
        var errTask = process.StandardError.ReadToEndAsync();
        var bothTask = Task.WhenAll(outTask, errTask);
        var completed = await Task.WhenAny(bothTask, Task.Delay(timeoutMs)).ConfigureAwait(false);

        // The whole tree: a fetch's real work happens in children (git-remote-https, ssh),
        // and killing only git.exe left those running.
        bool timedOut = completed != bothTask;
        if (timedOut)
        {
            try { process.Kill(entireProcessTree: true); } catch { }
        }
        try { await process.WaitForExitAsync().ConfigureAwait(false); } catch { }

        string stdout = outTask.IsCompletedSuccessfully ? outTask.Result : "";
        string stderr = errTask.IsCompletedSuccessfully ? errTask.Result : "";
        if (timedOut) return (stdout, stderr, TimedOutExit);
        return (stdout, stderr, process.HasExited ? process.ExitCode : -1);
    }

    /// <summary>
    /// Runs <c>wsl.exe -d &lt;distro&gt; -e sh -lc 'exec "$0" "$@"' git -C &lt;linuxPath&gt; …</c> — <c>-e</c>, never <c>--</c>. See BuildWslGitCommandLine.
    /// Translates any WSL UNC paths in <paramref name="arguments"/> to Linux form
    /// before invocation (so things like <c>worktree add "\\wsl$\Ubuntu\…"</c> reach
    /// git as a normal Linux path), and translates absolute Linux paths in stdout
    /// back to UNC form so callers receive Windows-shaped paths.
    /// </summary>
    private static async Task<(string stdout, string stderr, int exit)> RunGitInWslAsync(
        string distro, string linuxPath, IReadOnlyList<string> gitArgs, int timeoutMs, bool readOnly)
    {
        string cwd = string.IsNullOrEmpty(linuxPath) ? "/" : linuxPath;

        // `-e`, NOT `--`. This is the same rule ShellSession.BuildWslArgs documents and for
        // the same reason, but here it is a security boundary rather than a correctness one:
        // `wsl.exe … -- <tail>` runs the tail through the distro's DEFAULT LOGIN SHELL, so a
        // `$(…)`, backtick, `;` or `|` anywhere in the working folder or in a git argument
        // executed inside the distro. QuoteForCmd is MSVCRT *argv* quoting and does not — and
        // cannot — neutralise shell metacharacters.
        //
        // Reachable before this fix: a branch name from a cloned repo (git permits `$ ( ) ; &`
        // and backticks in refs) flowing into `worktree add -b`, and a working-folder path
        // reached automatically by the git poll. `-e` executes git directly with no shell
        // pass, which makes argv quoting both correct and sufficient.
        //
        // It also fixes a plain bug: `--format=%(refname:short)` was a bash syntax error once
        // the login shell saw it, so ListBranchesAsync could never have worked under WSL.
        string args = BuildWslGitCommandLine(distro, cwd, gitArgs);

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
        ApplyEnvironment(psi, readOnly, forWsl: true);

        using var process = Process.Start(psi);
        if (process is null) return ("", "", -1);

        var outTask = process.StandardOutput.ReadToEndAsync();
        var errTask = process.StandardError.ReadToEndAsync();
        var bothTask = Task.WhenAll(outTask, errTask);
        var completed = await Task.WhenAny(bothTask, Task.Delay(timeoutMs)).ConfigureAwait(false);
        bool timedOut = completed != bothTask;
        if (timedOut) { try { process.Kill(entireProcessTree: true); } catch { } }
        try { await process.WaitForExitAsync().ConfigureAwait(false); } catch { }

        string stdout = outTask.IsCompletedSuccessfully ? outTask.Result : "";
        string stderr = errTask.IsCompletedSuccessfully ? errTask.Result : "";
        // Drop anything the login shell's profile wrote before git started, then map Linux
        // paths back to UNC. Order matters: the sentinel must go before path translation,
        // or a banner containing a slash would be rewritten as if it were a git path.
        stdout = TranslateLinuxPathsToUnc(StripWslProfileNoise(stdout), distro);
        if (timedOut) return (stdout, stderr, TimedOutExit);
        return (stdout, stderr, process.HasExited ? process.ExitCode : -1);
    }

    /// <summary>
    /// Detects a <c>\\wsl$\&lt;distro&gt;\…</c> or <c>\\wsl.localhost\&lt;distro&gt;\…</c>
    /// path and splits it into (distro, linux-path). Returns (null, "") otherwise.
    /// Delegates to <see cref="WslDiscoveryService.TryParseUncPath"/> — the single
    /// parser for this shape shared with NewSessionDialog.
    /// </summary>
    internal static (string? distro, string linuxPath) TryParseWslUnc(string path)
        => WslDiscoveryService.TryParseUncPath(path);


    /// <summary>
    /// Replaces absolute Linux paths in <paramref name="text"/> (typically git stdout)
    /// with <c>\\wsl$\&lt;distro&gt;\…</c> equivalents so callers see Windows-shaped
    /// paths. Conservative — only matches tokens at start-of-line or after whitespace
    /// to avoid mangling text that happens to contain a slash.
    /// </summary>
    /// <summary>
    /// Translates ONE argument: a <c>\\wsl$\&lt;distro&gt;\…</c> path becomes its Linux
    /// equivalent, anything else is returned unchanged. A UNC belonging to a *different*
    /// distro is passed through untouched, so the caller sees the eventual "no such
    /// directory" rather than silently aiming at the wrong tree.
    ///
    /// Replaced a whole-command-line regex that needed two passes (quoted and unquoted)
    /// purely because arguments had been pre-joined into a string. Now that argv stays
    /// argv, an argument either is a UNC or is not — no quote handling, and no way for a
    /// path containing spaces to end up half-translated.
    /// </summary>
    internal static string TranslateUncArgToLinux(string argument, string distro)
    {
        if (string.IsNullOrEmpty(argument)) return argument;

        var (parsedDistro, linux) = TryParseWslUnc(argument);
        if (parsedDistro == null) return argument;
        if (!string.Equals(parsedDistro, distro, StringComparison.OrdinalIgnoreCase))
            return argument;

        return string.IsNullOrEmpty(linux) ? "/" : linux;
    }

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

/// <summary>One changed path from <c>git status</c>. <see cref="Code"/> is the two-letter XY
/// code ("M ", " M", "??", "UU" …); a rename's path reads "old → new".</summary>
public sealed record GitFileChange(string Code, string Path);

/// <summary>A repo snapshot from one <c>git status --porcelain=v2 --branch</c>.</summary>
/// <param name="Branch">Null when detached.</param>
/// <param name="Upstream">e.g. "origin/main"; null when the branch tracks nothing.</param>
/// <param name="Ahead">Null when there is no upstream (or it is gone).</param>
public sealed record GitRepoStatus(
    string? Branch, bool IsDetached, string? Upstream, int? Ahead, int? Behind,
    IReadOnlyList<GitFileChange> Changes)
{
    public bool IsDirty => Changes.Count > 0;
}

/// <summary>Outcome of a user-initiated git operation; <see cref="Output"/> is stdout + stderr.</summary>
public sealed record GitOpResult(bool Ok, string Output, bool TimedOut);

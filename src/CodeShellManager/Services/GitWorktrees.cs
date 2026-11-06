using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
using WorktreeInfo = CodeShellManager.Services.GitService.WorktreeInfo;

namespace CodeShellManager.Services;

/// <summary>
/// WPF-free worktree rules behind the git menu's worktree items. See "Worktrees in the git
/// menu" in CLAUDE.md.
/// </summary>
public static class GitWorktrees
{
    /// <summary>
    /// Forward slashes, no trailing separator — the form git prints and compares. The two
    /// spellings of a WSL share (<c>\\wsl.localhost\</c> and <c>\\wsl$\</c>) are folded to
    /// one: GitService always reports <c>\\wsl$</c>, and a session opened as
    /// <c>\\wsl.localhost</c> otherwise matched no worktree at all.
    /// </summary>
    public static string Normalize(string path)
    {
        if (string.IsNullOrEmpty(path)) return "";
        string n = path.Replace('\\', '/').TrimEnd('/');
        const string localhost = "//wsl.localhost/";
        if (n.StartsWith(localhost, StringComparison.OrdinalIgnoreCase)) n = "//wsl$/" + n[localhost.Length..];
        return n;
    }

    /// <summary>
    /// The comparable form of a path and how to compare it. Windows paths are
    /// case-insensitive; inside a WSL distro they are NOT — <c>/home/u/wt</c> and
    /// <c>/home/u/WT</c> are different folders, and treating them as one would close (and
    /// delete the sleeping) sessions of the wrong worktree. The distro name itself is
    /// case-insensitive, so only that segment is folded.
    /// </summary>
    private static (string key, StringComparison cmp) Key(string path)
    {
        string n = Normalize(path);
        const string wsl = "//wsl$/";
        if (!n.StartsWith(wsl, StringComparison.OrdinalIgnoreCase)) return (n, StringComparison.OrdinalIgnoreCase);
        int slash = n.IndexOf('/', wsl.Length);
        string distro = slash < 0 ? n[wsl.Length..] : n[wsl.Length..slash];
        string rest = slash < 0 ? "" : n[slash..];
        return (wsl + distro.ToLowerInvariant() + rest, StringComparison.Ordinal);
    }

    /// <summary>Same folder, by <see cref="Key"/> — case rules per Windows / WSL.</summary>
    public static bool SamePath(string a, string b)
    {
        var (ka, cmp) = Key(a);
        var (kb, _) = Key(b);
        return ka.Length > 0 && ka.Equals(kb, cmp);
    }

    /// <summary>
    /// The folder's real path, in <see cref="Normalize"/>d form: 8.3 short names expanded,
    /// junctions and symlinks followed. Comparing raw strings is not enough — git reports
    /// <c>C:/Users/MortenAsloOestergaar/…</c> for a worktree whose session was opened as
    /// <c>C:\Users\MORTEN~1\…</c> (an 8.3 %TEMP%; the end-to-end test of "Remove this
    /// worktree" failed on exactly that), and a folder reached through a junction has two
    /// spellings too. Touches the disk — call it off the UI thread. A path that doesn't
    /// exist, or a \\wsl$ / network path, is only normalized.
    /// </summary>
    public static string Canonical(string path)
    {
        if (string.IsNullOrEmpty(path)) return "";
        try
        {
            string full = Path.GetFullPath(path);
            if (full.StartsWith(@"\\", StringComparison.Ordinal) || !Directory.Exists(full))
                return Normalize(full);
            using var h = CreateFileW(full, 0, FileShareAll, IntPtr.Zero, OpenExisting, BackupSemantics, IntPtr.Zero);
            if (h.IsInvalid) return Normalize(full);
            var buf = new char[1024];
            uint n = GetFinalPathNameByHandleW(h, buf, (uint)buf.Length, 0);
            if (n == 0 || n >= buf.Length) return Normalize(full);
            string final = new string(buf, 0, (int)n);
            if (final.StartsWith(@"\\?\UNC\", StringComparison.Ordinal)) final = @"\\" + final[8..];
            else if (final.StartsWith(@"\\?\", StringComparison.Ordinal)) final = final[4..];
            return Normalize(final);
        }
        catch { return Normalize(path); }
    }

    /// <summary><see cref="Canonical"/> applied to every listed worktree's path.</summary>
    public static IReadOnlyList<WorktreeInfo> Canonicalize(IReadOnlyList<WorktreeInfo> worktrees) =>
        worktrees.Select(w => w with { Path = w.IsPrunable ? Normalize(w.Path) : Canonical(w.Path) }).ToList();

    private const uint FileShareAll = 0x7, OpenExisting = 3, BackupSemantics = 0x02000000;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFinalPathNameByHandleW(SafeFileHandle h, [Out] char[] buf, uint len, uint flags);

    /// <summary>
    /// The key a git operation's busy slot is held under: the repo root (the MAIN working
    /// tree, so every worktree of one repository shares it) when known, else the folder.
    /// Case-folded per <see cref="Key"/>'s rules so one HashSet with ordinal comparison can
    /// hold Windows and WSL keys side by side.
    /// </summary>
    public static string BusyKey(string? repoRoot, string? workingFolder)
    {
        var (k, cmp) = Key(string.IsNullOrEmpty(repoRoot) ? workingFolder ?? "" : repoRoot);
        return cmp == StringComparison.OrdinalIgnoreCase ? k.ToLowerInvariant() : k;
    }

    /// <summary>
    /// True when <paramref name="folder"/> is <paramref name="root"/> or below it. A plain
    /// prefix test is wrong: <c>C:/repo-wt</c> starts with <c>C:/repo</c> but is not inside it.
    /// </summary>
    public static bool IsInside(string folder, string root)
    {
        var (f, cmp) = Key(folder);
        var (r, _) = Key(root);
        if (f.Length == 0 || r.Length == 0) return false;
        return f.Equals(r, cmp) || f.StartsWith(r + "/", cmp);
    }

    /// <summary>
    /// The worktree containing <paramref name="folder"/> — the deepest match, since a worktree
    /// can legally live inside another one's folder.
    /// </summary>
    public static WorktreeInfo? Containing(IReadOnlyList<WorktreeInfo> worktrees, string folder) =>
        worktrees.Where(w => !w.IsBare && IsInside(folder, w.Path))
                 .OrderByDescending(w => Normalize(w.Path).Length)
                 .FirstOrDefault();

    /// <summary>
    /// git lists the main worktree first; every other entry is a linked one. A bare repo's
    /// entry is "main" too but has no working tree, so it is never offered for removal.
    /// </summary>
    public static bool IsMain(IReadOnlyList<WorktreeInfo> worktrees, WorktreeInfo wt) =>
        worktrees.Count > 0 && SamePath(worktrees[0].Path, wt.Path);

    /// <summary>
    /// Branches checked out in a worktree OTHER than <paramref name="current"/>, mapped to that
    /// worktree. git allows a branch in only one worktree at a time, so switching to one of
    /// these always fails ("already used by worktree at …"); the menu offers to go there instead.
    /// </summary>
    public static IReadOnlyDictionary<string, WorktreeInfo> BranchesElsewhere(
        IReadOnlyList<WorktreeInfo> worktrees, WorktreeInfo? current)
    {
        var map = new Dictionary<string, WorktreeInfo>(StringComparer.Ordinal);
        foreach (var w in worktrees)
        {
            if (w.IsBare || string.IsNullOrEmpty(w.Branch)) continue;
            if (current != null && SamePath(w.Path, current.Path))
                continue;
            map.TryAdd(w.Branch, w);
        }
        return map;
    }

    /// <summary>
    /// Other worktrees that live INSIDE <paramref name="target"/>'s folder. Removing the target
    /// would delete them too: git's clean check does not look inside an ignored folder, and a
    /// nested worktree usually sits in one (<c>.worktrees/</c> in .gitignore). Reproduced with
    /// git 2.54 — `worktree remove` on the outer one exited 0 and deleted the inner one along
    /// with its uncommitted file. So the menu refuses while any are listed.
    /// </summary>
    public static IReadOnlyList<WorktreeInfo> NestedInside(IReadOnlyList<WorktreeInfo> worktrees, WorktreeInfo target) =>
        worktrees.Where(w => !w.IsBare && !SamePath(w.Path, target.Path) && IsInside(w.Path, target.Path)).ToList();

    /// <summary>
    /// Folders BELOW <paramref name="root"/> that are a git repository, worktree or submodule
    /// of their own (they hold a <c>.git</c> folder or file). <see cref="NestedInside"/> only
    /// sees worktrees registered with the same repo; an independent clone, or another repo's
    /// worktree, inside an ignored folder would be deleted by `git worktree remove` just the
    /// same (second-round pre-release review). Junctions and symlinks are not followed, and
    /// <c>.git</c> folders are not descended into. Returns null when more than
    /// <paramref name="maxDirs"/> folders had to be visited — the caller cannot vouch for the
    /// tree then and must refuse. Disk I/O: call off the UI thread.
    /// </summary>
    public static IReadOnlyList<string>? NestedRepositories(string root, int maxDirs = 500_000)
    {
        var found = new List<string>();
        var options = new EnumerationOptions
        {
            RecurseSubdirectories = false,
            IgnoreInaccessible = true,
            AttributesToSkip = FileAttributes.ReparsePoint,
        };
        var pending = new Stack<string>();
        pending.Push(root);
        int visited = 0;
        while (pending.Count > 0)
        {
            string dir = pending.Pop();
            bool isRoot = ReferenceEquals(dir, root);
            if (!isRoot && File.Exists(Path.Combine(dir, ".git")))
            {
                found.Add(dir);                       // linked worktree or submodule
                continue;
            }
            foreach (string sub in Directory.EnumerateDirectories(dir, "*", options))
            {
                if (++visited > maxDirs) return null;
                if (Path.GetFileName(sub).Equals(".git", StringComparison.OrdinalIgnoreCase))
                {
                    if (!isRoot) found.Add(dir);      // a repository of its own
                    continue;                         // never walk into .git
                }
                pending.Push(sub);
            }
        }
        return found;
    }

    /// <summary>The worktree's folder name — what the menus show.</summary>
    public static string Name(string path)
    {
        string n = Normalize(path);
        int i = n.LastIndexOf('/');
        return i >= 0 ? n[(i + 1)..] : n;
    }

    /// <summary>
    /// "wt-feature  ⎇ feature ● ↑1 ↓2", "wt-old — folder missing", "… 🔒". The status is
    /// optional: it is fetched lazily and may not be there (or may have failed).
    /// </summary>
    public static string Label(WorktreeInfo w, GitRepoStatus? status)
    {
        string name = Name(w.Path);
        if (w.IsPrunable) return $"{name} — folder missing";
        string branch = w.IsDetached || string.IsNullOrEmpty(w.Branch) ? "detached" : w.Branch;
        string label = $"{name}  ⎇ {branch}";
        if (status is { IsDirty: true }) label += " ●";
        string sync = GitStatusText.Sync(status?.Ahead, status?.Behind);
        if (sync.Length > 0) label += " " + sync;
        if (w.IsLocked) label += " 🔒";
        return label;
    }

    /// <summary>
    /// For the chip and menu header: the main repo's name when <paramref name="folder"/> is
    /// in a LINKED worktree, else null. Pure file reads, no git spawn — but it does touch the
    /// disk (and a \\wsl$ path), so callers run it off the UI thread.
    /// </summary>
    /// <remarks>
    /// A linked worktree's <c>.git</c> is a FILE: <c>gitdir: &lt;common&gt;/worktrees/&lt;name&gt;</c>.
    /// A submodule's is a file too (<c>gitdir: …/.git/modules/&lt;name&gt;</c>), so the file alone
    /// is not enough — the <c>/worktrees/</c> segment is what distinguishes them.
    /// </remarks>
    public static string? LinkedWorktreeOf(string folder, string? repoRoot)
    {
        try
        {
            for (var dir = new DirectoryInfo(folder); dir != null; dir = dir.Parent)
            {
                string dotGit = Path.Combine(dir.FullName, ".git");
                if (Directory.Exists(dotGit)) return null;          // main working tree
                if (!File.Exists(dotGit)) continue;
                string text = File.ReadAllText(dotGit).Trim();
                if (!text.StartsWith("gitdir:", StringComparison.OrdinalIgnoreCase)) return null;
                string gitdir = Normalize(text["gitdir:".Length..].Trim());
                if (!gitdir.Contains("/worktrees/", StringComparison.OrdinalIgnoreCase)) return null;
                // repoRoot is the main working tree (GetRepoRootAsync strips the ".git").
                if (!string.IsNullOrEmpty(repoRoot)) return Name(repoRoot);
                // Fall back to the folder holding <common>/.git.
                int i = gitdir.IndexOf("/.git/worktrees/", StringComparison.OrdinalIgnoreCase);
                return i > 0 ? Name(gitdir[..i]) : "another checkout";
            }
        }
        catch { }
        return null;
    }
}

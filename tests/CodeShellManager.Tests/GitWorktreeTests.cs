using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using CodeShellManager.Services;
using Xunit;
using WorktreeInfo = CodeShellManager.Services.GitService.WorktreeInfo;

namespace CodeShellManager.Tests;

/// <summary>Pure rules behind the git menu's worktree items. See "Worktrees in the git menu".</summary>
public class GitWorktreeRuleTests
{
    private static WorktreeInfo Wt(string path, string? branch = null, bool bare = false,
        bool detached = false, bool locked = false, bool prunable = false) =>
        new(path, branch, bare, detached, locked, prunable);

    // ── git busy-slot key (#181) ──────────────────────────────────────────────

    [Fact]
    public void BusyKey_prefers_the_repo_root_over_the_folder()
    {
        // Two worktrees of one repo: different folders, one RepoRoot (the main working tree).
        Assert.Equal(GitWorktrees.BusyKey("C:/src/app", @"C:\src\app-wt-feature"),
                     GitWorktrees.BusyKey("C:/src/app", @"C:\src\app\sub"));
        Assert.Equal(GitWorktrees.BusyKey(null, @"C:\src\app"), GitWorktrees.BusyKey("", "C:/src/app/"));
    }

    [Fact]
    public void BusyKey_folds_spelling_and_case_on_Windows()
    {
        Assert.Equal(GitWorktrees.BusyKey(@"C:\Src\App\", null), GitWorktrees.BusyKey("c:/src/app", null));
    }

    [Fact]
    public void BusyKey_is_case_sensitive_inside_WSL_but_not_for_the_distro()
    {
        Assert.Equal(GitWorktrees.BusyKey(@"\\wsl.localhost\Ubuntu\home\u\repo", null),
                     GitWorktrees.BusyKey(@"\\wsl$\ubuntu\home\u\repo", null));
        Assert.NotEqual(GitWorktrees.BusyKey(@"\\wsl$\Ubuntu\home\u\Repo", null),
                        GitWorktrees.BusyKey(@"\\wsl$\Ubuntu\home\u\repo", null));
    }

    [Theory]
    [InlineData("C:/repo", "C:/repo", true)]
    [InlineData("C:/repo/src/app", "C:/repo", true)]
    [InlineData(@"C:\repo\src", "C:/repo", true)]          // mixed separators
    [InlineData("c:/REPO/src", "C:/repo/", true)]           // case, trailing slash
    [InlineData("C:/repo-wt", "C:/repo", false)]            // a prefix is not "inside"
    [InlineData("C:/repo2/x", "C:/repo", false)]
    [InlineData("", "C:/repo", false)]
    public void IsInside_is_a_path_test_not_a_string_prefix(string folder, string root, bool expected) =>
        Assert.Equal(expected, GitWorktrees.IsInside(folder, root));

    [Theory]
    // Inside a distro, Linux paths are case-sensitive: /home/u/WT is a different folder, and
    // matching it would close (and delete the sleeping) sessions of the wrong worktree.
    [InlineData(@"\\wsl$\Ubuntu\home\u\WT", @"\\wsl$\Ubuntu\home\u\wt", false)]
    [InlineData(@"\\wsl$\Ubuntu\home\u\wt\src", @"\\wsl$\Ubuntu\home\u\wt", true)]
    // …but the distro name is not, and the two share spellings are the same place.
    [InlineData(@"\\wsl$\ubuntu\home\u\wt", @"\\wsl$\Ubuntu\home\u\wt", true)]
    [InlineData(@"\\wsl.localhost\Ubuntu\home\u\wt", @"\\wsl$\Ubuntu\home\u\wt", true)]
    [InlineData(@"\\WSL.LOCALHOST\Ubuntu\home\u\wt\x", @"\\wsl$\Ubuntu\home\u\wt", true)]
    public void Wsl_paths_follow_linux_case_rules_and_both_share_spellings(string folder, string root, bool expected)
    {
        Assert.Equal(expected, GitWorktrees.IsInside(folder, root));
        Assert.Equal(expected && !folder.EndsWith(@"\src") && !folder.EndsWith(@"\x"), GitWorktrees.SamePath(folder, root));
    }

    [Fact]
    public void NestedInside_finds_worktrees_living_in_the_target_folder_only()
    {
        var outer = Wt("C:/w/a", "a");
        var wts = new[] { Wt("C:/w/main", "main"), outer, Wt("C:/w/a/.worktrees/b", "b"), Wt("C:/w/a-sibling", "s") };
        Assert.Equal(new[] { "b" }, GitWorktrees.NestedInside(wts, outer).Select(w => w.Branch));
        Assert.Empty(GitWorktrees.NestedInside(wts, wts[2]));
    }

    [Fact]
    public void Containing_picks_the_deepest_worktree()
    {
        // A worktree may legally live inside another one's folder.
        var wts = new[] { Wt("C:/repo", "main"), Wt("C:/repo/.wt/feat", "feat"), Wt("C:/repo-other", "x") };
        Assert.Equal("feat", GitWorktrees.Containing(wts, "C:/repo/.wt/feat/src")!.Branch);
        Assert.Equal("main", GitWorktrees.Containing(wts, "C:/repo/src")!.Branch);
        Assert.Null(GitWorktrees.Containing(wts, "D:/elsewhere"));
    }

    [Fact]
    public void Containing_ignores_the_bare_entry()
    {
        var wts = new[] { Wt("C:/repo.git", bare: true), Wt("C:/repo.git/wt", "feat") };
        Assert.Equal("feat", GitWorktrees.Containing(wts, "C:/repo.git/wt")!.Branch);
    }

    [Fact]
    public void BranchesElsewhere_maps_every_other_worktrees_branch_but_not_this_ones()
    {
        var main = Wt("C:/repo", "main");
        var wts = new[] { main, Wt("C:/wt-a", "feat-a"), Wt("C:/wt-d", detached: true), Wt("C:/b.git", "x", bare: true) };
        var map = GitWorktrees.BranchesElsewhere(wts, main);
        Assert.Equal(new[] { "feat-a" }, map.Keys);
        Assert.Equal("C:/wt-a", map["feat-a"].Path);
    }

    [Fact]
    public void IsMain_is_the_first_entry_git_lists()
    {
        var wts = new[] { Wt("C:/repo", "main"), Wt("C:/wt", "feat") };
        Assert.True(GitWorktrees.IsMain(wts, wts[0]));
        Assert.False(GitWorktrees.IsMain(wts, wts[1]));
    }

    [Fact]
    public void Label_shows_branch_state_and_problems()
    {
        var dirtyAhead = new GitRepoStatus("feat", false, "origin/feat", 1, 2,
            new[] { new GitFileChange(".M", "a.txt") });
        Assert.Equal("wt-feat  ⎇ feat ● ↑1 ↓2", GitWorktrees.Label(Wt("C:/x/wt-feat", "feat"), dirtyAhead));
        Assert.Equal("wt  ⎇ detached", GitWorktrees.Label(Wt("C:/wt", detached: true), null));
        Assert.Equal("wt  ⎇ feat 🔒", GitWorktrees.Label(Wt("C:/wt", "feat", locked: true), null));
        Assert.Equal("gone — folder missing", GitWorktrees.Label(Wt("C:/gone", "feat", prunable: true), null));
    }

    [Fact]
    public void Header_names_the_main_repo_only_in_a_linked_worktree()
    {
        Assert.EndsWith(" · worktree of CodeShellManager",
            GitStatusText.MenuHeader("feat", null, null, null, 0, "CodeShellManager"));
        Assert.DoesNotContain("worktree", GitStatusText.MenuHeader("main", null, null, null, 0));
    }

    [Theory]
    // Each message captured verbatim from git 2.54.
    [InlineData("fatal: 'feat' is already used by worktree at 'C:/tmp/wt-feat'", "another worktree")]
    [InlineData("fatal: '../wt-feat' contains modified or untracked files, use --force to delete it", "uncommitted or untracked")]
    [InlineData("fatal: cannot remove a locked working tree;\nuse 'remove -f -f' to override or unlock first", "locked")]
    [InlineData("fatal: '.' is a main working tree", "main checkout")]
    [InlineData("error: the branch 'feat' is not fully merged\nhint: If you are sure you want to delete it, run 'git branch -D feat'", "aren't merged")]
    [InlineData("error: failed to delete 'C:/tmp/wt-self': Permission denied", "files open")]
    public void Worktree_refusals_get_a_plain_hint(string gitOutput, string expectedFragment) =>
        Assert.Contains(expectedFragment, GitStatusText.FailureHint(gitOutput));

    [Fact]
    public void Remove_and_branch_delete_argv_keep_values_as_one_element_after_dashdash()
    {
        foreach (string hostile in new[] { "-x", "--force", "$(id)", "a b", "a\"b" })
        {
            Assert.Equal(new[] { "worktree", "remove", "--", hostile }, GitService.RemoveWorktreeArgs(hostile));
            Assert.Equal(new[] { "branch", "-d", "--", hostile }, GitService.DeleteMergedBranchArgs(hostile));
        }
    }
}

/// <summary>
/// The worktree operations end to end against real git: a throwaway repo with linked
/// worktrees, exercising every refusal the menu relies on.
/// </summary>
public sealed class GitWorktreeRepoTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "csm-gitwt-" + Guid.NewGuid().ToString("N")[..8]);
    private string Main => Path.Combine(_root, "main");
    private string WtPath(string name) => Path.Combine(_root, name);

    public GitWorktreeRepoTests()
    {
        Directory.CreateDirectory(Main);
        Git(Main, "init", "-q", "-b", "main");
        Git(Main, "config", "user.email", "test@example.invalid");
        Git(Main, "config", "user.name", "test");
        Git(Main, "config", "commit.gpgsign", "false");
        File.WriteAllText(Path.Combine(Main, "a.txt"), "one\n");
        Git(Main, "add", "a.txt");
        Git(Main, "commit", "-q", "-m", "one");
    }

    private void AddWorktree(string name, string branch) =>
        Git(Main, "worktree", "add", "-q", "-b", branch, "--", WtPath(name));

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
    public async Task Two_panes_of_one_repo_claim_the_same_busy_slot_before_RepoRoot_lands()
    {
        // #181: the busy slot used to be keyed by the folder until the poll set RepoRoot, so a
        // pane in the main checkout and one in a linked worktree held different keys.
        AddWorktree("wt-feat", "feat");
        string wt = WtPath("wt-feat");
        Assert.NotEqual(GitWorktrees.BusyKey(null, Main), GitWorktrees.BusyKey(null, wt));

        var a = new CodeShellManager.ViewModels.SessionViewModel(
            new CodeShellManager.Models.ShellSession { Id = "a", WorkingFolder = Main, Command = "cmd" });
        var b = new CodeShellManager.ViewModels.SessionViewModel(
            new CodeShellManager.Models.ShellSession { Id = "b", WorkingFolder = wt, Command = "cmd" });
        try
        {
            // What MainWindow.ResolveGitRepoKeyAsync does before claiming the slot.
            string ka = GitWorktrees.BusyKey(await a.EnsureRepoRootAsync(), a.WorkingFolder);
            string kb = GitWorktrees.BusyKey(await b.EnsureRepoRootAsync(), b.WorkingFolder);
            Assert.Equal(ka, kb);
            Assert.Equal(GitWorktrees.BusyKey(Main, null), ka);
        }
        finally
        {
            a.Dispose();
            b.Dispose();
        }
    }

    [Fact]
    public async Task Listing_maps_branches_to_worktrees_and_finds_the_current_one()
    {
        AddWorktree("wt-feat", "feat");
        var wts = await GitService.ListWorktreesAsync(Main);

        Assert.Equal(2, wts.Count);
        Assert.True(GitWorktrees.IsMain(wts, wts[0]));
        var here = GitWorktrees.Containing(wts, Path.Combine(WtPath("wt-feat")));
        Assert.Equal("feat", here!.Branch);
        Assert.Equal("wt-feat", GitWorktrees.Name(here.Path));
        // From main, "feat" is elsewhere; from the worktree, "main" is.
        Assert.Contains("feat", GitWorktrees.BranchesElsewhere(wts, wts[0]).Keys);
        Assert.Contains("main", GitWorktrees.BranchesElsewhere(wts, here).Keys);
    }

    [Fact]
    public async Task Switching_to_a_branch_held_by_another_worktree_fails_with_a_recognised_message()
    {
        // The case the Switch submenu now routes to "go to that worktree" instead.
        AddWorktree("wt-feat", "feat");
        var r = await GitService.SwitchBranchAsync(Main, "feat");
        Assert.False(r.Ok);
        Assert.Contains("another worktree", GitStatusText.FailureHint(r.Output));
    }

    [Fact]
    public async Task A_session_folder_spelled_differently_from_gits_path_still_finds_its_worktree()
    {
        // Found by the end-to-end test: %TEMP% here is an 8.3 path (C:\Users\MORTEN~1\…, and
        // GitHub's runners use RUNNER~1) while git lists the long form, so a raw compare decided
        // the session was in no worktree and "Remove this worktree" never appeared.
        AddWorktree("wt-feat", "feat");
        var wts = GitWorktrees.Canonicalize(await GitService.ListWorktreesAsync(Main));
        string asOpened = Path.Combine(Path.GetTempPath(), Path.GetFileName(_root), "wt-feat");

        Assert.Equal("feat", GitWorktrees.Containing(wts, GitWorktrees.Canonical(asOpened))?.Branch);
    }

    [Fact]
    public async Task A_session_opened_through_a_junction_finds_its_worktree()
    {
        AddWorktree("wt-feat", "feat");
        string link = Path.Combine(_root, "link-to-wt");
        var psi = new ProcessStartInfo("cmd.exe")
        {
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true,
        };
        foreach (var a in new[] { "/c", "mklink", "/J", link, WtPath("wt-feat") }) psi.ArgumentList.Add(a);
        using (var mk = Process.Start(psi)!) { mk.WaitForExit(); Assert.Equal(0, mk.ExitCode); }

        var wts = GitWorktrees.Canonicalize(await GitService.ListWorktreesAsync(Main));
        Assert.False(GitWorktrees.IsInside(link, wts[1].Path));   // the raw spellings really differ
        Assert.Equal("feat", GitWorktrees.Containing(wts, GitWorktrees.Canonical(link))?.Branch);
    }

    [Fact]
    public void Linked_worktree_is_told_apart_from_the_main_checkout_and_a_submodule()
    {
        AddWorktree("wt-feat", "feat");
        Assert.Equal("main", GitWorktrees.LinkedWorktreeOf(Path.Combine(WtPath("wt-feat")), Main));
        Assert.Null(GitWorktrees.LinkedWorktreeOf(Main, Main));

        // A submodule's .git is also a file — but it points into .git/modules, not worktrees.
        string fakeSub = Path.Combine(_root, "sub");
        Directory.CreateDirectory(fakeSub);
        File.WriteAllText(Path.Combine(fakeSub, ".git"), "gitdir: ../main/.git/modules/sub\n");
        Assert.Null(GitWorktrees.LinkedWorktreeOf(fakeSub, Main));
    }

    [Fact]
    public async Task A_worktree_nested_in_an_ignored_folder_is_detected_because_git_would_delete_it()
    {
        // The layout the pre-release review flagged, reproduced: worktree B inside A's
        // .gitignored .worktrees/ folder, with uncommitted work. git reports A clean, and
        // `worktree remove A` deletes B too. The menu must see B and refuse.
        File.WriteAllText(Path.Combine(Main, ".gitignore"), ".worktrees/\n");
        Git(Main, "add", ".gitignore");
        Git(Main, "commit", "-q", "-m", "ignore");
        AddWorktree("A", "a");
        Git(WtPath("A"), "worktree", "add", "-q", "-b", "b", "--", Path.Combine(WtPath("A"), ".worktrees", "b"));
        File.WriteAllText(Path.Combine(WtPath("A"), ".worktrees", "b", "work.txt"), "uncommitted\n");

        var status = await GitService.GetStatusAsync(WtPath("A"));
        Assert.False(status!.IsDirty);                         // git really does call A clean

        var wts = GitWorktrees.Canonicalize(await GitService.ListWorktreesAsync(Main));
        var a = GitWorktrees.Containing(wts, GitWorktrees.Canonical(WtPath("A")))!;
        Assert.Equal("a", a.Branch);
        Assert.Equal(new[] { "b" }, GitWorktrees.NestedInside(wts, a).Select(w => w.Branch));
    }

    [Fact]
    public void NestedRepositories_finds_an_independent_clone_and_a_submodule_but_not_the_worktree_itself()
    {
        // NestedInside only sees worktrees registered with THIS repo. An independent clone, or
        // a submodule (.git FILE), inside an ignored folder is deleted by `worktree remove` too.
        AddWorktree("A", "a");
        string a = WtPath("A");
        Git(a, "init", "-q", Path.Combine(a, ".cache", "other-clone"));                 // .git folder
        Directory.CreateDirectory(Path.Combine(a, "vendor", "sub"));
        File.WriteAllText(Path.Combine(a, "vendor", "sub", ".git"), "gitdir: ../../.git/modules/sub\n");
        Directory.CreateDirectory(Path.Combine(a, "src", "deep", "er"));                 // plain folders

        var found = GitWorktrees.NestedRepositories(a)!.Select(p => Path.GetFileName(p)).OrderBy(n => n);

        Assert.Equal(new[] { "other-clone", "sub" }, found);                             // not A itself
    }

    [Fact]
    public void NestedRepositories_is_empty_for_a_plain_worktree_and_does_not_follow_junctions()
    {
        AddWorktree("A", "a");
        // A junction from inside A to the MAIN checkout (which has a .git folder) must not be
        // followed — git doesn't follow it when deleting either.
        string link = Path.Combine(WtPath("A"), "link-to-main");
        var psi = new ProcessStartInfo("cmd.exe") { UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var x in new[] { "/c", "mklink", "/J", link, Main }) psi.ArgumentList.Add(x);
        using (var mk = Process.Start(psi)!) { mk.WaitForExit(); Assert.Equal(0, mk.ExitCode); }

        Assert.Empty(GitWorktrees.NestedRepositories(WtPath("A"))!);
        Directory.Delete(link);                                                           // the link only
    }

    [Fact]
    public void NestedRepositories_gives_up_rather_than_vouching_for_a_tree_it_did_not_finish()
    {
        AddWorktree("A", "a");
        for (int i = 0; i < 5; i++) Directory.CreateDirectory(Path.Combine(WtPath("A"), $"d{i}"));
        Assert.Null(GitWorktrees.NestedRepositories(WtPath("A"), maxDirs: 3));
    }

    [Fact]
    public async Task Removing_a_clean_worktree_deletes_it_and_keeps_its_branch()
    {
        AddWorktree("wt-feat", "feat");
        var r = await GitService.RemoveWorktreeAsync(Main, WtPath("wt-feat"));

        Assert.True(r.Ok, r.Output);
        Assert.False(Directory.Exists(WtPath("wt-feat")));
        Assert.Contains("feat", await GitService.ListBranchesAsync(Main));
    }

    [Theory]
    [InlineData("untracked.txt", false)]  // an untracked file
    [InlineData("a.txt", true)]           // a modified tracked file
    public async Task Removing_a_worktree_with_changes_is_refused_and_nothing_is_deleted(string file, bool tracked)
    {
        AddWorktree("wt-feat", "feat");
        string path = Path.Combine(WtPath("wt-feat"), file);
        File.WriteAllText(path, tracked ? "changed\n" : "new\n");

        var r = await GitService.RemoveWorktreeAsync(Main, WtPath("wt-feat"));

        Assert.False(r.Ok);
        Assert.Contains("uncommitted or untracked", GitStatusText.FailureHint(r.Output));
        Assert.True(File.Exists(path));
        Assert.True(File.Exists(Path.Combine(WtPath("wt-feat"), "a.txt")));
    }

    [Fact]
    public async Task Removing_a_locked_worktree_is_refused()
    {
        AddWorktree("wt-feat", "feat");
        Git(Main, "worktree", "lock", "--", WtPath("wt-feat"));

        var r = await GitService.RemoveWorktreeAsync(Main, WtPath("wt-feat"));

        Assert.False(r.Ok);
        Assert.Contains("locked", GitStatusText.FailureHint(r.Output));
        Assert.True(Directory.Exists(WtPath("wt-feat")));
        Git(Main, "worktree", "unlock", "--", WtPath("wt-feat"));
    }

    [Fact]
    public async Task The_main_checkout_can_never_be_removed()
    {
        var r = await GitService.RemoveWorktreeAsync(Main, Main);
        Assert.False(r.Ok);
        Assert.True(File.Exists(Path.Combine(Main, "a.txt")));
    }

    [Fact]
    public async Task Branch_delete_keeps_an_unmerged_branch_and_deletes_a_merged_one()
    {
        AddWorktree("wt-done", "done");       // no new commits → merged into main
        AddWorktree("wt-wip", "wip");
        File.WriteAllText(Path.Combine(WtPath("wt-wip"), "b.txt"), "b\n");
        Git(WtPath("wt-wip"), "add", "b.txt");
        Git(WtPath("wt-wip"), "commit", "-q", "-m", "wip");
        Assert.True((await GitService.RemoveWorktreeAsync(Main, WtPath("wt-done"))).Ok);
        Assert.True((await GitService.RemoveWorktreeAsync(Main, WtPath("wt-wip"))).Ok);

        var merged = await GitService.DeleteMergedBranchAsync(Main, "done");
        var unmerged = await GitService.DeleteMergedBranchAsync(Main, "wip");

        Assert.True(merged.Ok, merged.Output);
        Assert.False(unmerged.Ok);
        Assert.Contains("aren't merged", GitStatusText.FailureHint(unmerged.Output));
        var left = await GitService.ListBranchesAsync(Main);
        Assert.DoesNotContain("done", left);
        Assert.Contains("wip", left);
    }

    [Fact]
    public async Task An_option_shaped_branch_name_is_deleted_as_a_name()
    {
        // git won't create "-x" via `git branch`, but a ref can still be called that (a clone
        // or update-ref can produce one). Without `--`, `branch -d -x` would be an option.
        string head = Git(Main, "rev-parse", "HEAD").Trim();
        Git(Main, "update-ref", "refs/heads/-x", head);

        var r = await GitService.DeleteMergedBranchAsync(Main, "-x");

        Assert.True(r.Ok, r.Output);
        Assert.DoesNotContain("-x", await GitService.ListBranchesAsync(Main));
    }

    [Fact]
    public async Task Prune_forgets_a_worktree_whose_folder_was_deleted_and_nothing_else()
    {
        AddWorktree("wt-gone", "gone");
        AddWorktree("wt-kept", "kept");
        Directory.Delete(WtPath("wt-gone"), recursive: true);
        Assert.Contains(await GitService.ListWorktreesAsync(Main), w => w.IsPrunable);

        var r = await GitService.PruneWorktreesAsync(Main);

        Assert.True(r.Ok, r.Output);
        var after = await GitService.ListWorktreesAsync(Main);
        Assert.DoesNotContain(after, w => w.IsPrunable);
        Assert.Contains(after, w => GitWorktrees.Name(w.Path) == "wt-kept");
        Assert.True(Directory.Exists(WtPath("wt-kept")));
    }

    public void Dispose()
    {
        try
        {
            foreach (var f in Directory.EnumerateFiles(_root, "*", SearchOption.AllDirectories))
                File.SetAttributes(f, FileAttributes.Normal);
            Directory.Delete(_root, recursive: true);
        }
        catch { }
    }
}

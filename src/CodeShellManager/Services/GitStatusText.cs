using System;

namespace CodeShellManager.Services;

/// <summary>
/// Wording for the terminal toolbar's git chip and menu. WPF-free so the strings — which are
/// most of what the user actually sees of this feature — are unit-testable.
/// </summary>
public static class GitStatusText
{
    /// <summary>"↑2 ↓5", "↑2", "↓5", or "" when level with (or not tracking) an upstream.</summary>
    public static string Sync(int? ahead, int? behind)
    {
        string up = ahead is > 0 ? $"↑{ahead}" : "";
        string down = behind is > 0 ? $"↓{behind}" : "";
        return string.Join(" ", new[] { up, down }).Trim();
    }

    /// <summary>
    /// The disabled first line of the menu, e.g. "main → origin/main · ↑2 ↓5 · 3 changed"
    /// or "main · no upstream · clean". In a linked worktree, <paramref name="worktreeOf"/>
    /// (the main repo's name) adds " · worktree of X" — otherwise two panes on one repo
    /// differ only by their folder path.
    /// </summary>
    public static string MenuHeader(string? branch, string? upstream, int? ahead, int? behind, int changeCount,
        string? worktreeOf = null)
    {
        string head = string.IsNullOrEmpty(branch) ? "detached HEAD" : branch;
        string track = string.IsNullOrEmpty(upstream) ? " · no upstream" : $" → {upstream}";
        string sync = Sync(ahead, behind);
        // git omits "# branch.ab" when the upstream is configured but its remote branch no
        // longer exists (deleted after merge, say) — that is not "up to date".
        if (!string.IsNullOrEmpty(upstream) && sync.Length == 0)
            sync = ahead is null ? "upstream gone" : "up to date";
        string changes = changeCount switch
        {
            0 => "clean",
            1 => "1 changed",
            _ => $"{changeCount} changed",
        };
        return $"{head}{track}" + (sync.Length > 0 ? $" · {sync}" : "") + $" · {changes}"
               + (string.IsNullOrEmpty(worktreeOf) ? "" : $" · worktree of {worktreeOf}");
    }

    /// <summary>Toast text after a successful fetch, from the freshly re-read status.</summary>
    public static string FetchSummary(string? branch, string? upstream, int? ahead, int? behind)
    {
        if (string.IsNullOrEmpty(branch) || string.IsNullOrEmpty(upstream) || behind is null)
            return "Fetched.";
        if (behind == 0 && ahead is null or 0) return $"{branch} is up to date with {upstream}.";
        if (behind == 0) return $"Nothing new — {branch} is {ahead} ahead of {upstream}.";
        string commits = behind == 1 ? "1 new commit" : $"{behind} new commits";
        return ahead is > 0
            ? $"{commits} on {upstream}; {branch} has diverged ({ahead} ahead)."
            : $"{commits} on {upstream} — Pull to fast-forward.";
    }

    /// <summary>
    /// A short, actionable reason for a failed fetch/pull/switch, recognised from git's own
    /// message; null when the message is not one we know (the panel then shows git's text).
    /// </summary>
    public static string? FailureHint(string output)
    {
        string o = output ?? "";
        bool Has(string s) => o.Contains(s, StringComparison.OrdinalIgnoreCase);

        // Worktree refusals first — their wording overlaps the generic cases below. Each was
        // captured from git 2.54; see GitWorktreeRepoTests.
        if (Has("is already used by worktree at") || Has("is already checked out at"))
            return "That branch is checked out in another worktree. Open that worktree instead (Worktrees ▸).";
        if (Has("contains modified or untracked files"))
            return "The worktree has uncommitted or untracked files. Commit or stash them first.";
        if (Has("cannot remove a locked working tree"))
            return "The worktree is locked. Unlock it with 'git worktree unlock' first.";
        if (Has("is a main working tree"))
            return "That is the repository's main checkout, which can't be removed as a worktree.";
        if (Has("is not fully merged"))
            return "The branch has commits that aren't merged anywhere, so it was kept.";
        if (Has("failed to delete") && Has("Permission denied"))
            return "Something still has files open in that folder — an editor, a terminal or a dev " +
                   "server. Close it, then finish in a terminal with 'git worktree remove --force <folder>'. " +
                   "Everything in it was committed.";

        if (Has("Not possible to fast-forward") || Has("diverging branches") || Has("have diverged"))
            return "The branch has diverged from its upstream, so it can't fast-forward. Merge or rebase in a terminal.";
        if (Has("would be overwritten"))
            return "Local changes would be overwritten. Commit or stash them first.";
        if (Has("terminal prompts disabled") || Has("could not read Username") || Has("Authentication failed"))
            return "The remote needs credentials. Run git fetch once in a terminal to sign in.";
        if (Has("Permission denied (publickey)") || Has("Host key verification failed"))
            return "SSH authentication failed. Check your key or ssh-agent.";
        if (Has("no tracking information") || Has("There is no tracking information"))
            return "This branch has no upstream to pull from.";
        if (Has("Could not resolve host") || Has("unable to access"))
            return "Couldn't reach the remote. Check the network.";
        if (Has("index.lock"))
            return "Another git command is running in this repo. Try again in a moment.";
        return null;
    }
}

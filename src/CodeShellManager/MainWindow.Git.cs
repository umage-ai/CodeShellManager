using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Documents;
using System.Windows.Media;
using CodeShellManager.Models;
using CodeShellManager.Services;
using CodeShellManager.ViewModels;
using Brushes    = System.Windows.Media.Brushes;
using Color      = System.Windows.Media.Color;
using FontFamily = System.Windows.Media.FontFamily;
using WpfButton  = System.Windows.Controls.Button;
using WpfTextBox = System.Windows.Controls.TextBox;
using Clipboard   = System.Windows.Clipboard;
using ContextMenu = System.Windows.Controls.ContextMenu;
using MenuItem    = System.Windows.Controls.MenuItem;
using MessageBox  = System.Windows.MessageBox;

namespace CodeShellManager;

/// <summary>
/// The terminal toolbar's git chip ("⎇ main ● ↑2 ↓5") and its menu: fetch, fast-forward pull,
/// status, switch branch. See "Git tools on the terminal toolbar" in CLAUDE.md.
///
/// Every operation runs as its own background git process via <see cref="GitService"/> —
/// nothing is ever typed into the session's terminal, which in a Claude session would land
/// as text in Claude's prompt.
/// </summary>
public partial class MainWindow
{
    /// <summary>Per-session git controls, so a repo-wide change can repaint every pane on it.</summary>
    private sealed class GitPaneUi
    {
        public required WpfButton Chip;
        public required TextBlock ChipText;
        public required Border Panel;
        public required TextBlock PanelHeader;
        public required WpfTextBox PanelText;
        /// <summary>Main repo's name when this pane is in a LINKED worktree, else null.</summary>
        public string? WorktreeOf;
    }

    private readonly Dictionary<string, GitPaneUi> _gitPanes = [];

    /// <summary>
    /// Repos with a fetch / pull / switch in flight. One at a time per repo: two pulls racing
    /// on one .git directory just makes the second fail on index.lock, and a fetch triggered
    /// from a second pane of the same repo would be pure duplication. Keyed by RepoRoot, so
    /// worktrees of one repository (which share refs) share the slot.
    /// </summary>
    private readonly HashSet<string> _gitBusyRepos = new(StringComparer.OrdinalIgnoreCase);

    private const int MaxChipBranchChars = 32;

    private static readonly SolidColorBrush GitMuted  = Frozen(0xa6, 0xad, 0xc8);
    private static readonly SolidColorBrush GitDirty  = Frozen(0xfa, 0xb3, 0x87); // peach
    private static readonly SolidColorBrush GitSync   = Frozen(0x89, 0xb4, 0xfa); // blue
    private static readonly SolidColorBrush GitText   = Frozen(0xcd, 0xd6, 0xf4);

    private static SolidColorBrush Frozen(byte r, byte g, byte b)
    {
        var br = new SolidColorBrush(Color.FromRgb(r, g, b));
        br.Freeze();
        return br;
    }

    private static string GitRepoKey(SessionViewModel vm) =>
        string.IsNullOrEmpty(vm.RepoRoot) ? vm.WorkingFolder : vm.RepoRoot;

    /// <summary>
    /// Builds the chip (docked into the terminal toolbar) and the output panel (docked under
    /// it, like the notes panel). Both start hidden; the chip appears once the session is
    /// known to be in a repo.
    /// </summary>
    private (WpfButton chip, Border panel) BuildGitControls(SessionViewModel vm)
    {
        var chipText = new TextBlock { VerticalAlignment = VerticalAlignment.Center, FontSize = 11 };
        var chip = new WpfButton
        {
            Content = chipText,
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(6, 2, 6, 2),
            Margin = new Thickness(0, 0, 6, 0),
            Visibility = Visibility.Collapsed,
            ToolTip = "Git: fetch, pull, status, switch branch",
        };
        chip.Click += (_, _) => ShowGitMenu(vm, chip);

        // ── output panel ────────────────────────────────────────────────────────────
        var header = new TextBlock
        {
            Foreground = GitText,
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        var refreshBtn = MakeDrawerActionButton("↻ Status");
        var copyBtn = MakeDrawerActionButton("📋 Copy");
        var closeBtn = MakeDrawerActionButton("✕");
        var actions = new DockPanel { LastChildFill = true };
        DockPanel.SetDock(closeBtn, Dock.Right);
        DockPanel.SetDock(copyBtn, Dock.Right);
        DockPanel.SetDock(refreshBtn, Dock.Right);
        actions.Children.Add(closeBtn);
        actions.Children.Add(copyBtn);
        actions.Children.Add(refreshBtn);
        actions.Children.Add(header);

        var text = new WpfTextBox
        {
            Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25)),
            Foreground = GitText,
            BorderThickness = new Thickness(0),
            IsReadOnly = true,
            AcceptsReturn = true,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            FontFamily = new FontFamily("Consolas"),
            FontSize = 12,
            Padding = new Thickness(8, 6, 8, 6),
            TextWrapping = TextWrapping.NoWrap,
        };
        var inner = new DockPanel();
        DockPanel.SetDock(actions, Dock.Top);
        inner.Children.Add(actions);
        inner.Children.Add(text);
        var panel = new Border
        {
            Background = new SolidColorBrush(Color.FromRgb(0x11, 0x11, 0x1b)),
            BorderBrush = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Padding = new Thickness(8, 2, 0, 0),
            Height = 180,
            Visibility = Visibility.Collapsed,
            Child = inner,
        };

        refreshBtn.Click += async (_, _) => await ShowGitStatusAsync(vm);
        copyBtn.Click += (_, _) =>
        {
            string t = string.IsNullOrEmpty(text.SelectedText) ? text.Text : text.SelectedText;
            if (!string.IsNullOrEmpty(t)) { try { Clipboard.SetText(t); } catch { } }
        };
        closeBtn.Click += (_, _) => panel.Visibility = Visibility.Collapsed;

        _gitPanes[vm.Id] = new GitPaneUi
        {
            Chip = chip, ChipText = chipText, Panel = panel, PanelHeader = header, PanelText = text,
        };

        vm.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(SessionViewModel.GitInfoVersion) or nameof(SessionViewModel.RepoRoot))
                Dispatcher.Invoke(() => UpdateGitChip(vm));
            if (e.PropertyName is nameof(SessionViewModel.RepoRoot))
                ProbeLinkedWorktree(vm);
        };
        UpdateGitChip(vm);
        ProbeLinkedWorktree(vm);
        return (chip, panel);
    }

    /// <summary>
    /// Works out, off the UI thread, whether this pane is in a linked worktree — a few file
    /// reads walking up from the folder, which for a WSL session means \\wsl$ I/O.
    /// </summary>
    private void ProbeLinkedWorktree(SessionViewModel vm)
    {
        if (vm.Session.Kind == SessionKind.Ssh || string.IsNullOrEmpty(vm.RepoRoot)) return;
        string folder = vm.WorkingFolder, root = vm.RepoRoot;
        _ = Task.Run(() => GitWorktrees.LinkedWorktreeOf(folder, root)).ContinueWith(t =>
        {
            if (!t.IsCompletedSuccessfully) return;
            Dispatcher.BeginInvoke(() =>
            {
                if (!_gitPanes.TryGetValue(vm.Id, out var ui) || ui.WorktreeOf == t.Result) return;
                ui.WorktreeOf = t.Result;
                UpdateGitChip(vm);
            });
        }, TaskScheduler.Default);
    }

    /// <summary>Repaints one pane's chip from the VM. Cheap; called on every git notification.</summary>
    private void UpdateGitChip(SessionViewModel vm)
    {
        if (!_gitPanes.TryGetValue(vm.Id, out var ui)) return;

        // SSH has no local folder to run git in (an OSC 9001 branch is display-only), and a
        // folder that isn't a repo gets nothing.
        bool show = vm.Session.Kind != SessionKind.Ssh
                    && vm.GitInfoLoaded
                    && (!string.IsNullOrEmpty(vm.GitBranch) || !string.IsNullOrEmpty(vm.RepoRoot));
        ui.Chip.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        if (!show) return;

        bool busy = _gitBusyRepos.Contains(GitRepoKey(vm));
        var t = ui.ChipText;
        t.Inlines.Clear();
        // Capped by hand rather than TextTrimming, which would trim from the END and eat the
        // ↑/↓ counts first. The tooltip carries the full name.
        string branch = vm.GitBranch ?? "detached";
        if (branch.Length > MaxChipBranchChars) branch = branch[..(MaxChipBranchChars - 1)] + "…";
        t.Inlines.Add(new Run((busy ? "⟳ " : "⎇ ") + branch) { Foreground = GitMuted });
        if (vm.GitIsDirty) t.Inlines.Add(new Run(" ●") { Foreground = GitDirty });
        string sync = GitStatusText.Sync(vm.GitAhead, vm.GitBehind);
        if (sync.Length > 0) t.Inlines.Add(new Run(" " + sync) { Foreground = GitSync });

        ui.Chip.ToolTip = GitStatusText.MenuHeader(
            vm.GitBranch, vm.GitUpstream, vm.GitAhead, vm.GitBehind, vm.GitChangeCount, ui.WorktreeOf);
    }

    private void UpdateGitChipsForRepo(string repoKey)
    {
        foreach (var s in _vm.Sessions)
            if (string.Equals(GitRepoKey(s), repoKey, StringComparison.OrdinalIgnoreCase))
                UpdateGitChip(s);
    }

    private void ShowGitMenu(SessionViewModel vm, WpfButton anchor)
    {
        // Show what we know now, and re-read in the background so the chip catches up.
        _ = vm.RefreshGitInfoAsync();

        bool busy = _gitBusyRepos.Contains(GitRepoKey(vm));
        var menu = new ContextMenu
        {
            PlacementTarget = anchor,
            Placement = System.Windows.Controls.Primitives.PlacementMode.Bottom,
        };

        // Headers carrying a branch name are TextBlocks, not strings: a string Header is parsed
        // for access keys, so "fix_x" would render as "fixx" with the x underlined.
        menu.Items.Add(new MenuItem
        {
            Header = new TextBlock
            {
                Text = GitStatusText.MenuHeader(vm.GitBranch, vm.GitUpstream, vm.GitAhead, vm.GitBehind, vm.GitChangeCount,
                    _gitPanes.TryGetValue(vm.Id, out var paneUi) ? paneUi.WorktreeOf : null),
            },
            IsEnabled = false,
        });
        menu.Items.Add(new Separator());

        var fetch = new MenuItem { Header = busy ? "Fetch (running…)" : "Fetch", IsEnabled = !busy };
        fetch.Click += async (_, _) => await FetchAsync(vm);
        menu.Items.Add(fetch);

        bool hasUpstream = !string.IsNullOrEmpty(vm.GitUpstream);
        var pull = new MenuItem
        {
            Header = vm.GitBehind is > 0 ? $"Pull ↓{vm.GitBehind} (fast-forward only)" : "Pull (fast-forward only)",
            IsEnabled = !busy && hasUpstream,
            ToolTip = hasUpstream ? null : "This branch has no upstream to pull from.",
        };
        ToolTipService.SetShowOnDisabled(pull, true);
        pull.Click += async (_, _) => await PullAsync(vm);
        menu.Items.Add(pull);

        var status = new MenuItem { Header = "Status…" };
        status.Click += async (_, _) => await ShowGitStatusAsync(vm);
        menu.Items.Add(status);

        var switchMenu = new MenuItem { Header = "Switch branch", IsEnabled = !busy };
        switchMenu.Items.Add(new MenuItem { Header = "Loading…", IsEnabled = false });
        bool loaded = false;
        switchMenu.SubmenuOpened += async (_, _) =>
        {
            if (loaded) return;
            loaded = true;
            string folder = vm.WorkingFolder;
            var branchesTask = Task.Run(() => GitService.ListBranchesAsync(folder));
            var worktreesTask = LoadWorktreesAsync(folder);
            var branches = await branchesTask;
            var (worktrees, here) = await worktreesTask;
            // A branch checked out in another worktree can't be switched to here — git allows
            // it in one worktree at a time. Offer to go to that worktree instead.
            var elsewhere = GitWorktrees.BranchesElsewhere(worktrees, GitWorktrees.Containing(worktrees, here));
            switchMenu.Items.Clear();
            if (branches.Count == 0)
            {
                switchMenu.Items.Add(new MenuItem { Header = "(no local branches)", IsEnabled = false });
                return;
            }
            foreach (string b in branches)
            {
                bool current = b == vm.GitBranch;
                string target = b;
                if (!current && elsewhere.TryGetValue(b, out var wt))
                {
                    var go = new MenuItem
                    {
                        Header = new TextBlock { Text = $"{b} — in {GitWorktrees.Name(wt.Path)}" },
                        ToolTip = $"Checked out in {wt.Path}. Git allows a branch in only one worktree " +
                                  "at a time, so this opens that worktree instead of switching.",
                    };
                    string path = wt.Path;
                    go.Click += async (_, _) => await GoToWorktreeAsync(vm, path);
                    switchMenu.Items.Add(go);
                    continue;
                }
                var mi = new MenuItem
                {
                    Header = new TextBlock { Text = b },
                    IsCheckable = false,
                    IsChecked = current,
                    IsEnabled = !current,
                };
                mi.Click += async (_, _) => await SwitchBranchAsync(vm, target);
                switchMenu.Items.Add(mi);
            }
        };
        menu.Items.Add(switchMenu);

        menu.Items.Add(BuildWorktreesMenu(vm, busy));

        menu.Items.Add(new Separator());

        var copy = new MenuItem { Header = "Copy branch name", IsEnabled = !string.IsNullOrEmpty(vm.GitBranch) };
        copy.Click += (_, _) => { try { Clipboard.SetText(vm.GitBranch ?? ""); } catch { } };
        menu.Items.Add(copy);

        menu.IsOpen = true;
    }

    // ── operations ──────────────────────────────────────────────────────────────────

    private async Task FetchAsync(SessionViewModel vm)
    {
        var r = await RunGitOpAsync(vm, "fetch", GitService.FetchAsync);
        if (r is not { Ok: true }) return;
        ToastHelper.Show("git fetch",
            GitStatusText.FetchSummary(vm.GitBranch, vm.GitUpstream, vm.GitAhead, vm.GitBehind));
    }

    private async Task PullAsync(SessionViewModel vm)
    {
        var r = await RunGitOpAsync(vm, "pull", GitService.PullFastForwardAsync);
        if (r is not { Ok: true }) return;
        ToastHelper.Show($"git pull — {vm.GitBranch}", GitService.SummarizePull(r.Output));
    }

    private async Task SwitchBranchAsync(SessionViewModel vm, string branch)
    {
        var r = await RunGitOpAsync(vm, "switch", f => GitService.SwitchBranchAsync(f, branch));
        if (r is not { Ok: true }) return;
        ToastHelper.Show("git switch", $"Switched to {branch}.");
    }

    /// <summary>
    /// Runs one fetch/pull/switch: claims the repo's busy slot, runs it off the UI thread,
    /// re-reads git state for every pane on the repo, and on failure opens the panel with
    /// git's own output. Returns null when the repo was already busy.
    /// </summary>
    private async Task<GitOpResult?> RunGitOpAsync(
        SessionViewModel vm, string verb, Func<string, Task<GitOpResult>> op)
    {
        string key = GitRepoKey(vm);
        if (!_gitBusyRepos.Add(key))
        {
            ToastHelper.Show("Git busy", "A git operation is already running for this repository.");
            return null;
        }
        UpdateGitChipsForRepo(key);

        GitOpResult result;
        try
        {
            result = await op(vm.WorkingFolder);
        }
        finally
        {
            _gitBusyRepos.Remove(key);
        }

        // A fetch moves only remote refs, which GitRepoWatcher (HEAD + index) never sees, so
        // every pane on this repo — worktrees included, they share refs — re-reads explicitly.
        var peers = _vm.Sessions
            .Where(s => string.Equals(GitRepoKey(s), key, StringComparison.OrdinalIgnoreCase))
            .ToList();
        await Task.WhenAll(peers.Select(s => s.RefreshGitInfoAsync()));
        UpdateGitChipsForRepo(key);

        if (!result.Ok)
        {
            string? hint = GitStatusText.FailureHint(result.Output);
            ShowGitPanel(vm, $"git {verb} failed" + (hint is null ? "" : $" — {hint}"),
                string.IsNullOrWhiteSpace(result.Output) ? "(git printed nothing)" : result.Output);
            ToastHelper.Show($"git {verb} failed", hint ?? "See the git panel for details.");
        }
        return result;
    }

    // ── worktrees ───────────────────────────────────────────────────────────────────

    /// <summary>
    /// "Worktrees ▸": every worktree of the repo with its branch and state, go-to on click,
    /// then New / Prune / Remove. Filled when it opens — one `worktree list` plus one status
    /// per worktree, in parallel — so an unopened menu costs nothing.
    /// </summary>
    private MenuItem BuildWorktreesMenu(SessionViewModel vm, bool busy)
    {
        var wtMenu = new MenuItem { Header = "Worktrees" };
        wtMenu.Items.Add(new MenuItem { Header = "Loading…", IsEnabled = false });
        bool loaded = false;
        wtMenu.SubmenuOpened += async (_, _) =>
        {
            if (loaded) return;
            loaded = true;
            var (worktrees, here) = await LoadWorktreesAsync(vm.WorkingFolder);
            var current = GitWorktrees.Containing(worktrees, here);
            var live = worktrees.Where(w => !w.IsBare && !w.IsPrunable).ToList();
            var statuses = await Task.WhenAll(live.Select(w => Task.Run(() => GitService.GetStatusAsync(w.Path))));
            var statusOf = live.Zip(statuses).ToDictionary(p => p.First.Path, p => p.Second);

            wtMenu.Items.Clear();
            foreach (var w in worktrees)
            {
                if (w.IsBare) continue;
                bool isCurrent = current != null && GitWorktrees.Normalize(w.Path)
                    .Equals(GitWorktrees.Normalize(current.Path), StringComparison.OrdinalIgnoreCase);
                var mi = new MenuItem
                {
                    Header = new TextBlock { Text = GitWorktrees.Label(w, statusOf.GetValueOrDefault(w.Path)) },
                    IsChecked = isCurrent,
                    IsEnabled = !isCurrent && !w.IsPrunable,
                    ToolTip = w.IsPrunable ? $"{w.Path} no longer exists — prune to forget it." : w.Path,
                };
                ToolTipService.SetShowOnDisabled(mi, true);
                string path = w.Path;
                mi.Click += async (_, _) => await GoToWorktreeAsync(vm, path);
                wtMenu.Items.Add(mi);
            }

            wtMenu.Items.Add(new Separator());
            var add = new MenuItem { Header = "New worktree from this branch…" };
            add.Click += async (_, _) => await OpenNewWorktreeDialogAsync(vm);
            wtMenu.Items.Add(add);

            int prunable = worktrees.Count(w => w.IsPrunable);
            if (prunable > 0)
            {
                var prune = new MenuItem
                {
                    Header = prunable == 1 ? "Prune 1 missing worktree" : $"Prune {prunable} missing worktrees",
                    IsEnabled = !busy,
                    ToolTip = "Forgets worktrees whose folder was deleted. Only git's own bookkeeping is removed.",
                };
                prune.Click += async (_, _) => await PruneWorktreesAsync(vm, prunable);
                wtMenu.Items.Add(prune);
            }

            if (current != null && !GitWorktrees.IsMain(worktrees, current))
            {
                var remove = new MenuItem
                {
                    Header = "Remove this worktree…",
                    IsEnabled = !busy && !current.IsLocked,
                    ToolTip = current.IsLocked
                        ? "This worktree is locked (git worktree unlock)."
                        : "Closes the sessions in it and deletes the folder. Git refuses if anything is uncommitted.",
                };
                ToolTipService.SetShowOnDisabled(remove, true);
                string mainPath = worktrees[0].Path;
                var target = current;
                remove.Click += async (_, _) =>
                    await RemoveWorktreeAsync(vm, target, mainPath, statusOf.GetValueOrDefault(target.Path));
                wtMenu.Items.Add(remove);
            }
        };
        return wtMenu;
    }

    /// <summary>
    /// `git worktree list` with every path canonicalized, plus <paramref name="folder"/>'s own
    /// canonical path — both off the UI thread, since canonicalizing touches the disk.
    /// </summary>
    private static Task<(IReadOnlyList<GitService.WorktreeInfo> worktrees, string here)> LoadWorktreesAsync(string folder) =>
        Task.Run(async () =>
        {
            var raw = await GitService.ListWorktreesAsync(folder).ConfigureAwait(false);
            return (GitWorktrees.Canonicalize(raw), GitWorktrees.Canonical(folder));
        });

    /// <summary>
    /// Live and sleeping sessions whose folder is inside <paramref name="worktreePath"/>
    /// (already canonical). Folders are canonicalized off the UI thread — see
    /// <see cref="GitWorktrees.Canonical"/> for why a string compare is not enough.
    /// </summary>
    private async Task<(List<SessionViewModel> live, List<ShellSession> dormant)> SessionsInWorktreeAsync(string worktreePath)
    {
        var live = _vm.Sessions.Select(s => (s, s.WorkingFolder)).ToList();
        var dormant = _sessionManager.Sessions.Where(s => s.IsDormant).Select(s => (s, s.WorkingFolder)).ToList();
        return await Task.Run(() => (
            live.Where(x => GitWorktrees.IsInside(GitWorktrees.Canonical(x.WorkingFolder), worktreePath)).Select(x => x.s).ToList(),
            dormant.Where(x => GitWorktrees.IsInside(GitWorktrees.Canonical(x.WorkingFolder), worktreePath)).Select(x => x.s).ToList()));
    }

    /// <summary>
    /// Takes the user to a worktree: its live session if there is one, else wakes a sleeping
    /// one, else opens a new session there (same command, group and profile as this one).
    /// </summary>
    private async Task GoToWorktreeAsync(SessionViewModel from, string worktreePath)
    {
        var (liveHere, dormantHere) = await SessionsInWorktreeAsync(worktreePath);
        var live = liveHere.FirstOrDefault();
        if (live != null)
        {
            _vm.FocusSessionCommand.Execute(live);
            return;
        }
        var dormant = dormantHere.FirstOrDefault();
        if (dormant != null)
        {
            await WakeSessionAsync(dormant);
            return;
        }
        await LaunchSessionInSiblingWorktreeAsync(from, worktreePath);
    }

    private async Task PruneWorktreesAsync(SessionViewModel vm, int count)
    {
        var r = await RunGitOpAsync(vm, "worktree prune", GitService.PruneWorktreesAsync);
        if (r is not { Ok: true }) return;
        ToastHelper.Show("git worktree prune",
            count == 1 ? "Forgot 1 worktree whose folder was gone." : $"Forgot {count} worktrees whose folders were gone.");
    }

    /// <summary>
    /// The one write in this menu beyond pull/switch, so it is guarded three ways: refused up
    /// front when the worktree has changes, confirmed with the exact folder and the sessions it
    /// will close, and run as `git worktree remove` WITHOUT --force so git refuses anything
    /// uncommitted or untracked itself. Sessions in the folder are closed and their processes
    /// waited for before git runs: on Windows a process whose cwd is inside the folder locks it,
    /// and git then fails half-way through deleting it.
    /// </summary>
    private async Task RemoveWorktreeAsync(
        SessionViewModel vm, GitService.WorktreeInfo wt, string mainPath, GitRepoStatus? status)
    {
        string name = GitWorktrees.Name(wt.Path);
        if (status is { IsDirty: true })
        {
            MessageBox.Show(this,
                $"'{name}' has {status.Changes.Count} uncommitted or untracked change(s).\n\n" +
                "Commit or stash them first — removing the worktree would delete them.",
                "Remove worktree", MessageBoxButton.OK, MessageBoxImage.Information);
            return;
        }

        var (liveHere, dormantHere) = await SessionsInWorktreeAsync(wt.Path);

        var msg = new System.Text.StringBuilder();
        msg.AppendLine($"Remove the worktree '{name}'" + (string.IsNullOrEmpty(wt.Branch) ? "?" : $" (branch {wt.Branch})?"));
        msg.AppendLine();
        msg.AppendLine($"Folder: {wt.Path}");
        msg.AppendLine();
        msg.AppendLine("The folder is deleted. Git refuses if anything in it is uncommitted or untracked; " +
                       "ignored files such as build output are deleted with it. The branch itself is kept " +
                       "unless you choose to delete it next.");
        if (liveHere.Count > 0)
        {
            msg.AppendLine();
            msg.AppendLine("These sessions will be closed:");
            foreach (var s in liveHere) msg.AppendLine($"  • {s.DisplayName}");
        }
        if (dormantHere.Count > 0)
        {
            msg.AppendLine();
            msg.AppendLine("These sleeping sessions will be deleted:");
            foreach (var s in dormantHere) msg.AppendLine($"  • {(string.IsNullOrWhiteSpace(s.Name) ? s.DefaultDisplayName : s.Name)}");
        }
        if (MessageBox.Show(this, msg.ToString().TrimEnd(), "Remove worktree",
                MessageBoxButton.YesNo, MessageBoxImage.Warning, MessageBoxResult.No) != MessageBoxResult.Yes)
            return;

        string key = GitRepoKey(vm);
        if (!_gitBusyRepos.Add(key))
        {
            ToastHelper.Show("Git busy", "A git operation is already running for this repository.");
            return;
        }
        UpdateGitChipsForRepo(key);
        try
        {
            // Close first and wait for the processes to exit — see the summary for why.
            var exits = new List<Task>();
            foreach (var s in liveHere)
            {
                var pty = s.Pty;
                s.CloseCommand.Execute(null);
                if (pty != null) exits.Add(WaitForPtyExitAsync(pty, timeoutMs: 10_000));
            }
            await Task.WhenAll(exits);
            foreach (var d in dormantHere)
            {
                _dormantSidebarItems.Remove(d.Id);
                _sessionManager.RemoveSession(d.Id);
            }
            if (dormantHere.Count > 0)
            {
                RebuildSidebarOrder();
                _ = _vm.SaveStateAsync();
            }

            var r = await GitService.RemoveWorktreeAsync(mainPath, wt.Path);
            if (!r.Ok)
            {
                string? hint = GitStatusText.FailureHint(r.Output);
                MessageBox.Show(this,
                    $"git worktree remove failed{(hint is null ? "." : $": {hint}")}\n\n{r.Output}" +
                    (liveHere.Count > 0 ? "\n\nThe sessions that were in it are closed; Ctrl+Shift+T reopens them." : ""),
                    "Remove worktree", MessageBoxButton.OK, MessageBoxImage.Warning);
                return;
            }

            string removed = $"Removed worktree {name}.";
            if (!string.IsNullOrEmpty(wt.Branch) &&
                MessageBox.Show(this,
                    $"Worktree '{name}' removed.\n\nAlso delete the branch '{wt.Branch}'?\n\n" +
                    "Git only deletes it if it is fully merged; otherwise it is kept and nothing changes.",
                    "Delete branch", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No)
                == MessageBoxResult.Yes)
            {
                var b = await GitService.DeleteMergedBranchAsync(mainPath, wt.Branch);
                if (b.Ok) removed += $" Deleted branch {wt.Branch}.";
                else
                    MessageBox.Show(this,
                        $"The branch '{wt.Branch}' was kept: {GitStatusText.FailureHint(b.Output) ?? b.Output}",
                        "Delete branch", MessageBoxButton.OK, MessageBoxImage.Information);
            }
            ToastHelper.Show("git worktree remove", removed);
        }
        finally
        {
            _gitBusyRepos.Remove(key);
            var peers = _vm.Sessions
                .Where(s => string.Equals(GitRepoKey(s), key, StringComparison.OrdinalIgnoreCase)).ToList();
            await Task.WhenAll(peers.Select(s => s.RefreshGitInfoAsync()));
            UpdateGitChipsForRepo(key);
        }
    }

    /// <summary>
    /// Waits for a closed session's process to exit, bounded. HasExited, not IsRunning — see
    /// "Waiting for a PTY to exit" in CLAUDE.md.
    /// </summary>
    private static async Task WaitForPtyExitAsync(Terminal.PseudoTerminal pty, int timeoutMs)
    {
        if (pty.HasExited) return;
        var tcs = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        void OnExit() => tcs.TrySetResult();
        pty.Exited += OnExit;
        try
        {
            if (pty.HasExited) return;
            await Task.WhenAny(tcs.Task, Task.Delay(timeoutMs));
        }
        finally { pty.Exited -= OnExit; }
    }

    private async Task ShowGitStatusAsync(SessionViewModel vm)
    {
        ShowGitPanel(vm, "git status", "…");
        var r = await GitService.StatusTextAsync(vm.WorkingFolder);
        ShowGitPanel(vm, "git status", string.IsNullOrWhiteSpace(r.Output) ? "(git printed nothing)" : r.Output);
    }

    private void ShowGitPanel(SessionViewModel vm, string header, string body)
    {
        if (!_gitPanes.TryGetValue(vm.Id, out var ui)) return;
        ui.PanelHeader.Text = header;
        ui.PanelHeader.ToolTip = header;
        ui.PanelText.Text = body.Replace("\n", Environment.NewLine);
        ui.PanelText.ScrollToHome();
        ui.Panel.Visibility = Visibility.Visible;
    }
}

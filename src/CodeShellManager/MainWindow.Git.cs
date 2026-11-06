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
    }

    private readonly Dictionary<string, GitPaneUi> _gitPanes = [];

    /// <summary>
    /// Repos with a fetch / pull / switch in flight. One at a time per repo: two pulls racing
    /// on one .git directory just makes the second fail on index.lock, and a fetch triggered
    /// from a second pane of the same repo would be pure duplication. Keyed by RepoRoot, so
    /// worktrees of one repository (which share refs) share the slot.
    /// </summary>
    private readonly HashSet<string> _gitBusyRepos = new(StringComparer.OrdinalIgnoreCase);

    private static Task<string?>? _vsCodeLookup;
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
        _vsCodeLookup ??= Task.Run(FindVsCode);

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
        };
        UpdateGitChip(vm);
        return (chip, panel);
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
            vm.GitBranch, vm.GitUpstream, vm.GitAhead, vm.GitBehind, vm.GitChangeCount);
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
                Text = GitStatusText.MenuHeader(vm.GitBranch, vm.GitUpstream, vm.GitAhead, vm.GitBehind, vm.GitChangeCount),
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
            var branches = await GitService.ListBranchesAsync(vm.WorkingFolder);
            switchMenu.Items.Clear();
            if (branches.Count == 0)
            {
                switchMenu.Items.Add(new MenuItem { Header = "(no local branches)", IsEnabled = false });
                return;
            }
            foreach (string b in branches)
            {
                bool current = b == vm.GitBranch;
                var mi = new MenuItem
                {
                    Header = new TextBlock { Text = b },
                    IsCheckable = false,
                    IsChecked = current,
                    IsEnabled = !current,
                };
                string target = b;
                mi.Click += async (_, _) => await SwitchBranchAsync(vm, target);
                switchMenu.Items.Add(mi);
            }
        };
        menu.Items.Add(switchMenu);

        menu.Items.Add(new Separator());

        var copy = new MenuItem { Header = "Copy branch name", IsEnabled = !string.IsNullOrEmpty(vm.GitBranch) };
        copy.Click += (_, _) => { try { Clipboard.SetText(vm.GitBranch ?? ""); } catch { } };
        menu.Items.Add(copy);

        if (_vsCodeLookup is { IsCompletedSuccessfully: true, Result: { } codeExe })
        {
            var code = new MenuItem { Header = "Open in VS Code" };
            string folder = vm.WorkingFolder;
            code.Click += (_, _) => _ = Task.Run(() =>
            {
                try
                {
                    // Code.exe directly with the folder as one argv element — not code.cmd,
                    // which would put the folder through cmd.exe's parser.
                    var psi = new System.Diagnostics.ProcessStartInfo(codeExe) { UseShellExecute = false };
                    psi.ArgumentList.Add(folder);
                    System.Diagnostics.Process.Start(psi)?.Dispose();
                }
                catch (Exception ex)
                {
                    Dispatcher.BeginInvoke(() => ToastHelper.Show("Couldn't open VS Code", ex.Message));
                }
            });
            menu.Items.Add(code);
        }

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

    /// <summary>
    /// Finds VS Code's Code.exe via the <c>code.cmd</c> launcher on PATH (it lives in
    /// <c>&lt;install&gt;\bin\</c>). File checks only — no process is spawned to find it.
    /// </summary>
    private static string? FindVsCode()
    {
        try
        {
            string path = Environment.GetEnvironmentVariable("PATH") ?? "";
            foreach (string dir in path.Split(';', StringSplitOptions.RemoveEmptyEntries))
            {
                string d = dir.Trim().Trim('"');
                if (d.Length == 0 || d.StartsWith(@"\\", StringComparison.Ordinal)) continue; // skip network dirs
                string cmd = System.IO.Path.Combine(d, "code.cmd");
                if (!System.IO.File.Exists(cmd)) continue;
                string exe = System.IO.Path.GetFullPath(System.IO.Path.Combine(d, "..", "Code.exe"));
                if (System.IO.File.Exists(exe)) return exe;
            }
        }
        catch { }
        return null;
    }
}

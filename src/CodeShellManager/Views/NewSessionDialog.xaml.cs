using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using CodeShellManager.Models;
using CodeShellManager.Services;

namespace CodeShellManager.Views;

public partial class NewSessionDialog : Window
{
    private static readonly string[] DefaultCommands =
    [
        "claude", "claude --continue", "claude --model claude-opus-4-6",
        "claude --dangerously-skip-permissions", "codex", "gh copilot suggest", "pwsh", "cmd"
    ];

    // Local session output
    public string SelectedFolder { get; private set; } = "";
    public string SelectedCommand { get; private set; } = "claude";
    public string SelectedArgs { get; private set; } = "";
    public string SessionName { get; private set; } = "";
    public string SelectedGroupId { get; private set; } = "";

    // SSH session output
    public bool IsRemote { get; private set; } = false;
    public string SshHost { get; private set; } = "";
    public int SshPort { get; private set; } = 22;
    public string SshUser { get; private set; } = "";
    public string SshRemoteFolder { get; private set; } = "";

    // WSL session output
    public bool IsWsl { get; private set; } = false;
    public string WslDistro { get; private set; } = "";
    public string WslUser { get; private set; } = "";
    public string WslWorkingFolder { get; private set; } = "";

    // Profile-driven appearance overrides (null when no profile picked)
    public string? ProfileFontFamily { get; private set; }
    public int? ProfileFontSize { get; private set; }
    public string? ProfileFontWeight { get; private set; }
    public bool? ProfileFontLigatures { get; private set; }
    public string? ProfileCursorShape { get; private set; }
    public bool? ProfileCursorBlink { get; private set; }
    public string? ProfilePadding { get; private set; }
    public double? ProfileBackgroundOpacity { get; private set; }
    public bool? ProfileRetroEffect { get; private set; }
    public string? ProfileColorSchemeJson { get; private set; }

    /// <summary>Paths of sibling worktrees the user opted to also launch sessions for.</summary>
    public IReadOnlyList<string> AdditionalWorktreePaths { get; private set; } = Array.Empty<string>();

    /// <summary>
    /// When non-null, the user picked an entry from the "Recently closed" list instead of
    /// filling in the form. The caller (<c>OpenNewSessionDialogCore</c>) should reopen this
    /// session via <c>ReopenClosedSessionAsync</c> and ignore the rest of the form fields.
    /// </summary>
    public RecentlyClosedEntry? SelectedRecentlyClosed { get; private set; }

    private readonly IReadOnlyList<WindowsTerminalProfile> _profiles;
    private readonly System.Windows.Threading.DispatcherTimer _worktreeDebounce;
    private System.Threading.CancellationTokenSource? _worktreeProbeCts;
    private string? _lastProbedFolder;
    /// <summary>
    /// What we last auto-filled into <see cref="NameBox"/>. AutoFillName uses this
    /// to tell "the user hasn't typed anything custom" from "the user has". When
    /// the box equals this value (or is empty), we're free to overwrite it when
    /// the source context (folder / distro / host) changes. Anything else means
    /// the user has edited it and we must not stomp.
    /// </summary>
    private string _lastAutoFilledName = "";
    /// <summary>
    /// Distro name we want PopulateWslDistrosAsync to pre-select once the combo
    /// finishes loading. Empty = use the default (first / system default distro).
    /// </summary>
    private readonly string _preselectWslDistro = "";

    public NewSessionDialog(
        string defaultFolder = "",
        IEnumerable<string>? launchCommands = null,
        IReadOnlyList<WindowsTerminalProfile>? profiles = null,
        string? defaultCommand = null,
        string? defaultArgs = null,
        string? defaultName = null,
        IReadOnlyList<RecentlyClosedEntry>? recentlyClosed = null,
        ShellSession? defaultSourceSession = null)
    {
        InitializeComponent();
        FolderBox.Text = defaultFolder;
        _profiles = profiles ?? Array.Empty<WindowsTerminalProfile>();
        _preselectWslDistro = defaultSourceSession?.IsWsl == true ? defaultSourceSession.WslDistro : "";

        var customItem = CommandCombo.Items[0];
        CommandCombo.Items.Clear();
        foreach (var cmd in launchCommands ?? DefaultCommands)
            CommandCombo.Items.Add(new ComboBoxItem { Content = cmd, Tag = cmd });
        CommandCombo.Items.Add(customItem);

        // Pre-fill command if a parent session passed one through; otherwise default to first entry.
        ComboBoxItem? matchedCmd = null;
        if (!string.IsNullOrWhiteSpace(defaultCommand))
        {
            string combined = string.IsNullOrEmpty(defaultArgs)
                ? defaultCommand
                : $"{defaultCommand} {defaultArgs}";
            matchedCmd = CommandCombo.Items.OfType<ComboBoxItem>()
                .FirstOrDefault(it => string.Equals(it.Tag?.ToString(), combined, StringComparison.Ordinal));
            if (matchedCmd == null)
            {
                // Fall back to [custom] + populate args box.
                matchedCmd = CommandCombo.Items.OfType<ComboBoxItem>()
                    .FirstOrDefault(it => it.Tag?.ToString() == "custom");
                CustomArgsBox.Text = combined;
            }
        }
        CommandCombo.SelectedItem = matchedCmd ?? CommandCombo.Items[0];

        if (!string.IsNullOrWhiteSpace(defaultName)) NameBox.Text = defaultName;

        if (_profiles.Count > 0)
        {
            ProfilePanel.Visibility = Visibility.Visible;
            ProfileCombo.Items.Add(new ComboBoxItem { Content = "— none —", Tag = null });
            foreach (var p in _profiles)
                ProfileCombo.Items.Add(new ComboBoxItem { Content = p.Name, Tag = p });
            ProfileCombo.SelectedIndex = 0;
        }

        PopulateRecentlyClosed(recentlyClosed);

        _worktreeDebounce = new System.Windows.Threading.DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(700)
        };
        _worktreeDebounce.Tick += async (_, _) =>
        {
            _worktreeDebounce.Stop();
            await ProbeSiblingWorktreesAsync(FolderBox.Text.Trim());
        };

        FolderBox.TextChanged += (_, _) => { AutoFillName(); ScheduleWorktreeProbe(); };
        SshHostBox.TextChanged += (_, _) => AutoFillName();
        WslDistroCombo.SelectionChanged += (_, _) => AutoFillName();
        WslWorkingFolderBox.TextChanged += (_, _) => AutoFillName();

        // Inherit WSL parent: when a user right-clicks a WSL session and picks
        // "New session here", default the new dialog to WSL mode with the same
        // distro/user/folder pre-filled. The combo selection happens later in
        // PopulateWslDistrosAsync (it's async-populated on Loaded).
        if (defaultSourceSession?.IsWsl == true)
        {
            WslRadio.IsChecked = true;
            WslUserBox.Text = defaultSourceSession.WslUser ?? "";
            WslWorkingFolderBox.Text = defaultSourceSession.WslWorkingFolder ?? "";
        }

        Loaded += async (_, _) =>
        {
            if (IsLocalMode && !string.IsNullOrWhiteSpace(FolderBox.Text))
                await ProbeSiblingWorktreesAsync(FolderBox.Text.Trim());
            await PopulateWslDistrosAsync();
        };
    }

    /// <summary>
    /// Fills <c>WslDistroCombo</c> from <see cref="WslDiscoveryService.GetDistrosAsync"/>.
    /// On hosts without WSL installed we leave the combo empty and surface a one-line hint
    /// so the WSL radio doesn't appear broken.
    /// </summary>
    private async System.Threading.Tasks.Task PopulateWslDistrosAsync()
    {
        var distros = await WslDiscoveryService.GetDistrosAsync();
        WslDistroCombo.Items.Clear();
        if (distros.Count == 0)
        {
            WslHelpText.Text = "No WSL distros found. Install WSL from the Microsoft Store, then re-open this dialog.";
            return;
        }
        ComboBoxItem? preselectMatch = null;
        foreach (var d in distros)
        {
            string label = d.IsDefault ? $"{d.Name}  (default, v{d.Version})" : $"{d.Name}  (v{d.Version})";
            var item = new ComboBoxItem { Content = label, Tag = d.Name };
            WslDistroCombo.Items.Add(item);
            if (!string.IsNullOrEmpty(_preselectWslDistro)
                && string.Equals(d.Name, _preselectWslDistro, StringComparison.OrdinalIgnoreCase))
            {
                preselectMatch = item;
            }
        }
        WslDistroCombo.SelectedItem = preselectMatch ?? WslDistroCombo.Items[0];
        WslHelpText.Text = "";
    }

    private void ScheduleWorktreeProbe()
    {
        if (!IsLocalMode)
        {
            WorktreesPanel.Visibility = Visibility.Collapsed;
            return;
        }
        _worktreeDebounce.Stop();
        _worktreeDebounce.Start();
    }

    private async System.Threading.Tasks.Task ProbeSiblingWorktreesAsync(string folder)
    {
        if (string.IsNullOrWhiteSpace(folder) || !Directory.Exists(folder))
        {
            WorktreesPanel.Visibility = Visibility.Collapsed;
            return;
        }
        if (folder == _lastProbedFolder) return;
        _lastProbedFolder = folder;

        _worktreeProbeCts?.Cancel();
        var cts = new System.Threading.CancellationTokenSource();
        _worktreeProbeCts = cts;

        var worktrees = await Services.GitService.ListWorktreesAsync(folder);
        if (cts.IsCancellationRequested) return;

        // Exclude the chosen folder itself + bare repos. Normalize paths for comparison.
        string norm = Path.TrimEndingDirectorySeparator(Path.GetFullPath(folder)).Replace('\\', '/');
        var siblings = worktrees
            .Where(w => !w.IsBare)
            .Where(w =>
            {
                try
                {
                    string wp = Path.TrimEndingDirectorySeparator(Path.GetFullPath(w.Path)).Replace('\\', '/');
                    return !string.Equals(wp, norm, StringComparison.OrdinalIgnoreCase);
                }
                catch { return true; }
            })
            .ToList();

        WorktreesList.Children.Clear();
        if (siblings.Count == 0)
        {
            WorktreesPanel.Visibility = Visibility.Collapsed;
            return;
        }
        foreach (var w in siblings)
        {
            string label = string.IsNullOrEmpty(w.Branch)
                ? (w.IsDetached ? $"{Path.GetFileName(w.Path)} (detached)" : Path.GetFileName(w.Path))
                : $"{Path.GetFileName(w.Path)}  ⎇ {w.Branch}";
            var cb = new System.Windows.Controls.CheckBox
            {
                Content = label,
                Tag = w.Path,
                Foreground = new System.Windows.Media.SolidColorBrush(
                    System.Windows.Media.Color.FromRgb(0xcd, 0xd6, 0xf4)),
                Margin = new Thickness(0, 2, 0, 2),
                IsChecked = false
            };
            WorktreesList.Children.Add(cb);
        }
        WorktreesPanel.Visibility = Visibility.Visible;
    }

    private bool IsRemoteMode => RemoteRadio?.IsChecked == true;
    private bool IsWslMode => WslRadio?.IsChecked == true;
    private bool IsLocalMode => !IsRemoteMode && !IsWslMode;

    private void AutoFillName()
    {
        // Allow overwrite when the box is empty OR still holds our last auto-fill.
        // Anything else means the user typed something — leave it alone.
        if (!string.IsNullOrWhiteSpace(NameBox.Text) && NameBox.Text != _lastAutoFilledName)
            return;

        string suggested = "";
        if (IsRemoteMode)
        {
            var raw = SshHostBox.Text.Trim();
            if (!string.IsNullOrWhiteSpace(raw))
            {
                try { suggested = raw.Split(':')[0]; }
                catch { }
            }
        }
        else if (IsWslMode)
        {
            string distro = (WslDistroCombo.SelectedItem as ComboBoxItem)?.Tag as string ?? "";
            string folder = WslWorkingFolderBox.Text.Trim();
            string leaf = string.IsNullOrEmpty(folder)
                ? ""
                : Path.GetFileName(folder.TrimEnd('/'));
            suggested = string.IsNullOrEmpty(leaf)
                ? distro
                : (string.IsNullOrEmpty(distro) ? leaf : $"{distro}: {leaf}");
        }
        else
        {
            if (!string.IsNullOrWhiteSpace(FolderBox.Text))
            {
                try { suggested = Path.GetFileName(FolderBox.Text.TrimEnd('/', '\\')) ?? ""; }
                catch { }
            }
        }

        NameBox.Text = suggested;
        _lastAutoFilledName = suggested;
    }

    private void SessionType_Changed(object sender, RoutedEventArgs e)
    {
        if (LocalPanel == null) return;
        LocalPanel.Visibility = IsLocalMode ? Visibility.Visible : Visibility.Collapsed;
        SshPanel.Visibility = IsRemoteMode ? Visibility.Visible : Visibility.Collapsed;
        WslPanel.Visibility = IsWslMode ? Visibility.Visible : Visibility.Collapsed;
        // Profile combobox is local-only
        if (ProfilePanel != null && _profiles.Count > 0)
            ProfilePanel.Visibility = IsLocalMode ? Visibility.Visible : Visibility.Collapsed;
        if (WorktreesPanel != null)
        {
            WorktreesPanel.Visibility = Visibility.Collapsed;
            _lastProbedFolder = null;
        }
        CommandLabel.Text = IsRemoteMode ? "Remote Shell"
            : IsWslMode ? "Shell (inside WSL)"
            : "Command";
        NameBox.Text = "";
        AutoFillName();
    }

    private void BrowseFolder_Click(object sender, RoutedEventArgs e)
    {
        using var dialog = new System.Windows.Forms.FolderBrowserDialog
        {
            Description = "Select working folder",
            UseDescriptionForTitle = true,
            SelectedPath = FolderBox.Text
        };
        if (dialog.ShowDialog() == System.Windows.Forms.DialogResult.OK)
        {
            FolderBox.Text = dialog.SelectedPath;
            AutoFillName();
        }
    }

    /// <summary>
    /// Pops a folder picker rooted at the WSL filesystem (<c>\\wsl$\</c>). When the
    /// user picks a folder under one of the distros, both the distro combo and the
    /// Linux working-folder box update to match — so they can also switch distros
    /// by drilling into a different one in the dialog.
    /// </summary>
    private async void BrowseWslFolder_Click(object sender, RoutedEventArgs e)
    {
        string selectedDistro = (WslDistroCombo.SelectedItem as ComboBoxItem)?.Tag as string ?? "";
        string seed = await ComputeWslBrowseSeedAsync(selectedDistro, WslUserBox.Text.Trim());

        // Only InitialDirectory is set: it navigates the dialog to the seed but
        // leaves the bottom "Folder:" textbox empty (the user is about to pick anyway).
        // Setting SelectedPath as well shoves the raw UNC into that textbox, which the
        // shell renders as a truncated, slash-flipped mess (e.g. "bu/home/bitblade") —
        // worse than empty.
        using var dialog = new System.Windows.Forms.FolderBrowserDialog
        {
            Description = "Select Linux working folder (inside WSL)",
            UseDescriptionForTitle = true,
            InitialDirectory = seed,
        };
        if (dialog.ShowDialog() != System.Windows.Forms.DialogResult.OK) return;

        var (distro, linuxPath) = ParseWslUncPath(dialog.SelectedPath);
        if (string.IsNullOrEmpty(distro))
        {
            // User navigated out of the WSL share entirely (e.g. into C:\…). Putting
            // a Windows path into the Linux-folder box would just make `wsl --cd`
            // fail later — so refuse the selection and tell them why.
            System.Windows.MessageBox.Show(
                $"'{dialog.SelectedPath}' is not inside a WSL distro.\n\n" +
                "Please pick a folder under one of the distros shown in the left pane (Linux → Ubuntu, etc.).",
                "Not a WSL folder", MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }
        else
        {
            // If they drilled into a different distro than the combo had, switch the combo too.
            if (!string.Equals(distro, selectedDistro, StringComparison.OrdinalIgnoreCase))
            {
                foreach (var item in WslDistroCombo.Items.OfType<ComboBoxItem>())
                {
                    if (string.Equals(item.Tag as string, distro, StringComparison.OrdinalIgnoreCase))
                    {
                        WslDistroCombo.SelectedItem = item;
                        break;
                    }
                }
            }
            WslWorkingFolderBox.Text = linuxPath;
        }
        AutoFillName();
    }

    /// <summary>
    /// Seed path for the WSL folder picker. Prefers the user's home directory inside
    /// the distro (resolved via <c>cd ~ &amp;&amp; pwd</c>) so picking lands somewhere
    /// useful; falls back to the distro root when WSL isn't reachable, and to
    /// <c>\\wsl$</c> when no distro is selected yet.
    /// </summary>
    private async System.Threading.Tasks.Task<string> ComputeWslBrowseSeedAsync(string distro, string user)
    {
        if (string.IsNullOrEmpty(distro)) return @"\\wsl$";
        string? home = await WslDiscoveryService.GetDistroHomeAsync(distro, user);
        if (string.IsNullOrEmpty(home)) return $@"\\wsl$\{distro}";
        return WslDiscoveryService.ToUncPath(distro, home);
    }

    /// <summary>
    /// Splits a WSL UNC path (<c>\\wsl$\Ubuntu\home\alice</c> or the
    /// <c>\\wsl.localhost\</c> variant) into (distro, linux-path). Returns empty
    /// strings when the input isn't a recognizable WSL UNC.
    /// </summary>
    internal static (string distro, string linuxPath) ParseWslUncPath(string unc)
    {
        if (string.IsNullOrWhiteSpace(unc)) return ("", "");
        string normalized = unc.Replace('/', '\\').TrimEnd('\\');
        string[] prefixes = { @"\\wsl$\", @"\\wsl.localhost\" };
        foreach (var prefix in prefixes)
        {
            if (!normalized.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) continue;
            string rest = normalized[prefix.Length..];
            if (string.IsNullOrEmpty(rest)) return ("", "");
            int slash = rest.IndexOf('\\');
            string distro = slash < 0 ? rest : rest[..slash];
            string linuxRest = slash < 0 ? "" : rest[(slash + 1)..];
            string linuxPath = string.IsNullOrEmpty(linuxRest) ? "" : "/" + linuxRest.Replace('\\', '/');
            return (distro, linuxPath);
        }
        return ("", "");
    }

    private void CommandCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (CustomArgsPanel == null) return;
        var selected = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString();
        CustomArgsPanel.Visibility = selected == "custom" ? Visibility.Visible : Visibility.Collapsed;
    }

    private void ProfileCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var profile = (ProfileCombo.SelectedItem as ComboBoxItem)?.Tag as WindowsTerminalProfile;

        if (profile == null)
        {
            // — none — clears all overrides; folder/name/command stay as the user left them.
            ProfileFontFamily = null;
            ProfileFontSize = null;
            ProfileFontWeight = null;
            ProfileFontLigatures = null;
            ProfileCursorShape = null;
            ProfileCursorBlink = null;
            ProfilePadding = null;
            ProfileBackgroundOpacity = null;
            ProfileRetroEffect = null;
            ProfileColorSchemeJson = null;
            return;
        }

        // Pre-fill empty fields only — preserve any user edits.
        if (string.IsNullOrWhiteSpace(FolderBox.Text) && !string.IsNullOrEmpty(profile.StartingDirectory))
            FolderBox.Text = profile.StartingDirectory;

        if (string.IsNullOrWhiteSpace(NameBox.Text))
            NameBox.Text = profile.Name;

        // Add the profile's commandline as a transient entry in CommandCombo and select it
        var cmdString = profile.Commandline;
        var existing = CommandCombo.Items.OfType<ComboBoxItem>()
            .FirstOrDefault(it => it.Tag?.ToString() == cmdString);
        if (existing != null)
        {
            CommandCombo.SelectedItem = existing;
        }
        else
        {
            // Insert just before the [custom] item (which is always last)
            var item = new ComboBoxItem { Content = cmdString, Tag = cmdString };
            CommandCombo.Items.Insert(CommandCombo.Items.Count - 1, item);
            CommandCombo.SelectedItem = item;
        }

        // Stash overrides
        ProfileFontFamily = profile.FontFamily;
        ProfileFontSize = profile.FontSize;
        ProfileFontWeight = profile.FontWeight;
        ProfileFontLigatures = profile.FontLigatures;
        ProfileCursorShape = profile.CursorShape;
        ProfileCursorBlink = profile.CursorBlink;
        ProfilePadding = profile.Padding;
        ProfileBackgroundOpacity = profile.BackgroundOpacity;
        ProfileRetroEffect = profile.RetroEffect;
        ProfileColorSchemeJson = profile.ColorSchemeJson;
    }

    private async void Start_Click(object sender, RoutedEventArgs e)
    {
        IsRemote = IsRemoteMode;
        IsWsl = IsWslMode;
        SessionName = NameBox.Text.Trim();

        if (IsLocalMode && WorktreesPanel.Visibility == Visibility.Visible)
        {
            AdditionalWorktreePaths = WorktreesList.Children.OfType<System.Windows.Controls.CheckBox>()
                .Where(c => c.IsChecked == true)
                .Select(c => c.Tag as string)
                .Where(p => !string.IsNullOrEmpty(p))
                .Select(p => p!)
                .ToList();
        }

        if (IsWsl)
        {
            WslDistro = (WslDistroCombo.SelectedItem as ComboBoxItem)?.Tag as string ?? "";
            if (string.IsNullOrWhiteSpace(WslDistro))
            {
                System.Windows.MessageBox.Show(
                    "Please select a WSL distro.",
                    "Distro required", MessageBoxButton.OK, MessageBoxImage.Warning);
                WslDistroCombo.Focus();
                return;
            }

            WslUser = WslUserBox.Text.Trim();
            WslWorkingFolder = WslWorkingFolderBox.Text.Trim();

            // If the user left the Linux folder blank, resolve $HOME eagerly so the
            // session's WorkingFolder UNC and its Linux path stay in sync. Otherwise
            // git status runs against the distro root (\\wsl$\<distro> → "/") while
            // the shell actually starts in $HOME — and the sidebar branch info goes
            // missing for repos under home. Best-effort: silent fallback to blank
            // (the existing "land in $HOME, no git info" behavior) when WSL is
            // unreachable.
            if (string.IsNullOrEmpty(WslWorkingFolder))
            {
                string? home = await WslDiscoveryService.GetDistroHomeAsync(WslDistro, WslUser);
                if (!string.IsNullOrEmpty(home)) WslWorkingFolder = home;
            }

            var selectedTag = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "bash";
            string raw = selectedTag == "custom" ? CustomArgsBox.Text.Trim() : selectedTag;
            var (exe, args) = CommandLineSplitter.Split(raw);
            SelectedCommand = string.IsNullOrEmpty(exe) ? "bash" : exe;
            SelectedArgs = args;

            SelectedFolder = "";
            DialogResult = true;
            Close();
            return;
        }

        if (IsRemote)
        {
            if (string.IsNullOrWhiteSpace(SshHostBox.Text))
            {
                System.Windows.MessageBox.Show(
                    "Please enter a host (e.g. user@hostname).",
                    "Host required", MessageBoxButton.OK, MessageBoxImage.Warning);
                SshHostBox.Focus();
                return;
            }

            var hostRaw = SshHostBox.Text.Trim();
            var atIdx = hostRaw.IndexOf('@');
            if (atIdx > 0)
            {
                SshUser = hostRaw[..atIdx];
                SshHost = hostRaw[(atIdx + 1)..];
            }
            else
            {
                SshUser = "";
                SshHost = hostRaw;
            }

            SshPort = int.TryParse(SshPortBox.Text.Trim(), out int port) && port is > 0 and <= 65535
                ? port : 22;

            SshRemoteFolder = SshRemoteFolderBox.Text.Trim();

            var selectedTag = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "bash";
            if (selectedTag == "custom")
            {
                var (exe, args) = CommandLineSplitter.Split(CustomArgsBox.Text.Trim());
                SelectedCommand = string.IsNullOrEmpty(exe) ? "bash" : exe;
                SelectedArgs = args;
            }
            else
            {
                var (exe, args) = CommandLineSplitter.Split(selectedTag);
                SelectedCommand = string.IsNullOrEmpty(exe) ? "bash" : exe;
                SelectedArgs = args;
            }

            SelectedFolder = "";
        }
        else
        {
            SelectedFolder = FolderBox.Text.Trim();

            var selectedTag = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "claude";
            if (selectedTag == "custom")
            {
                var (exe, args) = CommandLineSplitter.Split(CustomArgsBox.Text.Trim());
                SelectedCommand = string.IsNullOrEmpty(exe) ? "claude" : exe;
                SelectedArgs = args;
            }
            else
            {
                var (exe, args) = CommandLineSplitter.Split(selectedTag);
                SelectedCommand = string.IsNullOrEmpty(exe) ? "claude" : exe;
                SelectedArgs = args;
            }
        }

        DialogResult = true;
        Close();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }

    private void PopulateRecentlyClosed(IReadOnlyList<RecentlyClosedEntry>? entries)
    {
        if (entries == null || entries.Count == 0)
        {
            RecentlyClosedPanel.Visibility = Visibility.Collapsed;
            return;
        }
        RecentlyClosedPanel.Visibility = Visibility.Visible;
        var fg = new System.Windows.Media.SolidColorBrush(
            System.Windows.Media.Color.FromRgb(0xcd, 0xd6, 0xf4));
        var sub = new System.Windows.Media.SolidColorBrush(
            System.Windows.Media.Color.FromRgb(0x6c, 0x70, 0x86));
        foreach (var entry in entries)
        {
            var stack = new StackPanel { Orientation = System.Windows.Controls.Orientation.Vertical };
            stack.Children.Add(new TextBlock
            {
                Text = string.IsNullOrWhiteSpace(entry.Name) ? "(unnamed)" : entry.Name,
                Foreground = fg, FontSize = 13,
            });
            stack.Children.Add(new TextBlock
            {
                Text = entry.Subtitle,
                Foreground = sub, FontSize = 11,
                TextTrimming = System.Windows.TextTrimming.CharacterEllipsis,
            });
            var btn = new System.Windows.Controls.Button
            {
                Content = stack,
                Tag = entry,
                Background = System.Windows.Media.Brushes.Transparent,
                BorderBrush = new System.Windows.Media.SolidColorBrush(
                    System.Windows.Media.Color.FromRgb(0x31, 0x32, 0x44)),
                BorderThickness = new Thickness(0, 0, 0, 1),
                Padding = new Thickness(6, 5, 6, 5),
                HorizontalContentAlignment = System.Windows.HorizontalAlignment.Left,
                Cursor = System.Windows.Input.Cursors.Hand,
            };
            btn.Click += RecentlyClosed_Click;
            RecentlyClosedList.Children.Add(btn);
        }
    }

    private void RecentlyClosed_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.Tag is RecentlyClosedEntry entry)
        {
            SelectedRecentlyClosed = entry;
            DialogResult = true;
            Close();
        }
    }
}

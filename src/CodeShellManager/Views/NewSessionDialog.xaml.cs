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

    private readonly IReadOnlyList<WindowsTerminalProfile> _profiles;

    public NewSessionDialog(
        string defaultFolder = "",
        IEnumerable<string>? launchCommands = null,
        IReadOnlyList<WindowsTerminalProfile>? profiles = null)
    {
        InitializeComponent();
        FolderBox.Text = defaultFolder;
        _profiles = profiles ?? Array.Empty<WindowsTerminalProfile>();

        var customItem = CommandCombo.Items[0];
        CommandCombo.Items.Clear();
        foreach (var cmd in launchCommands ?? DefaultCommands)
            CommandCombo.Items.Add(new ComboBoxItem { Content = cmd, Tag = cmd });
        CommandCombo.Items.Add(customItem);
        CommandCombo.SelectedIndex = 0;

        if (_profiles.Count > 0)
        {
            ProfilePanel.Visibility = Visibility.Visible;
            ProfileCombo.Items.Add(new ComboBoxItem { Content = "— none —", Tag = null });
            foreach (var p in _profiles)
                ProfileCombo.Items.Add(new ComboBoxItem { Content = p.Name, Tag = p });
            ProfileCombo.SelectedIndex = 0;
        }

        FolderBox.TextChanged += (_, _) => AutoFillName();
        SshHostBox.TextChanged += (_, _) => AutoFillName();
    }

    private bool IsRemoteMode => RemoteRadio?.IsChecked == true;

    private void AutoFillName()
    {
        if (!string.IsNullOrWhiteSpace(NameBox.Text)) return;

        if (IsRemoteMode)
        {
            var raw = SshHostBox.Text.Trim();
            if (!string.IsNullOrWhiteSpace(raw))
            {
                try { NameBox.Text = raw.Split(':')[0]; }
                catch { }
            }
        }
        else
        {
            if (!string.IsNullOrWhiteSpace(FolderBox.Text))
            {
                try { NameBox.Text = Path.GetFileName(FolderBox.Text.TrimEnd('/', '\\')); }
                catch { }
            }
        }
    }

    private void SessionType_Changed(object sender, RoutedEventArgs e)
    {
        if (LocalPanel == null) return;
        LocalPanel.Visibility = IsRemoteMode ? Visibility.Collapsed : Visibility.Visible;
        SshPanel.Visibility = IsRemoteMode ? Visibility.Visible : Visibility.Collapsed;
        // Profile combobox is local-only
        if (ProfilePanel != null && _profiles.Count > 0)
            ProfilePanel.Visibility = IsRemoteMode ? Visibility.Collapsed : Visibility.Visible;
        CommandLabel.Text = IsRemoteMode ? "Remote Shell" : "Command";
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

    private void Start_Click(object sender, RoutedEventArgs e)
    {
        IsRemote = IsRemoteMode;
        SessionName = NameBox.Text.Trim();

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
}

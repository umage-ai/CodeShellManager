using System;
using System.Collections.Generic;
using System.IO;
using System.Windows;
using System.Windows.Controls;

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

    public NewSessionDialog(string defaultFolder = "", IEnumerable<string>? launchCommands = null)
    {
        InitializeComponent();
        FolderBox.Text = defaultFolder;

        var customItem = CommandCombo.Items[0];
        CommandCombo.Items.Clear();
        foreach (var cmd in launchCommands ?? DefaultCommands)
            CommandCombo.Items.Add(new ComboBoxItem { Content = cmd, Tag = cmd });
        CommandCombo.Items.Add(customItem);
        CommandCombo.SelectedIndex = 0;

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

    private void Start_Click(object sender, RoutedEventArgs e)
    {
        IsRemote = IsRemoteMode;
        SessionName = NameBox.Text.Trim();

        if (IsRemote)
        {
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
                var parts = CustomArgsBox.Text.Trim().Split(' ', 2);
                SelectedCommand = parts.Length > 0 ? parts[0] : "bash";
                SelectedArgs = parts.Length > 1 ? parts[1] : "";
            }
            else
            {
                var parts = selectedTag.Split(' ', 2);
                SelectedCommand = parts.Length > 0 ? parts[0] : "bash";
                SelectedArgs = parts.Length > 1 ? parts[1] : "";
            }

            SelectedFolder = "";
        }
        else
        {
            SelectedFolder = FolderBox.Text.Trim();

            var selectedTag = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "claude";
            if (selectedTag == "custom")
            {
                var parts = CustomArgsBox.Text.Trim().Split(' ', 2);
                SelectedCommand = parts.Length > 0 ? parts[0] : "claude";
                SelectedArgs = parts.Length > 1 ? parts[1] : "";
            }
            else
            {
                var parts = selectedTag.Split(' ', 2);
                SelectedCommand = parts.Length > 0 ? parts[0] : "claude";
                SelectedArgs = parts.Length > 1 ? parts[1] : "";
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

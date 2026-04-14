using System.Collections.Generic;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using CodeShellManager.Models;

namespace CodeShellManager.Views;

public partial class NewSessionDialog : Window
{
    private static readonly string[] DefaultCommands =
    [
        "claude", "claude --continue", "claude --model claude-opus-4-6",
        "claude --dangerously-skip-permissions", "codex", "gh copilot suggest", "pwsh", "cmd"
    ];

    public string SelectedFolder { get; private set; } = "";
    public string SelectedCommand { get; private set; } = "claude";
    public string SelectedArgs { get; private set; } = "";
    public string SessionName { get; private set; } = "";
    public string SelectedGroupId { get; private set; } = "";

    public NewSessionDialog(IEnumerable<SessionGroup> groups, string defaultFolder = "",
        IEnumerable<string>? launchCommands = null)
    {
        InitializeComponent();
        FolderBox.Text = defaultFolder;

        // Populate command list from settings (insert before the [custom] item)
        var customItem = CommandCombo.Items[0]; // [custom] placeholder
        CommandCombo.Items.Clear();
        foreach (var cmd in launchCommands ?? DefaultCommands)
            CommandCombo.Items.Add(new ComboBoxItem { Content = cmd, Tag = cmd });
        CommandCombo.Items.Add(customItem);
        CommandCombo.SelectedIndex = 0;

        foreach (var g in groups)
        {
            GroupCombo.Items.Add(new ComboBoxItem { Content = g.Name, Tag = g.Id });
        }
        if (GroupCombo.Items.Count > 0)
            GroupCombo.SelectedIndex = 0;

        FolderBox.TextChanged += (_, _) => AutoFillName();
    }

    private void AutoFillName()
    {
        if (string.IsNullOrWhiteSpace(NameBox.Text) && !string.IsNullOrWhiteSpace(FolderBox.Text))
        {
            try { NameBox.Text = Path.GetFileName(FolderBox.Text.TrimEnd('/', '\\')); }
            catch { }
        }
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
        SelectedFolder = FolderBox.Text.Trim();
        SessionName = NameBox.Text.Trim();

        var selectedTag = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "claude";
        if (selectedTag == "custom")
        {
            var parts = (CustomArgsBox.Text.Trim()).Split(' ', 2);
            SelectedCommand = parts.Length > 0 ? parts[0] : "claude";
            SelectedArgs = parts.Length > 1 ? parts[1] : "";
        }
        else
        {
            var parts = selectedTag.Split(' ', 2);
            SelectedCommand = parts.Length > 0 ? parts[0] : "claude";
            SelectedArgs = parts.Length > 1 ? parts[1] : "";
        }

        SelectedGroupId = (GroupCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "";

        DialogResult = true;
        Close();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }
}

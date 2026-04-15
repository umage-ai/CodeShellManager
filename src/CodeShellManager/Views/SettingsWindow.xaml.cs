using System.Collections.Generic;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using CodeShellManager.Models;

namespace CodeShellManager.Views;

public partial class SettingsWindow : Window
{
    private readonly AppSettings _edited;

    public AppSettings EditedSettings => _edited;

    public SettingsWindow(AppSettings current)
    {
        InitializeComponent();

        // Clone the settings so we edit a copy
        _edited = new AppSettings
        {
            AutoRestoreSessions = current.AutoRestoreSessions,
            ShowToastNotifications = current.ShowToastNotifications,
            AnthropicApiKey = current.AnthropicApiKey,
            DefaultCommand = current.DefaultCommand,
            DefaultWorkingFolder = current.DefaultWorkingFolder,
            ShowGitBranch = current.ShowGitBranch,
            SearchCollapseAfterNavigate = current.SearchCollapseAfterNavigate,
            Theme = current.Theme,
            MaxSearchResults = current.MaxSearchResults,
            ShowTerminalStatusDot = current.ShowTerminalStatusDot,
            TerminalFontFamily = current.TerminalFontFamily,
            TerminalFontSize = current.TerminalFontSize,
            TerminalFontLigatures = current.TerminalFontLigatures,
            TerminalFontWeight = current.TerminalFontWeight,
            TerminalLetterSpacing = current.TerminalLetterSpacing,
            TerminalLineHeight = current.TerminalLineHeight,
        };

        // Populate controls
        DefaultFolderBox.Text = _edited.DefaultWorkingFolder;
        AutoRestoreCheck.IsChecked = _edited.AutoRestoreSessions;
        ShowToastCheck.IsChecked = _edited.ShowToastNotifications;
        ShowGitBranchCheck.IsChecked = _edited.ShowGitBranch;
        ShowTerminalStatusDotCheck.IsChecked = _edited.ShowTerminalStatusDot;
        SearchCollapseAfterNavigateCheck.IsChecked = _edited.SearchCollapseAfterNavigate;
        MaxSearchResultsBox.Text = _edited.MaxSearchResults.ToString();
        ApiKeyBox.Password = _edited.AnthropicApiKey;
        LaunchCommandsBox.Text = string.Join("\r\n", _edited.LaunchCommands);

        // Terminal font controls
        FontFamilyBox.Text = _edited.TerminalFontFamily;
        FontSizeBox.Text = _edited.TerminalFontSize.ToString();
        FontLigaturesCheck.IsChecked = _edited.TerminalFontLigatures;
        LetterSpacingBox.Text = _edited.TerminalLetterSpacing.ToString("G");
        LineHeightBox.Text = _edited.TerminalLineHeight.ToString("G");

        foreach (ComboBoxItem item in FontWeightCombo.Items)
        {
            if (item.Tag?.ToString() == _edited.TerminalFontWeight)
            {
                FontWeightCombo.SelectedItem = item;
                break;
            }
        }
        if (FontWeightCombo.SelectedIndex < 0)
            FontWeightCombo.SelectedIndex = 0;

        // Select matching command in ComboBox
        foreach (ComboBoxItem item in DefaultCommandCombo.Items)
        {
            if (item.Tag?.ToString() == _edited.DefaultCommand)
            {
                DefaultCommandCombo.SelectedItem = item;
                break;
            }
        }
        if (DefaultCommandCombo.SelectedIndex < 0)
            DefaultCommandCombo.SelectedIndex = 0;
    }

    private void BrowseFolderButton_Click(object sender, RoutedEventArgs e)
    {
        using var dialog = new System.Windows.Forms.FolderBrowserDialog
        {
            Description = "Select default working folder",
            UseDescriptionForTitle = true,
            SelectedPath = DefaultFolderBox.Text
        };
        if (dialog.ShowDialog() == System.Windows.Forms.DialogResult.OK)
            DefaultFolderBox.Text = dialog.SelectedPath;
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        _edited.DefaultWorkingFolder = DefaultFolderBox.Text.Trim();
        _edited.AutoRestoreSessions = AutoRestoreCheck.IsChecked == true;
        _edited.ShowToastNotifications = ShowToastCheck.IsChecked == true;
        _edited.ShowGitBranch = ShowGitBranchCheck.IsChecked == true;
        _edited.ShowTerminalStatusDot = ShowTerminalStatusDotCheck.IsChecked == true;
        _edited.SearchCollapseAfterNavigate = SearchCollapseAfterNavigateCheck.IsChecked == true;
        _edited.AnthropicApiKey = ApiKeyBox.Password;

        if (int.TryParse(MaxSearchResultsBox.Text, out int maxResults) && maxResults > 0)
            _edited.MaxSearchResults = maxResults;

        var commands = LaunchCommandsBox.Text
            .Split('\n', System.StringSplitOptions.RemoveEmptyEntries)
            .Select(s => s.Trim().TrimEnd('\r'))
            .Where(s => s.Length > 0)
            .ToList();
        if (commands.Count > 0)
            _edited.LaunchCommands = commands;

        var selectedTag = (DefaultCommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString();
        if (!string.IsNullOrEmpty(selectedTag))
            _edited.DefaultCommand = selectedTag;

        // Terminal font settings
        _edited.TerminalFontFamily = FontFamilyBox.Text.Trim();
        _edited.TerminalFontLigatures = FontLigaturesCheck.IsChecked == true;
        if (int.TryParse(FontSizeBox.Text, out int fontSize) && fontSize >= 6 && fontSize <= 72)
            _edited.TerminalFontSize = fontSize;
        if (double.TryParse(LetterSpacingBox.Text, out double letterSpacing))
            _edited.TerminalLetterSpacing = letterSpacing;
        if (double.TryParse(LineHeightBox.Text, out double lineHeight) && lineHeight >= 0.5 && lineHeight <= 3.0)
            _edited.TerminalLineHeight = lineHeight;
        var weightTag = (FontWeightCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString();
        if (!string.IsNullOrEmpty(weightTag))
            _edited.TerminalFontWeight = weightTag;

        DialogResult = true;
        Close();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }
}

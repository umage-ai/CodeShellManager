using System.Collections.Generic;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using MessageBox = System.Windows.MessageBox;
using MessageBoxButton = System.Windows.MessageBoxButton;
using MessageBoxImage = System.Windows.MessageBoxImage;

namespace CodeShellManager.Views;

public partial class NewWorktreeDialog : Window
{
    public string RepoRoot { get; }
    public string BranchOrRef { get; private set; } = "";
    public bool CreateBranch { get; private set; } = true;
    public string TargetPath { get; private set; } = "";
    public string SessionName { get; private set; } = "";

    private readonly string _currentBranch;
    private readonly string _repoName;

    public NewWorktreeDialog(string repoRoot, string currentBranch, IReadOnlyList<string> branches)
    {
        InitializeComponent();
        RepoRoot = repoRoot;
        _currentBranch = currentBranch ?? "";
        _repoName = Path.GetFileName(repoRoot.TrimEnd('/', '\\')) ?? "repo";

        RepoLabel.Text = repoRoot;
        BaseBranchHint.Text = string.IsNullOrEmpty(_currentBranch)
            ? "Base: HEAD"
            : $"Base: {_currentBranch}";

        NewBranchBox.Text = SuggestNewBranchName();
        NewBranchBox.TextChanged += (_, _) => UpdateDerivedFields();

        ExistingBranchCombo.ItemsSource = branches;
        if (branches.Count > 0)
            ExistingBranchCombo.SelectedItem = branches.Contains(_currentBranch)
                ? _currentBranch
                : branches[0];
        ExistingBranchCombo.SelectionChanged += (_, _) => UpdateDerivedFields();

        UpdateDerivedFields();
    }

    private static string SuggestNewBranchName() =>
        $"feat/{System.DateTime.Now:yyMMdd-HHmm}";

    private string CurrentBranchInput => NewBranchRadio.IsChecked == true
        ? NewBranchBox.Text.Trim()
        : (ExistingBranchCombo.SelectedItem as string ?? "").Trim();

    private void UpdateDerivedFields()
    {
        string branch = CurrentBranchInput;
        if (string.IsNullOrEmpty(branch)) return;

        string safe = branch.Replace('/', '-').Replace(' ', '-');
        string parent = Path.GetDirectoryName(RepoRoot.TrimEnd('/', '\\'))
            ?? RepoRoot;
        TargetPathBox.Text = Path.Combine(parent, $"{_repoName}-{safe}");
        if (string.IsNullOrWhiteSpace(SessionNameBox.Text))
            SessionNameBox.Text = $"{_repoName} ⎇ {branch}";
    }

    private void BranchMode_Changed(object sender, RoutedEventArgs e)
    {
        if (NewBranchPanel == null) return;
        bool newMode = NewBranchRadio.IsChecked == true;
        NewBranchPanel.Visibility = newMode ? Visibility.Visible : Visibility.Collapsed;
        ExistingBranchPanel.Visibility = newMode ? Visibility.Collapsed : Visibility.Visible;
        // Clear auto-filled session name so the new branch choice can repopulate it.
        SessionNameBox.Text = "";
        UpdateDerivedFields();
    }

    private void Browse_Click(object sender, RoutedEventArgs e)
    {
        using var dlg = new System.Windows.Forms.FolderBrowserDialog
        {
            Description = "Select worktree folder",
            UseDescriptionForTitle = true,
            SelectedPath = TargetPathBox.Text
        };
        if (dlg.ShowDialog() == System.Windows.Forms.DialogResult.OK)
            TargetPathBox.Text = dlg.SelectedPath;
    }

    private void Ok_Click(object sender, RoutedEventArgs e)
    {
        CreateBranch = NewBranchRadio.IsChecked == true;
        BranchOrRef = CurrentBranchInput;
        TargetPath = TargetPathBox.Text.Trim();
        SessionName = SessionNameBox.Text.Trim();

        if (string.IsNullOrWhiteSpace(BranchOrRef))
        {
            MessageBox.Show(this, "Branch name is required.", "Missing branch",
                MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }
        if (string.IsNullOrWhiteSpace(TargetPath))
        {
            MessageBox.Show(this, "Worktree folder is required.", "Missing folder",
                MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }
        if (Directory.Exists(TargetPath) && Directory.EnumerateFileSystemEntries(TargetPath).GetEnumerator().MoveNext())
        {
            MessageBox.Show(this,
                $"'{TargetPath}' already exists and is non-empty. git worktree add will refuse to use it.",
                "Folder not empty", MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
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

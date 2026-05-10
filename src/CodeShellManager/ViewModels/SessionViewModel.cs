using System;
using System.Threading;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CodeShellManager.Models;
using CodeShellManager.Services;
using CodeShellManager.Terminal;

namespace CodeShellManager.ViewModels;

public partial class SessionViewModel : ObservableObject, IDisposable
{
    public ShellSession Session { get; }

    [ObservableProperty] private bool _needsAttention;
    [ObservableProperty] private string _alertMessage = "";
    [ObservableProperty] private bool _isActive;
    [ObservableProperty] private bool _isWaitingForInput;
    [ObservableProperty] private bool _isWaitingForApproval;
    [ObservableProperty] private string? _gitBranch;
    [ObservableProperty] private bool _gitIsDirty;
    [ObservableProperty] private bool _gitInfoLoaded;

    public PseudoTerminal? Pty { get; set; }
    public TerminalBridge? Bridge { get; set; }
    public AlertDetector? AlertDetector { get; set; }
    public OutputIndexer? OutputIndexer { get; set; }

    public string Id => Session.Id;
    public string Name => Session.Name;
    public string WorkingFolder => Session.WorkingFolder;
    public string Command => Session.Command;
    public string Args => Session.Args;
    public string GroupId => Session.GroupId;

    public string AccentColor => Session.ColorOverride
        ?? ColorService.GetHexColor(
            Session.IsRemote
                ? (string.IsNullOrWhiteSpace(Session.SshUser)
                    ? Session.SshHost
                    : $"{Session.SshUser}@{Session.SshHost}")
                : Session.WorkingFolder);

    public string DisplayName => string.IsNullOrWhiteSpace(Session.Name)
        ? (Session.IsRemote
            ? (string.IsNullOrWhiteSpace(Session.SshHost) ? Session.Command : Session.SshHost)
            : System.IO.Path.GetFileName(Session.WorkingFolder.TrimEnd('/', '\\')) ?? Session.Command)
        : Session.Name;

    public string FolderShort
    {
        get
        {
            if (string.IsNullOrEmpty(Session.WorkingFolder)) return "";
            var di = new System.IO.DirectoryInfo(Session.WorkingFolder);
            return di.Name;
        }
    }

    public event Action<SessionViewModel>? CloseRequested;

    private readonly CancellationTokenSource _gitPollCts = new();

    public SessionViewModel(ShellSession session)
    {
        Session = session;
        _ = RefreshGitInfoAsync();
        _ = PollGitInfoAsync(_gitPollCts.Token);
    }

    public async Task RefreshGitInfoAsync()
    {
        if (Session.IsRemote) return;
        var (branch, isDirty) = await GitService.GetGitInfoAsync(Session.WorkingFolder);
        GitBranch = branch;
        GitIsDirty = isDirty;
        GitInfoLoaded = true;
    }

    private async Task PollGitInfoAsync(CancellationToken ct)
    {
        using var timer = new PeriodicTimer(TimeSpan.FromSeconds(10));
        try
        {
            while (await timer.WaitForNextTickAsync(ct))
                await RefreshGitInfoAsync();
        }
        catch (OperationCanceledException) { }
    }

    [RelayCommand]
    private void Close() => CloseRequested?.Invoke(this);

    [RelayCommand]
    private void OpenInExplorer()
    {
        if (System.IO.Directory.Exists(Session.WorkingFolder))
            System.Diagnostics.Process.Start("explorer.exe", Session.WorkingFolder);
    }

    /// <summary>
    /// Applies a CSM shell-integration payload (OSC 9001) emitted by the running program.
    /// Recognised keys: <c>color</c> (#rrggbb), <c>git-branch</c>, <c>git-dirty</c> (0/1),
    /// <c>title</c>. Unknown keys are ignored. Useful for SSH overlays whose remote
    /// state CSM cannot inspect locally.
    /// </summary>
    public void ApplyShellIntegration(System.Collections.Generic.IReadOnlyDictionary<string, string> fields)
    {
        if (fields.TryGetValue("color", out var color) && IsValidHexColor(color))
        {
            Session.ColorOverride = color;
            OnPropertyChanged(nameof(AccentColor));
        }

        if (fields.TryGetValue("git-branch", out var branch))
        {
            GitBranch = string.IsNullOrWhiteSpace(branch) ? null : branch;
            GitInfoLoaded = true;
        }

        if (fields.TryGetValue("git-dirty", out var dirty))
            GitIsDirty = dirty == "1" || string.Equals(dirty, "true", StringComparison.OrdinalIgnoreCase);

        if (fields.TryGetValue("title", out var title) && !string.IsNullOrWhiteSpace(title))
            Rename(title.Trim());
    }

    private static bool IsValidHexColor(string s)
    {
        if (string.IsNullOrEmpty(s) || s[0] != '#') return false;
        if (s.Length != 4 && s.Length != 7 && s.Length != 9) return false;
        for (int i = 1; i < s.Length; i++)
            if (!Uri.IsHexDigit(s[i])) return false;
        return true;
    }

    public void RaiseAlert(string message, AlertType alertType = AlertType.InputRequired)
    {
        NeedsAttention = true;
        AlertMessage = message;
        IsWaitingForInput = alertType == AlertType.InputRequired;
        IsWaitingForApproval = alertType == AlertType.ToolApproval;
    }

    public void Rename(string newName)
    {
        Session.Name = newName;
        OnPropertyChanged(nameof(Name));
        OnPropertyChanged(nameof(DisplayName));
    }

    public void ClearAlert()
    {
        NeedsAttention = false;
        AlertMessage = "";
        IsWaitingForInput = false;
        IsWaitingForApproval = false;
    }

    public void Dispose()
    {
        _gitPollCts.Cancel();
        _gitPollCts.Dispose();
        AlertDetector?.Dispose();
        OutputIndexer?.Dispose();
        Bridge?.Dispose();
        Pty?.Dispose();
    }
}

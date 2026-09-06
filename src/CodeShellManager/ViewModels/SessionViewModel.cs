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
    /// <summary>Absolute path to the session's repo top-level, or null if the working folder is not in a git repo.</summary>
    [ObservableProperty] private string? _repoRoot;
    /// <summary>Set by MainWindow whenever another live session shares this session's RepoRoot.</summary>
    [ObservableProperty] private bool _hasWorktreeSiblings;

    public PseudoTerminal? Pty { get; set; }
    public TerminalBridge? Bridge { get; set; }
    public AlertDetector? AlertDetector { get; set; }
    public OutputIndexer? OutputIndexer { get; set; }
    public SessionRunner Runner { get; }

    public string Id => Session.Id;
    public string Name => Session.Name;
    public string WorkingFolder => Session.WorkingFolder;
    public string Command => Session.Command;
    public string Args => Session.Args;
    public string GroupId => Session.GroupId;

    public string AccentColor => Session.ColorOverride
        ?? ColorService.GetHexColor(
            // SSH never gets a RepoRoot override (no local filesystem); for Local + WSL
            // prefer RepoRoot so worktree siblings share a color, falling back to
            // the kind-specific accent key.
            Session.Kind == SessionKind.Ssh
                ? Session.AccentKey
                : (string.IsNullOrEmpty(RepoRoot) ? Session.AccentKey : RepoRoot));

    partial void OnRepoRootChanged(string? value) => OnPropertyChanged(nameof(AccentColor));

    public string DisplayName => string.IsNullOrWhiteSpace(Session.Name)
        ? Session.DefaultDisplayName
        : Session.Name;

    public string FolderShort => Session.FolderShort;

    public event Action<SessionViewModel>? CloseRequested;

    private readonly CancellationTokenSource _gitPollCts = new();

    // Set by ApplyShellIntegration when the running program pushes git-branch
    // or git-dirty via OSC 9001. Tells the local poller to stand down: the
    // program is sourcing its own git state (e.g. `nexus ssh` into a container
    // whose /workspace branch is unrelated to the host CWD) and the local
    // poll would otherwise clobber the OSC value every 10s. Sticky for the
    // lifetime of the session — once a program declares itself the source of
    // truth, we trust it.
    private bool _gitOverriddenByOsc;

    public SessionViewModel(ShellSession session)
    {
        Session = session;
        Runner = new SessionRunner(session);
        _ = RefreshGitInfoAsync();
        _ = PollGitInfoAsync(_gitPollCts.Token);
    }

    /// <summary>
    /// Git poll cadence per kind. WSL probes spawn wsl.exe (much heavier than a local git
    /// spawn) and defeat WSL2's idle-VM shutdown, so they run a third as often.
    /// </summary>
    internal static TimeSpan GitPollIntervalFor(SessionKind kind) =>
        kind == SessionKind.Wsl ? TimeSpan.FromSeconds(30) : TimeSpan.FromSeconds(10);

    // WSL only: a "not a repo" answer costs a wsl.exe spawn per tick, so remember it.
    // Local folders keep re-probing (a `git init` should be picked up within a tick).
    private bool _repoRootProbedNegative;

    public async Task RefreshGitInfoAsync()
    {
        // SSH sessions have no local working folder to inspect. WSL sessions store
        // their WorkingFolder as a `\\wsl$\<distro>\...` UNC; GitService detects that
        // and dispatches to `wsl.exe -- git -C <linuxPath>` internally (Git for
        // Windows itself trips on those UNCs — dubious-ownership / .git symlinks).
        if (Session.Kind == SessionKind.Ssh || _gitOverriddenByOsc) return;

        // Off the dispatcher: GitService begins with a synchronous Directory.Exists, and on
        // a \\wsl$ share that boots a stopped distro (seconds). Continuations return to the
        // captured UI context, so the property sets below stay on the UI thread.
        string folder = Session.WorkingFolder;
        var (branch, isDirty) = await Task.Run(() => GitService.GetGitInfoAsync(folder));
        GitBranch = branch;
        GitIsDirty = isDirty;
        GitInfoLoaded = true;

        // RepoRoot is stable for the life of the session — resolve it once. Don't gate on
        // a non-empty branch: detached HEADs report no branch but are still valid repos
        // that should participate in sibling detection, shared accent color, and clusters.
        if (RepoRoot == null && !_repoRootProbedNegative)
        {
            RepoRoot = await Task.Run(() => GitService.GetRepoRootAsync(folder));
            if (RepoRoot == null && Session.Kind == SessionKind.Wsl) _repoRootProbedNegative = true;
        }
    }

    /// <summary>Short repo + branch label shown beneath the session name when sibling worktrees are open.</summary>
    public string WorktreeSubtitle
    {
        get
        {
            if (string.IsNullOrEmpty(RepoRoot)) return "";
            string repoName = System.IO.Path.GetFileName(RepoRoot.TrimEnd('/', '\\')) ?? "";
            string branch = string.IsNullOrEmpty(GitBranch) ? "—" : GitBranch;
            return $"\U0001F4C1 {repoName} ⎇ {branch}";
        }
    }

    private async Task PollGitInfoAsync(CancellationToken ct)
    {
        using var timer = new PeriodicTimer(GitPollIntervalFor(Session.Kind));
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
    /// Recognised keys: <c>color</c> (#rrggbb / #aarrggbb), <c>git-branch</c>, <c>git-dirty</c> (0/1),
    /// <c>title</c>. Unknown keys are ignored. Useful for SSH overlays whose remote
    /// state CSM cannot inspect locally.
    /// </summary>
    public void ApplyShellIntegration(System.Collections.Generic.IReadOnlyDictionary<string, string> fields)
    {
        // Every value is untrusted terminal output — validation lives in ShellIntegrationPayload.
        if (fields.TryGetValue("color", out var color)
            && ShellIntegrationPayload.TryNormalizeColor(color, out var wpfHex))
        {
            Session.ColorOverride = wpfHex;
            OnPropertyChanged(nameof(AccentColor));
        }

        if (fields.TryGetValue("git-branch", out var branch))
        {
            GitBranch = ShellIntegrationPayload.SanitizeBranch(branch);
            GitInfoLoaded = true;
            _gitOverriddenByOsc = true;
        }

        if (fields.TryGetValue("git-dirty", out var dirty))
        {
            GitIsDirty = ShellIntegrationPayload.ParseDirty(dirty);
            _gitOverriddenByOsc = true;
        }

        if (fields.TryGetValue("title", out var title)
            && ShellIntegrationPayload.SanitizeTitle(title) is { } cleanTitle)
            Rename(cleanTitle);
    }

    /// <summary>
    /// Drops a <see cref="ShellSession.ColorOverride"/> so the accent falls back to the
    /// hash-derived colour. OSC 9001 is currently the only writer of that field and it
    /// persists across sleep/wake and restart, so without this a program that recoloured
    /// a session once would own its colour forever.
    /// </summary>
    public void ClearColorOverride()
    {
        if (Session.ColorOverride is null) return;
        Session.ColorOverride = null;
        OnPropertyChanged(nameof(AccentColor));
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

    /// <summary>
    /// Re-raises every property that mirrors a field on <see cref="Session"/>. Call after
    /// editing the model in place (see <c>MainWindow.EditSessionAsync</c>) so the sidebar row
    /// and terminal toolbar pick up the new name / folder / command without being rebuilt.
    /// </summary>
    public void NotifyConfigChanged()
    {
        OnPropertyChanged(nameof(Name));
        OnPropertyChanged(nameof(DisplayName));
        OnPropertyChanged(nameof(WorkingFolder));
        OnPropertyChanged(nameof(FolderShort));
        OnPropertyChanged(nameof(Command));
        OnPropertyChanged(nameof(Args));
        OnPropertyChanged(nameof(AccentColor));
        OnPropertyChanged(nameof(WorktreeSubtitle));
    }

    /// <summary>
    /// Drops the cached git info and re-probes it. <see cref="RefreshGitInfoAsync"/> resolves
    /// <see cref="RepoRoot"/> only once (it's stable for the life of a session), so a working
    /// folder that changed under us needs the cache cleared first.
    /// </summary>
    public Task ReloadGitInfoAsync()
    {
        RepoRoot = null;
        GitBranch = null;
        GitIsDirty = false;
        GitInfoLoaded = false;
        HasWorktreeSiblings = false;
        // The user just pointed this session at a different folder, so whatever program
        // pushed git info via OSC 9001 was describing the old one. Let the local poller
        // back in until a program re-declares itself.
        _gitOverriddenByOsc = false;
        // Same reason: a "not a repo" answer for the old folder doesn't apply to the new one.
        _repoRootProbedNegative = false;
        return RefreshGitInfoAsync();
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
        Runner.Dispose();
        _gitPollCts.Cancel();
        _gitPollCts.Dispose();
        AlertDetector?.Dispose();
        OutputIndexer?.Dispose();
        Bridge?.Dispose();
        Pty?.Dispose();
    }
}

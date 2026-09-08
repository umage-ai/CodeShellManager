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
    // Hand-written rather than [ObservableProperty] so all three share ONE change
    // notification, GitInfoVersion. As generated properties they raised three events per
    // poll per session — 141 sidebar rebuilds per cycle at 47 sessions (issue #70).
    private string? _gitBranch;
    public string? GitBranch
    {
        get => _gitBranch;
        set { if (_gitBranch != value) { _gitBranch = value; BumpGitInfo(); } }
    }

    private bool _gitIsDirty;
    public bool GitIsDirty
    {
        get => _gitIsDirty;
        set { if (_gitIsDirty != value) { _gitIsDirty = value; BumpGitInfo(); } }
    }

    private bool _gitInfoLoaded;
    public bool GitInfoLoaded
    {
        get => _gitInfoLoaded;
        set { if (_gitInfoLoaded != value) { _gitInfoLoaded = value; BumpGitInfo(); } }
    }
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

        // Captured here, on the UI thread, so the watcher callback (which fires on a
        // threadpool thread) can hop back before touching properties.
        _uiContext = SynchronizationContext.Current;

        // These intentionally keep the UI context: RefreshGitInfoAsync pushes the actual
        // probe off-thread itself and resumes here to set properties. What made this
        // dangerous before was GitService running Process.Start on whichever thread called
        // in — fixed inside GitService, so capturing the context is safe again (issue #70).
        _ = RefreshGitInfoAsync();
        _ = PollGitInfoAsync(_gitPollCts.Token);
        StartGitWatcher();
    }

    /// <summary>
    /// Git poll cadence, by kind and by whether the pane is the one on screen.
    ///
    /// Kind: WSL probes spawn wsl.exe (much heavier than a local git spawn) and defeat
    /// WSL2's idle-VM shutdown, so they run a third as often.
    ///
    /// Foreground: everything you are not looking at backs off hard. A poll is ~all process
    /// creation — `git --version` costs 42ms against `branch --show-current` at 41ms — so
    /// 46 background sessions polling every 10s was pure overhead to learn nothing had
    /// changed. Real git operations still arrive immediately via <see cref="GitRepoWatcher"/>;
    /// the slow poll only has to catch working-tree edits, which dirty `status` without
    /// touching anything under .git (issue #70).
    /// </summary>
    internal static TimeSpan GitPollIntervalFor(SessionKind kind, bool isForeground)
    {
        if (!isForeground) return TimeSpan.FromSeconds(120);
        return kind == SessionKind.Wsl ? TimeSpan.FromSeconds(30) : TimeSpan.FromSeconds(10);
    }

    // WSL only: a "not a repo" answer costs a wsl.exe spawn per tick, so remember it.
    // Local folders keep re-probing (a `git init` should be picked up within a tick).
    private bool _repoRootProbedNegative;

    public async Task RefreshGitInfoAsync()
    {
        // SSH sessions have no local working folder to inspect. WSL sessions store
        // their WorkingFolder as a `\\wsl$\<distro>\...` UNC; GitService detects that
        // and dispatches to `wsl.exe -d <distro> -e sh -lc ... git -C <linuxPath>` internally (Git for
        // Windows itself trips on those UNCs — dubious-ownership / .git symlinks).
        if (Session.Kind == SessionKind.Ssh || _gitOverriddenByOsc) return;

        // Captured before the probe goes off-thread, not read from _gitPollCts afterwards:
        // Dispose() cancels (then disposes) that CTS if the session closes while the
        // Task.Run below is still in flight, and CancellationToken.IsCancellationRequested
        // never throws even once the source is disposed, so this stays safe either way.
        var token = _gitPollCts.Token;

        // Off the dispatcher: GitService begins with a synchronous Directory.Exists, and on
        // a \\wsl$ share that boots a stopped distro (seconds).
        //
        // Deliberately NOT ConfigureAwait(false): when a caller has a UI context, the
        // property sets below resume on it. GitService is separately hardened so no git
        // work can run on the caller's thread regardless (issue #70) — this Task.Run covers
        // the synchronous Directory.Exists prefix that sits outside it, and keeping the
        // resume on the UI thread is what lets the sets below notify WPF safely.
        string folder = Session.WorkingFolder;
        var (branch, isDirty) = await Task.Run(() => GitService.GetGitInfoAsync(folder));
        if (token.IsCancellationRequested) return; // session closed while the probe was off-thread
        ApplyGitInfo(branch, isDirty);

        // RepoRoot is stable for the life of the session — resolve it once. Don't gate on
        // a non-empty branch: detached HEADs report no branch but are still valid repos
        // that should participate in sibling detection, shared accent color, and clusters.
        if (RepoRoot == null && !_repoRootProbedNegative)
        {
            string? repoRoot = await Task.Run(() => GitService.GetRepoRootAsync(folder));
            if (token.IsCancellationRequested) return; // session closed while the probe was off-thread
            RepoRoot = repoRoot;
            if (RepoRoot == null && Session.Kind == SessionKind.Wsl) _repoRootProbedNegative = true;
        }
    }

    /// <summary>
    /// Applies a git poll result as ONE change notification instead of three (issue #70).
    ///
    /// GitBranch, GitIsDirty and GitInfoLoaded are three <c>[ObservableProperty]</c> writes,
    /// so every poll raised three PropertyChanged events per session, each crossing a
    /// blocking Dispatcher.Invoke and rebuilding the sidebar row's WPF inlines — 141 rebuilds
    /// per cycle at 47 sessions, to redraw text that almost never differs.
    ///
    /// The early-out matters more than the coalescing: a branch changes maybe once an hour,
    /// so the overwhelmingly common poll result is "identical to last time", and that now
    /// costs no UI work at all.
    /// </summary>
    public void ApplyGitInfo(string? branch, bool isDirty)
    {
        if (_gitInfoLoaded && _gitBranch == branch && _gitIsDirty == isDirty) return;

        // Backing fields directly — the generated setters would raise one event each.
        _gitBranch = branch;
        _gitIsDirty = isDirty;
        _gitInfoLoaded = true;

        BumpGitInfo();
    }

    /// <summary>
    /// The single change notification for git state. Watch this rather than GitBranch /
    /// GitIsDirty / GitInfoLoaded, none of which raise events of their own.
    /// </summary>
    public int GitInfoVersion => _gitInfoVersion;
    private int _gitInfoVersion;

    private void BumpGitInfo()
    {
        _gitInfoVersion++;
        OnPropertyChanged(nameof(GitInfoVersion));
    }

    /// <summary>
    /// True while this session's pane is the active one. Set by MainWindow alongside
    /// <c>TerminalBridge.IsForeground</c>. Governs poll cadence, and forces an immediate
    /// refresh on activation so switching to a pane shows current git state at once rather
    /// than up to <see cref="BackgroundPollMs"/> later.
    /// </summary>
    public bool IsForegroundSession
    {
        get => _isForegroundSession;
        set
        {
            if (_isForegroundSession == value) return;
            _isForegroundSession = value;
            if (value) _ = Task.Run(() => RefreshGitInfoAsync());
        }
    }
    private bool _isForegroundSession;

    // The pane you're looking at keeps the old cadence. Everything else backs off hard: the
    // watcher catches real git operations immediately, so this slow poll only has to catch
    // working-tree edits that dirty the tree without touching anything under .git.
    private const int ForegroundPollMs = 10_000;
    private const int BackgroundPollMs = 120_000;

    private GitRepoWatcher? _gitWatcher;
    private readonly SynchronizationContext? _uiContext;

    private void StartGitWatcher()
    {
        // Local only. SSH has no local filesystem, and a WSL session's folder is a
        // `\\wsl$\...` UNC — watching that keeps the distro's 9p server busy and defeats
        // the idle-VM shutdown the WSL cadence above exists to protect.
        if (Session.Kind != SessionKind.Local) return;
        _gitWatcher = GitRepoWatcher.Acquire(Session.WorkingFolder);
        if (_gitWatcher != null) _gitWatcher.Changed += OnGitDirChanged;
        // null is normal: a plain folder, or a platform that refused the watch. Poll only.
    }

    private void OnGitDirChanged()
    {
        if (_gitPollCts.IsCancellationRequested) return;

        // The watcher fires on a threadpool thread. Hop back to the context this VM was
        // created on so RefreshGitInfoAsync's property sets land on the UI thread, exactly
        // as they do on the poll path.
        if (_uiContext != null) _uiContext.Post(_ => { _ = RefreshGitInfoAsync(); }, null);
        else _ = RefreshGitInfoAsync();
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
        try
        {
            while (!ct.IsCancellationRequested)
            {
                // Interval is recomputed each iteration rather than fixed by a PeriodicTimer,
                // so promoting a session to the foreground speeds up its next poll without
                // restarting the loop.
                await Task.Delay(GitPollIntervalFor(Session.Kind, IsForegroundSession), ct);
                await RefreshGitInfoAsync();
            }
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

    private bool _disposed;

    public void Dispose()
    {
        // Idempotent. MainWindow disposes a session VM from several paths (close, sleep,
        // restart, shutdown) and some of them can both run for one session; a second call
        // used to throw ObjectDisposedException on the CTS below, *before* reaching the
        // watcher release — leaking a shared watcher reference on the way out.
        if (_disposed) return;
        _disposed = true;

        Runner.Dispose();
        _gitPollCts.Cancel();
        _gitPollCts.Dispose();
        if (_gitWatcher != null)
        {
            _gitWatcher.Changed -= OnGitDirChanged;
            // Release, not Dispose — the watcher is shared with any other session in the
            // same repo and only the last one out disposes it.
            GitRepoWatcher.Release(_gitWatcher);
            _gitWatcher = null;
        }
        AlertDetector?.Dispose();
        OutputIndexer?.Dispose();
        Bridge?.Dispose();
        Pty?.Dispose();
    }
}

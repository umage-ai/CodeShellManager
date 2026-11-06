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
    // Sync state for the toolbar git chip. Same rule as the three above: plain fields,
    // announced only through GitInfoVersion.
    /// <summary>Tracking branch ("origin/main"), or null when the branch tracks nothing.</summary>
    public string? GitUpstream { get; private set; }
    /// <summary>Commits ahead of / behind <see cref="GitUpstream"/>; null without an upstream.</summary>
    public int? GitAhead { get; private set; }
    public int? GitBehind { get; private set; }
    /// <summary>Number of changed paths (staged, unstaged and untracked).</summary>
    public int GitChangeCount { get; private set; }

    /// <summary>Absolute path to the session's repo top-level, or null if the working folder is not in a git repo.</summary>
    [ObservableProperty] private string? _repoRoot;
    /// <summary>Set by MainWindow whenever another live session shares this session's RepoRoot.</summary>
    [ObservableProperty] private bool _hasWorktreeSiblings;

    public PseudoTerminal? Pty { get; set; }
    public TerminalBridge? Bridge { get; set; }
    public AlertDetector? AlertDetector { get; set; }
    public OutputIndexer? OutputIndexer { get; set; }
    public SessionRunner Runner { get; }

    // UTC ticks of the last PTY output chunk; 0 = none yet. Written from the PTY reader
    // thread on every chunk, read on the UI thread at app close — hence a plain long with
    // Volatile access rather than a property with change notification.
    private long _lastOutputTicks;

    /// <summary>Called from the PTY output path (any thread). Allocation-free.</summary>
    public void NoteOutput() => Volatile.Write(ref _lastOutputTicks, DateTime.UtcNow.Ticks);

    /// <summary>When the session last produced PTY output, or null if it never has.</summary>
    public DateTime? LastOutputUtc
    {
        get
        {
            long t = Volatile.Read(ref _lastOutputTicks);
            return t == 0 ? null : new DateTime(t, DateTimeKind.Utc);
        }
    }

    /// <summary>
    /// Why closing the app now would interrupt this session, or null when it is idle.
    /// See <see cref="SessionBusyRules"/>.
    /// </summary>
    public string? BusyReason(DateTime nowUtc)
    {
        int running = 0;
        foreach (var inst in Runner.Instances.Values)
            if (inst.State == RunState.Running) running++;
        return SessionBusyRules.Reason(HasProgramStatus, AgentState, WorkProgress,
            running, LastOutputUtc, nowUtc);
    }

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
        // One spawn for branch, dirty, upstream and ahead/behind (it used to be two for just
        // branch + dirty). A detached HEAD keeps reporting "no branch, clean" as it always has.
        var status = await Task.Run(() => GitService.GetStatusAsync(folder));
        if (token.IsCancellationRequested) return; // session closed while the probe was off-thread
        if (status is null || status.IsDetached || string.IsNullOrEmpty(status.Branch))
            ApplyGitInfo(null, false);
        else
            ApplyGitInfo(status.Branch, status.IsDirty, status.Upstream,
                status.Ahead, status.Behind, status.Changes.Count);

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
    public void ApplyGitInfo(string? branch, bool isDirty) =>
        ApplyGitInfo(branch, isDirty, upstream: null, ahead: null, behind: null, changeCount: isDirty ? 1 : 0);

    /// <inheritdoc cref="ApplyGitInfo(string?, bool)"/>
    public void ApplyGitInfo(string? branch, bool isDirty, string? upstream, int? ahead, int? behind, int changeCount)
    {
        if (_gitInfoLoaded && _gitBranch == branch && _gitIsDirty == isDirty
            && GitUpstream == upstream && GitAhead == ahead && GitBehind == behind
            && GitChangeCount == changeCount) return;

        // Backing fields directly — the generated setters would raise one event each.
        _gitBranch = branch;
        _gitIsDirty = isDirty;
        _gitInfoLoaded = true;
        GitUpstream = upstream;
        GitAhead = ahead;
        GitBehind = behind;
        GitChangeCount = changeCount;

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

    /// <summary>Releases the current watcher and acquires one for the session's folder.</summary>
    private void RestartGitWatcher()
    {
        // Never re-acquire after Dispose. ReloadGitInfoAsync is reachable from an edit that
        // races a close, and acquiring there would take a reference on the shared watcher
        // that nothing ever releases.
        if (_disposed) return;

        if (_gitWatcher != null)
        {
            _gitWatcher.Changed -= OnGitDirChanged;
            GitRepoWatcher.Release(_gitWatcher);
            _gitWatcher = null;
        }
        StartGitWatcher();
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

    /// <summary>
    /// Opens the folder via <see cref="FolderOpener"/> — not <c>explorer.exe &lt;folder&gt;</c>,
    /// which silently opened Documents for forward-slash paths and names with a comma. Off the
    /// UI thread: the existence check is \\wsl$ I/O for a WSL session.
    /// </summary>
    [RelayCommand]
    private void OpenInExplorer() => _ = FolderOpener.OpenAsync(Session.WorkingFolder);

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

    // ── OSC 7501 program status ────────────────────────────────────────────────
    // Runtime-only: never persisted. A relaunch builds a new VM, so everything here —
    // including the AlertDetector stand-down — resets naturally on restart / wake.

    private readonly ProgramStatusRecords _programStatus = new();
    private ProgramStatusSummary _programSummary = ProgramStatusSummary.Empty;
    private DateTime _lastProgramToastUtc = DateTime.MinValue;
    private int _programStatusVersion;

    /// <summary>
    /// Minimum gap between two program-status toasts for one session. The spec asks terminals
    /// to rate-limit external effects; a program flipping blocked → working → blocked must not
    /// produce a balloon per flip.
    /// </summary>
    internal static readonly TimeSpan ProgramStatusToastInterval = TimeSpan.FromSeconds(5);

    /// <summary>
    /// True once the program has sent any valid OSC 7501 report. From then on the reported
    /// state is authoritative and <see cref="AlertDetector"/>'s heuristic stands down.
    /// </summary>
    public bool HasProgramStatus { get; private set; }

    /// <summary>The aggregate reported state, or null when no records are held.</summary>
    public ProgramState? AgentState => _programSummary.State;

    /// <summary>The program reports it is working (the aggregate state is <c>working</c>).</summary>
    public bool IsWorking => _programSummary.State == ProgramState.Working;

    /// <summary>0–100 while working with a known progress; null when indeterminate or not working.</summary>
    public int? WorkProgress => IsWorking ? _programSummary.Progress : null;

    /// <summary>The program's decoded <c>msg</c> for the winning record, already sanitised.</summary>
    public string? ProgramStatusMessage => _programSummary.Message;

    /// <summary>
    /// One notification for <see cref="IsWorking"/>, <see cref="WorkProgress"/>,
    /// <see cref="AgentState"/> and <see cref="ProgramStatusMessage"/> — same reasoning as
    /// <see cref="GitInfoVersion"/>: one row repaint per change, not one per property.
    /// </summary>
    public int ProgramStatusVersion => _programStatusVersion;

    /// <summary>
    /// Applies one raw OSC 7501 payload. Invalid reports are ignored entirely (and do not
    /// count as "the program speaks 7501"). Returns a toast-worthy notice when the session
    /// just entered an attention state (blocked / done / error) and the per-session rate
    /// limit allows one; otherwise null.
    /// </summary>
    public ProgramStatusNotice? ApplyProgramStatus(string payload, DateTime utcNow)
    {
        var report = ProgramStatusPayload.TryParse(payload);
        if (report is null) return null;

        // Exit race: web messages queued behind the PTY's exit describe a process that is
        // gone. A live state from it would never be retired (no heartbeat, no further exit),
        // leaving a stuck working/blocked indicator. Results and clears are still honoured.
        if (_processExited && report.State is ProgramState.Working or ProgramState.Blocked or ProgramState.Idle)
            return null;

        bool first = !HasProgramStatus;
        if (first)
        {
            HasProgramStatus = true;
            AlertDetector?.StandDown();
        }
        _programStatus.Apply(report, out bool entered);
        string? enteredMessage = entered
            ? DescribeProgramStatus(report.State, report.Kind, report.Message)
            : null;
        SyncProgramStatus(force: first, enteredMessage);

        // A clear that emptied the store hands the session back to the heuristic.
        if (report.State == ProgramState.Clear && _programStatus.Count == 0) RearmHeuristic();

        if (!entered) return null;
        if (utcNow - _lastProgramToastUtc < ProgramStatusToastInterval) return null;
        _lastProgramToastUtc = utcNow;
        return new ProgramStatusNotice(enteredMessage!);
    }

    /// <summary>
    /// A new shell prompt began (OSC 133 A), or the process exited. Drops the live records
    /// per the spec's lifetime rule; done/error survive until acknowledged. If nothing is
    /// left, the heuristic is re-armed.
    /// </summary>
    public void DropTransientProgramStatus()
    {
        if (!HasProgramStatus) return;
        if (_programStatus.DropTransient()) SyncProgramStatus(force: false, enteredMessage: null);
        if (_programStatus.Count == 0) RearmHeuristic();
    }

    /// <summary>
    /// The session's PTY exited. From here on, live-state reports still in flight are ignored
    /// (see <see cref="ApplyProgramStatus"/>).
    /// </summary>
    public void OnProcessExited()
    {
        _processExited = true;
        DropTransientProgramStatus();
    }

    /// <summary>
    /// "Clear status" from the sidebar menu: the user's way out of a state that got stuck
    /// (a program that died mid-turn without a clear, say). Drops every record and hands the
    /// session back to the heuristic until the program reports again.
    /// </summary>
    public void ClearProgramStatus()
    {
        if (!HasProgramStatus) return;
        _programStatus.Apply(new ProgramStatusReport(ProgramState.Clear));
        SyncProgramStatus(force: false, enteredMessage: null);
        RearmHeuristic();
    }

    /// <summary>
    /// True when a keystroke has something to acknowledge (a badge or a done/error record).
    /// Allocation-free — MainViewModel checks it on every key event.
    /// </summary>
    public bool HasUnacknowledgedProgramStatus =>
        HasProgramStatus && (NeedsAttention || _programStatus.HasUnacknowledgedResults);

    private bool _processExited;

    /// <summary>
    /// Undo the stand-down: the program no longer holds any record, so the screen heuristic
    /// is the only source again. Not done on acknowledge — a Claude session whose <c>done</c>
    /// was just seen would otherwise have the regex guess at its prompt again.
    /// </summary>
    private void RearmHeuristic()
    {
        if (!HasProgramStatus) return;
        HasProgramStatus = false;
        AlertDetector?.Rearm();
    }

    /// <summary>
    /// Re-derives the session's indicators from the records. The dots and the working
    /// indicator follow the aggregate. The badge is raised from <paramref name="enteredMessage"/>
    /// — a RECORD that just entered blocked/done/error, so a root <c>done</c> still badges
    /// while a child keeps the aggregate at working — and cleared only once no record is in
    /// an attention state, so that child work does not wipe an unseen result.
    /// </summary>
    private void SyncProgramStatus(bool force, string? enteredMessage)
    {
        var prev = _programSummary;
        var next = _programStatus.Summarize();
        bool summaryChanged = force || next != prev;
        if (!summaryChanged && enteredMessage is null) return;
        _programSummary = next;

        bool blocked = next.State == ProgramState.Blocked;
        IsWaitingForApproval = blocked && next.Kind == BlockedKind.Permission;
        // question, auth, and a blocked report with no kind all mean "your turn to type".
        IsWaitingForInput = blocked && next.Kind != BlockedKind.Permission;

        if (enteredMessage is not null)
        {
            NeedsAttention = true;
            AlertMessage = enteredMessage;
        }
        else if (!_programStatus.HasAttentionRecord)
        {
            // Nothing needs the user any more.
            NeedsAttention = false;
            AlertMessage = "";
        }

        if (!summaryChanged) return;
        _programStatusVersion++;
        OnPropertyChanged(nameof(ProgramStatusVersion));
    }

    /// <summary>Short one-liner for the badge tooltip / toast. <c>msg</c> is already sanitised.</summary>
    internal static string DescribeProgramStatus(ProgramStatusSummary s) =>
        DescribeProgramStatus(s.State, s.Kind, s.Message);

    internal static string DescribeProgramStatus(ProgramState? state, BlockedKind kind, string? message)
    {
        string text = message ?? (state, kind) switch
        {
            (ProgramState.Blocked, BlockedKind.Permission) => "Needs permission",
            (ProgramState.Blocked, BlockedKind.Auth) => "Sign-in required",
            (ProgramState.Blocked, _) => "Waiting for your input",
            (ProgramState.Done, _) => "Finished",
            (ProgramState.Error, _) => "Failed",
            _ => "",
        };
        return text.Length > 100 ? text[..100] + "…" : text;
    }

    public void RaiseAlert(string message, AlertType alertType = AlertType.InputRequired)
    {
        // A heuristic alert can already be queued on the dispatcher when the first OSC 7501
        // report lands; once the program reports its own state, the guess must not win.
        if (HasProgramStatus) return;
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
    /// Returns <see cref="RepoRoot"/>, resolving it now if the poll has not got there yet.
    /// The git busy slot is keyed by it, and falling back to the working folder in that
    /// window let two panes of one repo hold different keys and run git at once (#181).
    /// The probe runs on the pool — see "Never spawn a process on the UI thread".
    /// </summary>
    public async Task<string?> EnsureRepoRootAsync()
    {
        if (!string.IsNullOrEmpty(RepoRoot) || Session.Kind == SessionKind.Ssh) return RepoRoot;
        string folder = Session.WorkingFolder;
        string? root = await Task.Run(() => GitService.GetRepoRootAsync(folder));
        // The folder may have been edited during the await (ReloadGitInfoAsync clears RepoRoot):
        // then this answer describes the OLD folder, and storing it would stick — nothing
        // re-probes a non-null RepoRoot. Probe again for the folder as it is now (a folder
        // edit is a user action, so this cannot spin).
        if (!string.Equals(Session.WorkingFolder, folder, StringComparison.Ordinal))
            return await EnsureRepoRootAsync();
        if (!string.IsNullOrEmpty(root) && string.IsNullOrEmpty(RepoRoot)) RepoRoot = root;
        return string.IsNullOrEmpty(RepoRoot) ? root : RepoRoot;
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
        GitUpstream = null;
        GitAhead = null;
        GitBehind = null;
        GitChangeCount = 0;
        HasWorktreeSiblings = false;
        // The user just pointed this session at a different folder, so whatever program
        // pushed git info via OSC 9001 was describing the old one. Let the local poller
        // back in until a program re-declares itself.
        _gitOverriddenByOsc = false;
        // Same reason: a "not a repo" answer for the old folder doesn't apply to the new one.
        _repoRootProbedNegative = false;
        // And the watcher is still pointed at the OLD repo's .git. Without this, moving a
        // session to another repo silently degraded it to poll-only — up to 120s stale in
        // the background — while appearing to work.
        RestartGitWatcher();
        return RefreshGitInfoAsync();
    }

    public void ClearAlert()
    {
        if (HasProgramStatus)
        {
            // The user has seen the session (focus, keystroke). Clear the badge, and retire
            // done/error records — they mean "a result you haven't looked at". The waiting
            // dots and the working indicator are NOT cleared: they describe what the program
            // is doing right now, and it will report the next state itself. Clearing them
            // here would blank an orange "needs permission" dot on the first keystroke.
            NeedsAttention = false;
            AlertMessage = "";
            if (_programStatus.Acknowledge()) SyncProgramStatus(force: false, enteredMessage: null);
            return;
        }

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

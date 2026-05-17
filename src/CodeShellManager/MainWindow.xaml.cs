using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using CodeShellManager.Models;
using CodeShellManager.Services;
using CodeShellManager.Terminal;
using CodeShellManager.ViewModels;
using CodeShellManager.Views;
using Microsoft.Data.Sqlite;
using Microsoft.Web.WebView2.Wpf;
// Explicit WPF aliases to avoid ambiguity with System.Windows.Forms
using Application      = System.Windows.Application;
using MessageBox       = System.Windows.MessageBox;
using MessageBoxButton = System.Windows.MessageBoxButton;
using MessageBoxImage  = System.Windows.MessageBoxImage;
using MessageBoxResult = System.Windows.MessageBoxResult;
using Color            = System.Windows.Media.Color;
using ColorConverter   = System.Windows.Media.ColorConverter;
using Brushes          = System.Windows.Media.Brushes;
using FontFamily       = System.Windows.Media.FontFamily;
using Orientation      = System.Windows.Controls.Orientation;
using HorizontalAlignment = System.Windows.HorizontalAlignment;
using VerticalAlignment   = System.Windows.VerticalAlignment;
using WpfButton        = System.Windows.Controls.Button;
using WpfKeyEventArgs  = System.Windows.Input.KeyEventArgs;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using Ellipse = System.Windows.Shapes.Ellipse;
using WpfTextBox = System.Windows.Controls.TextBox;

namespace CodeShellManager;

public partial class MainWindow : Window
{
    private readonly SessionManager _sessionManager = new();
    private readonly StateService _stateService = new();
    private readonly MainViewModel _vm;
    private string? _updateReleaseUrl;
    // Per-session UI: the WebView2, its persistent wrapper Border (built once, reused across layouts),
    // and its sidebar item.
    private readonly Dictionary<string, (WebView2 webView, Border terminalWrapper, Border sidebarItem)> _sessionUi = [];
    // Sidebar items for dormant (asleep) sessions — kept here so RebuildSidebarOrder
    // can re-append them to the bottom of the list after rebuilding active items.
    private readonly Dictionary<string, Border> _dormantSidebarItems = [];
    // Sidebar placeholders shown while a session is still launching (restore-on-startup).
    // RebuildSidebarOrder weaves them into the live-session list in saved order so the
    // user sees the full set of icons immediately, each with a "loading" indicator.
    // Items are removed once LaunchSessionAsync registers the real sidebar item.
    private readonly Dictionary<string, Border> _launchingSidebarItems = [];
    /// <summary>
    /// Per-session references to the run-related controls inside the terminal wrapper.
    /// Used by RefreshTerminalRunControls() to update the play button / chips strip
    /// when the session's RunCommands list or its RunInstances change.
    /// </summary>
    private readonly Dictionary<string, (
        WpfButton playBtn,
        WpfButton chevronBtn,
        Border chipsStrip,
        StackPanel chipsPanel,
        Border drawer,
        WpfTextBox drawerText,
        TextBlock drawerHeader,
        WpfButton drawerStopBtn,
        WpfButton drawerCopyBtn,
        WpfButton drawerSendBtn)> _runControls = new();
    private readonly Dictionary<string, string> _drawerItemBySession = new();
    // Per-session sidebar action button panels — kept so SettingsButton_Click
    // can flip every row to a new SidebarActionIconsMode without rebuilding sidebar items.
    private readonly Dictionary<string, StackPanel> _sidebarActionPanels = new();
    // Per-session rename trigger — captured from BuildSidebarItem so the context menu's
    // Rename action can invoke the same in-place editor as the double-click handler.
    private readonly Dictionary<string, Action> _sidebarRenameActions = new();
    // Anchor for shift-click range selection in the sidebar.
    private string? _selectionAnchorId;
    // Group-tab notification indicators (badge + text), keyed by group id (or "__ALL__"
    // / GroupFilter.Ungrouped sentinels). Repopulated on every RebuildGroupStrip.
    private readonly Dictionary<string, (Border badge, TextBlock badgeText)> _groupTabIndicators = [];

    // Sidebar sort state — in-memory only. Re-clicking the same field reverses direction;
    // clicking a new field starts in its natural direction (A→Z for text, newest-first for
    // last active). After a drag-reorder the field stays remembered so subsequent toggles
    // still make sense from the user's mental model.
    private enum SortField { None, Name, Folder, LastActive, Branch, Dirty, Repo }
    private SortField _currentSortField = SortField.None;
    private bool _sortDescending;
    private SqliteConnection? _db;
    private SearchService? _searchService;
    private LayoutMode _currentLayout = LayoutMode.Single;
    private int _layoutViewportOffset = 0;

    // Window state debounce
    private readonly System.Windows.Threading.DispatcherTimer _windowStateTimer;
    private bool _windowStateReady = false; // don't save before state is loaded

    // OnClosing is async void, which WPF does not await — without these gates the window
    // tears down while SaveStateAsync / claude disposal is still mid-flight. First entry
    // sets _isShuttingDown, cancels the close, runs the async cleanup, sets _shutdownComplete,
    // then re-invokes Close(); the second entry passes through to base.OnClosing. Any
    // intermediate re-entries (e.g. user double-clicks the X) hit the _isShuttingDown gate
    // and just cancel without re-running cleanup.
    private bool _isShuttingDown = false;
    private bool _shutdownComplete = false;

    public MainWindow()
    {
        InitializeComponent();
        _vm = new MainViewModel(_sessionManager, _stateService);
        _vm.SessionClosed += OnSessionVmClosed;

        // Refresh terminal layout whenever active session changes (Single mode needs this)
        _vm.PropertyChanged += (_, args) =>
        {
            if (args.PropertyName == nameof(MainViewModel.ActiveSession))
            {
                if (_vm.ActiveSession != null)
                    _vm.ActiveSession.Session.LastActivityAt = DateTime.UtcNow;
                RefreshTerminalLayout();
                UpdateSidebarActiveState();
            }
            else if (args.PropertyName == nameof(MainViewModel.Layout))
            {
                // Sync local layout field (used by RefreshTerminalLayout) with VM-driven changes
                // — fires both for state-restore at startup and any future programmatic changes.
                _currentLayout = _vm.Layout;
                _layoutViewportOffset = 0;
                RefreshTerminalLayout();
            }
            else if (args.PropertyName == nameof(MainViewModel.ActiveGroupId))
            {
                RebuildSidebarOrder();
                UpdateGroupStripActiveState();
            }
        };

        _vm.GroupsChanged += () => Dispatcher.Invoke(() =>
        {
            RebuildGroupStrip();
            UpdateGroupStripVisibility();
            RebuildSidebarOrder();
        });
        _vm.SelectionChanged += () => Dispatcher.Invoke(UpdateSidebarActiveState);
        // Re-filter the sidebar when a session's GroupId changes — otherwise the current
        // filter view stays stale until the user clicks a different tab. Also refresh the
        // group-tab indicators since session-to-group membership just shifted.
        _vm.SessionMembershipChanged += () => Dispatcher.Invoke(() =>
        {
            RebuildSidebarOrder();
            UpdateGroupTabIndicators();
        });
        _vm.Sessions.CollectionChanged += (_, e) =>
        {
            // Skip on Move so the in-place reordering done by RecomputeWorktreeSiblings
            // (and user drag-to-reorder) doesn't recurse back into itself or fight the user.
            // Add/Remove/Reset are the cases that genuinely shift the sibling landscape.
            if (e.Action != System.Collections.Specialized.NotifyCollectionChangedAction.Move)
            {
                RecomputeWorktreeSiblings();
                UpdateGroupTabIndicators();
            }
        };

        Loaded += OnLoaded;
        KeyDown += OnKeyDown;
        Activated += OnWindowActivated;

        // Window state persistence: debounce position/size changes
        _windowStateTimer = new System.Windows.Threading.DispatcherTimer
        {
            Interval = TimeSpan.FromSeconds(1)
        };
        _windowStateTimer.Tick += (_, _) =>
        {
            _windowStateTimer.Stop();
            SaveWindowBounds();
        };
        SizeChanged += (_, _) => OnWindowBoundsChanged();
        LocationChanged += (_, _) => OnWindowBoundsChanged();

        BuildShortcutPanel();
        SetupSidebarDrop();
        SetupGroupStripDrop();
        AttachSidebarQuickMenus();
    }

    private void OnWindowBoundsChanged()
    {
        if (!_windowStateReady) return;
        _windowStateTimer.Stop();
        _windowStateTimer.Start();
    }

    private void SaveWindowBounds()
    {
        if (!_windowStateReady) return;
        _vm.UpdateWindowState(WindowState, Left, Top, Width, Height);
        _ = _vm.SaveStateAsync();
    }

    // ── Startup ───────────────────────────────────────────────────────────────

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        await InitDatabaseAsync();
        await _vm.LoadStateAsync();
        RestoreWindowState();
        _windowStateReady = true;

        // Build the group strip (it'll only show once there are groups + the setting is on).
        RebuildGroupStrip();
        UpdateGroupStripVisibility();

        // Prune indexed output per retention policy (runs once at startup, after settings load)
        if (_searchService != null)
        {
            try { await _searchService.PruneOldOutputAsync(_vm.Settings.OutputRetentionDays); }
            catch { /* non-critical */ }
        }
        _ = CheckForUpdatesAsync();   // fire-and-forget; never blocks startup

        var saved = _sessionManager.Sessions.ToList();
        Log($"OnLoaded: {saved.Count} saved sessions, AutoRestore={_vm.Settings.AutoRestoreSessions}, CleanStart={App.CleanStart}");
        if (App.CleanStart)
        {
            // --clean: skip restore and leave state.json untouched. Drop sessions,
            // groups, AND the recently-closed ring from in-memory state so any new
            // work this run starts from a clean slate (no leftover scaffolding from
            // prior debug sessions). SaveStateAsync is a no-op in --clean mode so
            // these clears don't touch the persisted file.
            foreach (var s in saved)
                _sessionManager.RemoveSession(s.Id);
            foreach (var g in _sessionManager.Groups.ToList())
                _sessionManager.RemoveGroup(g.Id);
            _vm.ClearRecentlyClosed();
            return;
        }
        if (saved.Count == 0) return;

        bool doRestore;
        if (_vm.Settings.AutoRestoreSessions)
        {
            doRestore = true;
        }
        else
        {
            var result = MessageBox.Show(
                $"Restore {saved.Count} saved session(s)?",
                "CodeShellManager", MessageBoxButton.YesNo, MessageBoxImage.Question);
            doRestore = result == MessageBoxResult.Yes;
        }

        if (doRestore)
        {
            // Build "launching" placeholder sidebar items for every live session up-front
            // so the full list of icons appears immediately, with a loading indicator on
            // each row until its real sidebar item replaces it. Dormant entries are added
            // at the bottom (live ones get placeholders woven in by RebuildSidebarOrder).
            foreach (var s in saved)
            {
                if (s.IsDormant) continue;
                AddLaunchingSidebarItem(s);
            }
            foreach (var s in saved)
            {
                if (s.IsDormant) AddDormantSidebarItem(s);
            }
            // Render the staged placeholders now. RebuildSidebarOrder weaves them into
            // the saved-order list (Resolve picks them when no live item exists yet) and
            // applies the active group filter so off-group placeholders are hidden.
            RebuildSidebarOrder();

            // Launch live sessions sequentially. Stagger consecutive claude launches:
            // claude's CLI does an unlocked read-modify-write on ~/.claude.json at startup,
            // so simultaneous boots can corrupt the user's profile.
            int staggerMs = _vm.Settings.ClaudeLaunchStaggerMs;
            bool lastWasClaude = false;
            // WebView2 user-data folder access-denied is a common shared-failure
            // when another instance is running. Batch these so the user gets one
            // actionable dialog at the end instead of N "Restore Error" popups.
            var webView2AccessDenied = new List<string>();
            foreach (var s in saved)
            {
                if (s.IsDormant) continue;
                bool isClaude = ClaudeSessionService.IsClaudeCommand(s.Command);
                if (isClaude && lastWasClaude && staggerMs > 0)
                    await Task.Delay(staggerMs);
                try { await LaunchSessionAsync(s, restoring: true); }
                catch (Exception ex)
                {
                    Log($"Restore FAILED for '{s.Name}': {ex}");
                    if (IsWebView2AccessDenied(ex))
                        webView2AccessDenied.Add(s.Name);
                    else
                        MessageBox.Show($"Failed to restore '{s.Name}': {ex.Message}",
                            "Restore Error", MessageBoxButton.OK, MessageBoxImage.Warning);
                }
                lastWasClaude = isClaude;
            }
            if (webView2AccessDenied.Count > 0)
            {
                MessageBox.Show(
                    $"Could not initialize WebView2 for {webView2AccessDenied.Count} session(s):\n\n" +
                    string.Join("\n", webView2AccessDenied.Select(n => "  • " + n)) +
                    "\n\nThis usually means another CodeShellManager instance is running, " +
                    "or a previous instance didn't shut down cleanly. Close any other " +
                    "instances (or wait a few seconds for the WebView2 user-data folder " +
                    "to unlock) and reopen the affected sessions from the sidebar.",
                    "WebView2 unavailable", MessageBoxButton.OK, MessageBoxImage.Warning);
            }
        }
        else
        {
            foreach (var s in saved)
                _sessionManager.RemoveSession(s.Id);
            await _vm.SaveStateAsync();
        }
    }

    // Detects WebView2 user-data folder access-denied, which surfaces as
    // UnauthorizedAccessException from CoreWebView2Environment.CreateAsync /
    // CreateCoreWebView2ControllerAsync when another process is holding the
    // folder. We surface a clearer message in that specific case.
    private static bool IsWebView2AccessDenied(Exception ex) =>
        ex is UnauthorizedAccessException
        && (ex.StackTrace?.Contains("WebView2", StringComparison.Ordinal) ?? false);

    private void RestoreWindowState()
    {
        var bounds = _vm.GetSavedWindowBounds();
        if (bounds != null)
        {
            // Validate bounds are at least partially on-screen
            var screenWidth = SystemParameters.VirtualScreenWidth;
            var screenHeight = SystemParameters.VirtualScreenHeight;
            var screenLeft = SystemParameters.VirtualScreenLeft;
            var screenTop = SystemParameters.VirtualScreenTop;

            double left = Math.Max(screenLeft, Math.Min(bounds.Left, screenLeft + screenWidth - 100));
            double top = Math.Max(screenTop, Math.Min(bounds.Top, screenTop + screenHeight - 100));
            double width = Math.Max(400, Math.Min(bounds.Width, screenWidth));
            double height = Math.Max(300, Math.Min(bounds.Height, screenHeight));

            Left = left;
            Top = top;
            Width = width;
            Height = height;
        }

        if (_vm.IsWindowMaximized())
            WindowState = WindowState.Maximized;
    }

    private async Task InitDatabaseAsync()
    {
        string dbPath = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            "CodeShellManager", "output.db");
        Directory.CreateDirectory(Path.GetDirectoryName(dbPath)!);
        _db = new SqliteConnection($"Data Source={dbPath}");
        _db.Open();
        await SearchService.InitializeSchemaAsync(_db);
        _searchService = new SearchService(_db);
    }

    // ── New session ───────────────────────────────────────────────────────────

    private void NewSession_Click(object sender, RoutedEventArgs e) => OpenNewSessionDialog();

    private void BroadcastRemoteControl_Click(object sender, RoutedEventArgs e)
    {
        foreach (var session in _vm.Sessions)
        {
            session.Bridge?.SendToTerminal("/remote-control\r");
            session.AlertDetector?.NotifyUserInteracted();
        }
    }

    private void OpenNewSessionDialog(string defaultFolder = "")
        => OpenNewSessionDialogCore(defaultFolder, parent: null);

    /// <summary>
    /// Opens the New Session dialog pre-filled with the parent session's folder, command, args.
    /// The new session lands immediately after the parent in the sidebar and inherits its
    /// GroupId + profile overrides (issue #27).
    /// </summary>
    private void OpenNewSessionDialogFromParent(SessionViewModel parent)
        => OpenNewSessionDialogCore(parent.WorkingFolder, parent);

    private void OpenNewSessionDialogCore(string defaultFolder, SessionViewModel? parent)
    {
        var profiles = _vm.Settings.ImportWindowsTerminalProfiles
            ? Services.WindowsTerminalProfileService.GetProfiles()
            : null;

        string folder = !string.IsNullOrEmpty(defaultFolder)
            ? defaultFolder
            : _vm.Settings.DefaultWorkingFolder;

        var dialog = new NewSessionDialog(
            folder,
            _vm.Settings.LaunchCommands,
            profiles,
            defaultCommand: parent?.Session.Command,
            defaultArgs: parent?.Session.Args,
            defaultName: null,
            recentlyClosed: _vm.RecentlyClosed,
            defaultSourceSession: parent?.Session)
        {
            Owner = this
        };

        if (dialog.ShowDialog() != true) return;

        // If the user picked an entry from the "Recently closed" list, reopen that
        // session directly with copied settings and skip the rest of the form.
        if (dialog.SelectedRecentlyClosed != null)
        {
            var entry = dialog.SelectedRecentlyClosed;
            // Only drop the entry from the ring after reopen succeeds — a transient
            // launch failure (bad folder, SSH unavailable) would otherwise lose the
            // entry permanently and the user couldn't retry.
            _ = ReopenAndRemoveOnSuccessAsync(entry);
            return;
        }

        // Group resolution priority:
        //   1. Explicit selection from the dialog (currently unused — no group picker there)
        //   2. Inherited from a parent session (spawn-near-parent flows)
        //   3. The active group filter, when the user is currently looking at a real group
        //      (not All / not Ungrouped) — FilterStrip mode lands new sessions where the
        //      user is currently filtered.
        //   4. The active session's group — InlineHeaders/None mode has no filter concept,
        //      so fall back to the group of the session the user was just working in.
        //   5. Ungrouped
        string? groupId = !string.IsNullOrEmpty(dialog.SelectedGroupId)
            ? dialog.SelectedGroupId
            : !string.IsNullOrEmpty(parent?.Session.GroupId)
                ? parent!.Session.GroupId
                : (_vm.ActiveGroupId != null && _vm.ActiveGroupId != GroupFilter.Ungrouped
                    ? _vm.ActiveGroupId
                    : !string.IsNullOrEmpty(_vm.ActiveSession?.GroupId)
                        ? _vm.ActiveSession!.GroupId
                        : null);

        var session = _sessionManager.CreateSession(
            dialog.SessionName,
            dialog.SelectedFolder,
            dialog.SelectedCommand,
            dialog.SelectedArgs,
            groupId,
            colorOverride: null,
            afterSessionId: parent?.Id);

        if (dialog.IsRemote)
        {
            session.Kind = Models.SessionKind.Ssh;
            session.SshUser = dialog.SshUser;
            session.SshHost = dialog.SshHost;
            session.SshPort = dialog.SshPort;
            session.SshRemoteFolder = dialog.SshRemoteFolder;
        }
        else if (dialog.IsWsl)
        {
            session.Kind = Models.SessionKind.Wsl;
            session.WslDistro = dialog.WslDistro;
            session.WslUser = dialog.WslUser;
            session.WslWorkingFolder = dialog.WslWorkingFolder;
            // The session's WorkingFolder stays as a Windows UNC view of the same path
            // so anything that touches the filesystem (git status, "open in Explorer")
            // resolves correctly. Empty = unmounted; LaunchSessionAsync falls back.
            session.WorkingFolder = Services.WslDiscoveryService.ToUncPath(
                dialog.WslDistro, dialog.WslWorkingFolder);
        }

        // Profile overrides come from the dialog (which may have copied from a Windows Terminal
        // profile). When the dialog left them blank and we have a parent, inherit the parent's.
        session.ProfileFontFamily = dialog.ProfileFontFamily ?? parent?.Session.ProfileFontFamily;
        session.ProfileFontSize = dialog.ProfileFontSize ?? parent?.Session.ProfileFontSize;
        session.ProfileFontWeight = dialog.ProfileFontWeight ?? parent?.Session.ProfileFontWeight;
        session.ProfileFontLigatures = dialog.ProfileFontLigatures ?? parent?.Session.ProfileFontLigatures;
        session.ProfileCursorShape = dialog.ProfileCursorShape ?? parent?.Session.ProfileCursorShape;
        session.ProfileCursorBlink = dialog.ProfileCursorBlink ?? parent?.Session.ProfileCursorBlink;
        session.ProfilePadding = dialog.ProfilePadding ?? parent?.Session.ProfilePadding;
        session.ProfileBackgroundOpacity = dialog.ProfileBackgroundOpacity ?? parent?.Session.ProfileBackgroundOpacity;
        session.ProfileRetroEffect = dialog.ProfileRetroEffect ?? parent?.Session.ProfileRetroEffect;
        session.ProfileColorSchemeJson = dialog.ProfileColorSchemeJson ?? parent?.Session.ProfileColorSchemeJson;

        _ = LaunchAndFollowUpWorktreesAsync(session, dialog.AdditionalWorktreePaths);
    }

    /// <summary>
    /// Recreates a session from a <see cref="RecentlyClosedEntry"/> snapshot. Gets a fresh
    /// Id (so it's independent of the original) and goes through the normal launch path.
    /// Returns the created session; callers can verify it remained in
    /// <c>_sessionManager.Sessions</c> after the await to confirm launch success.
    /// </summary>
    private async Task<ShellSession> ReopenClosedSessionAsync(RecentlyClosedEntry entry)
    {
        var session = _sessionManager.CreateSession(
            entry.Name,
            entry.WorkingFolder,
            entry.Command,
            entry.Args,
            string.IsNullOrEmpty(entry.GroupId) ? null : entry.GroupId,
            colorOverride: entry.ColorOverride);

        // Kind first so the IsRemote shim below doesn't promote a Wsl entry back
        // to Ssh when its IsRemote happens to round-trip as false.
        session.Kind = entry.Kind;
        // Legacy entries (pre-Kind) have Kind=Local but IsRemote=true for SSH —
        // the IsRemote setter on ShellSession migrates that to Kind=Ssh.
        if (entry.Kind == Models.SessionKind.Local) session.IsRemote = entry.IsRemote;
        session.SshUser = entry.SshUser;
        session.SshHost = entry.SshHost;
        session.SshPort = entry.SshPort;
        session.SshRemoteFolder = entry.SshRemoteFolder;
        session.WslDistro = entry.WslDistro;
        session.WslUser = entry.WslUser;
        session.WslWorkingFolder = entry.WslWorkingFolder;

        session.ProfileFontFamily = entry.ProfileFontFamily;
        session.ProfileFontSize = entry.ProfileFontSize;
        session.ProfileFontWeight = entry.ProfileFontWeight;
        session.ProfileFontLigatures = entry.ProfileFontLigatures;
        session.ProfileCursorShape = entry.ProfileCursorShape;
        session.ProfileCursorBlink = entry.ProfileCursorBlink;
        session.ProfilePadding = entry.ProfilePadding;
        session.ProfileBackgroundOpacity = entry.ProfileBackgroundOpacity;
        session.ProfileRetroEffect = entry.ProfileRetroEffect;
        session.ProfileColorSchemeJson = entry.ProfileColorSchemeJson;

        // Deep-copy RunCommands so subsequent edits don't mutate any other entry
        // that may still share the same list reference.
        session.RunCommands = entry.RunCommands.Select(r => new RunCommandItem
        {
            Id = Guid.NewGuid().ToString(),
            Label = r.Label,
            CommandLine = r.CommandLine,
            IsDefault = r.IsDefault,
            Mode = r.Mode,
            PostRunUrl = r.PostRunUrl,
        }).ToList();

        await LaunchSessionAsync(session);
        return session;
    }

    /// <summary>
    /// Reopens a recently-closed entry and only drops it from the ring if the launch
    /// actually succeeded. LaunchSessionAsync's catch path removes the session it
    /// created on failure, so we use SessionManager membership as the success signal.
    /// </summary>
    private async Task ReopenAndRemoveOnSuccessAsync(RecentlyClosedEntry entry)
    {
        var session = await ReopenClosedSessionAsync(entry);
        if (_sessionManager.Sessions.Any(s => s.Id == session.Id))
            _vm.RemoveRecentlyClosed(entry);
    }

    /// <summary>
    /// Launches the primary session, then any opt-in sibling worktrees from the dialog —
    /// each inheriting the primary's command, group, and profile overrides, and inserted
    /// immediately after it so they cluster in the sidebar.
    /// </summary>
    private async Task LaunchAndFollowUpWorktreesAsync(ShellSession primary, IReadOnlyList<string> additionalPaths)
    {
        SeedRunCommandsAsync(primary);
        await LaunchSessionAsync(primary);
        if (additionalPaths.Count == 0) return;

        // Stagger consecutive claude launches for the same reason the boot path does
        // (see commit 59a7067): claude's CLI does an unlocked read-modify-write on
        // ~/.claude.json at startup, and back-to-back launches can corrupt it.
        int staggerMs = _vm.Settings.ClaudeLaunchStaggerMs;
        string anchorId = primary.Id;
        bool lastWasClaude = ClaudeSessionService.IsClaudeCommand(primary.Command);
        foreach (var path in additionalPaths)
        {
            if (!System.IO.Directory.Exists(path)) continue;
            bool isClaude = ClaudeSessionService.IsClaudeCommand(primary.Command);
            if (isClaude && lastWasClaude && staggerMs > 0) await Task.Delay(staggerMs);
            var sibling = _sessionManager.CreateSession(
                System.IO.Path.GetFileName(path.TrimEnd('/', '\\')) ?? primary.Command,
                path,
                primary.Command,
                primary.Args,
                string.IsNullOrEmpty(primary.GroupId) ? null : primary.GroupId,
                colorOverride: null,
                afterSessionId: anchorId);
            InheritSessionKindFrom(sibling, primary);
            // Inherit profile so siblings look identical.
            sibling.ProfileFontFamily = primary.ProfileFontFamily;
            sibling.ProfileFontSize = primary.ProfileFontSize;
            sibling.ProfileFontWeight = primary.ProfileFontWeight;
            sibling.ProfileFontLigatures = primary.ProfileFontLigatures;
            sibling.ProfileCursorShape = primary.ProfileCursorShape;
            sibling.ProfileCursorBlink = primary.ProfileCursorBlink;
            sibling.ProfilePadding = primary.ProfilePadding;
            sibling.ProfileBackgroundOpacity = primary.ProfileBackgroundOpacity;
            sibling.ProfileRetroEffect = primary.ProfileRetroEffect;
            sibling.ProfileColorSchemeJson = primary.ProfileColorSchemeJson;
            SeedRunCommandsAsync(sibling);
            await LaunchSessionAsync(sibling);
            anchorId = sibling.Id;
            lastWasClaude = isClaude;
        }
    }

    /// <summary>
    /// Duplicates a session without a dialog: same folder, command, args, group, and
    /// profile overrides; new GUID; a derived name like "<original> (2)". Lands after parent.
    /// </summary>
    private async Task DuplicateSessionAsync(SessionViewModel parent)
    {
        var p = parent.Session;
        string baseName = string.IsNullOrEmpty(p.Name) ? parent.DisplayName : p.Name;
        var clone = _sessionManager.CreateSession(
            DeriveDuplicateName(baseName),
            p.WorkingFolder,
            p.Command,
            p.Args,
            string.IsNullOrEmpty(p.GroupId) ? null : p.GroupId,
            colorOverride: null,
            afterSessionId: parent.Id);
        InheritSessionKindFrom(clone, p);
        clone.ProfileFontFamily = p.ProfileFontFamily;
        clone.ProfileFontSize = p.ProfileFontSize;
        clone.ProfileFontWeight = p.ProfileFontWeight;
        clone.ProfileFontLigatures = p.ProfileFontLigatures;
        clone.ProfileCursorShape = p.ProfileCursorShape;
        clone.ProfileCursorBlink = p.ProfileCursorBlink;
        clone.ProfilePadding = p.ProfilePadding;
        clone.ProfileBackgroundOpacity = p.ProfileBackgroundOpacity;
        clone.ProfileRetroEffect = p.ProfileRetroEffect;
        clone.ProfileColorSchemeJson = p.ProfileColorSchemeJson;
        // Copy parent's run commands with fresh Ids so the duplicate has its own list.
        foreach (var item in p.RunCommands)
        {
            clone.RunCommands.Add(new Models.RunCommandItem
            {
                Id = System.Guid.NewGuid().ToString(),
                Label = item.Label,
                CommandLine = item.CommandLine,
                IsDefault = item.IsDefault,
                Mode = item.Mode,
                PostRunUrl = item.PostRunUrl,
            });
        }
        // If the parent had no commands, fall back to detection.
        if (clone.RunCommands.Count == 0) SeedRunCommandsAsync(clone);
        await LaunchSessionAsync(clone);
    }

    private string DeriveDuplicateName(string baseName)
    {
        // If baseName already ends with " (N)", increment; otherwise append " (2)".
        var match = System.Text.RegularExpressions.Regex.Match(baseName, @"^(.*) \((\d+)\)$");
        string stem = match.Success ? match.Groups[1].Value : baseName;
        int start = match.Success ? int.Parse(match.Groups[2].Value) + 1 : 2;
        var existing = new HashSet<string>(
            _vm.Sessions.Select(s => s.DisplayName), StringComparer.OrdinalIgnoreCase);
        for (int n = start; n < start + 100; n++)
        {
            string candidate = $"{stem} ({n})";
            if (!existing.Contains(candidate)) return candidate;
        }
        return $"{stem} ({start})";
    }

    /// <summary>
    /// Propagates a parent session's <see cref="Models.SessionKind"/> and kind-specific
    /// fields (SSH host/user/port, WSL distro/user) onto a freshly-created child
    /// session. For WSL children it also derives <c>WslWorkingFolder</c> from the
    /// child's <c>WorkingFolder</c>, which the worktree code paths set to a
    /// <c>\\wsl$\&lt;distro&gt;\…</c> UNC. Without this step a new session spawned
    /// from a WSL parent (Duplicate, sibling worktree, new worktree) silently falls
    /// back to <see cref="Models.SessionKind.Local"/> and tries to run the parent's
    /// command (e.g. <c>claude</c>) inside a Windows PowerShell at the UNC path.
    /// </summary>
    private static void InheritSessionKindFrom(Models.ShellSession target, Models.ShellSession source)
    {
        target.Kind = source.Kind;
        if (source.Kind == Models.SessionKind.Ssh)
        {
            target.SshUser = source.SshUser;
            target.SshHost = source.SshHost;
            target.SshPort = source.SshPort;
            target.SshRemoteFolder = source.SshRemoteFolder;
            return;
        }
        if (source.Kind == Models.SessionKind.Wsl)
        {
            target.WslDistro = source.WslDistro;
            target.WslUser = source.WslUser;

            var (parsedDistro, parsedLinux) = Services.GitService.TryParseWslUnc(target.WorkingFolder);
            if (!string.IsNullOrEmpty(parsedDistro))
            {
                // Common path: WorkingFolder is a WSL UNC the caller already built.
                target.WslWorkingFolder = parsedLinux == "/" ? "" : parsedLinux;
            }
            else if (!string.IsNullOrEmpty(target.WorkingFolder) && target.WorkingFolder.StartsWith('/'))
            {
                // Caller passed a Linux path directly (e.g. typed into a worktree dialog).
                target.WslWorkingFolder = target.WorkingFolder;
                target.WorkingFolder = Services.WslDiscoveryService.ToUncPath(
                    source.WslDistro, target.WslWorkingFolder);
            }
            else
            {
                // Unknown shape — keep the parent's folder so the child at least lands
                // somewhere usable instead of in $HOME-by-accident.
                target.WslWorkingFolder = source.WslWorkingFolder;
                target.WorkingFolder = source.WorkingFolder;
            }
        }
    }

    /// <summary>
    /// Launches a new session in an existing sibling worktree (path resolved via
    /// `git worktree list`). Inherits the source session's command, group, and profile.
    /// </summary>
    private async Task LaunchSessionInSiblingWorktreeAsync(SessionViewModel parent, string worktreePath)
    {
        if (!System.IO.Directory.Exists(worktreePath))
        {
            MessageBox.Show(this, $"Worktree folder '{worktreePath}' does not exist.",
                "Worktree missing", MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }
        var p = parent.Session;
        var sibling = _sessionManager.CreateSession(
            System.IO.Path.GetFileName(worktreePath.TrimEnd('/', '\\')) ?? p.Command,
            worktreePath,
            p.Command,
            p.Args,
            string.IsNullOrEmpty(p.GroupId) ? null : p.GroupId,
            colorOverride: null,
            afterSessionId: parent.Id);
        InheritSessionKindFrom(sibling, p);
        sibling.ProfileFontFamily = p.ProfileFontFamily;
        sibling.ProfileFontSize = p.ProfileFontSize;
        sibling.ProfileFontWeight = p.ProfileFontWeight;
        sibling.ProfileFontLigatures = p.ProfileFontLigatures;
        sibling.ProfileCursorShape = p.ProfileCursorShape;
        sibling.ProfileCursorBlink = p.ProfileCursorBlink;
        sibling.ProfilePadding = p.ProfilePadding;
        sibling.ProfileBackgroundOpacity = p.ProfileBackgroundOpacity;
        sibling.ProfileRetroEffect = p.ProfileRetroEffect;
        sibling.ProfileColorSchemeJson = p.ProfileColorSchemeJson;
        SeedRunCommandsAsync(sibling);
        await LaunchSessionAsync(sibling);
    }

    /// <summary>
    /// Stamps the session's RunCommands list from the matching project-type template,
    /// if the list is currently empty AND the session is local (not SSH). Runs on a
    /// background task so the UI doesn't block on folder enumeration. No-op if the
    /// folder doesn't match any template.
    /// </summary>
    private void SeedRunCommandsAsync(Models.ShellSession session)
    {
        // SSH is out of reach for the synchronous Directory.EnumerateFiles probe.
        // WSL is reachable via the `\\wsl$\<distro>\…` UNC view — slow on first
        // access if the distro VM is stopped, but the probe runs on a background
        // task so the UI doesn't block. RunInstance already wraps run commands in
        // `wsl.exe -- bash -lc` for WSL parents.
        if (session.Kind == Models.SessionKind.Ssh) return;
        if (session.RunCommands.Count > 0) return;
        if (string.IsNullOrWhiteSpace(session.WorkingFolder)) return;

        string folder = session.WorkingFolder;
        _ = System.Threading.Tasks.Task.Run(() =>
        {
            var template = Services.RunCommandTemplatesService.SeedFor(folder);
            if (template == null) return;
            Dispatcher.Invoke(() =>
            {
                // Re-check on UI thread — the user may have edited the list manually
                // while we were scanning (race with the editor dialog).
                if (session.RunCommands.Count == 0)
                {
                    foreach (var item in template.Items)
                        session.RunCommands.Add(item);
                    _ = _vm.SaveStateAsync();
                    RefreshTerminalRunControls(session.Id);
                }
            });
        });
    }

    private static WpfButton MakeDrawerActionButton(string label) => new()
    {
        Content = label,
        Background = Brushes.Transparent,
        BorderThickness = new Thickness(0),
        Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
        FontSize = 11,
        Cursor = System.Windows.Input.Cursors.Hand,
        Padding = new Thickness(8, 4, 8, 4),
    };

    /// <summary>
    /// Rebuilds chips + play-button visibility + drawer content for one session.
    /// Idempotent — safe to call from every InstancesChanged event.
    /// </summary>
    private void RefreshTerminalRunControls(string sessionId)
    {
        if (!_runControls.TryGetValue(sessionId, out var c)) return;
        var vm = _vm.Sessions.FirstOrDefault(s => s.Id == sessionId);
        if (vm == null) return;

        // Play / chevron visibility — driven by whether the list has anything to run.
        var vis = vm.Session.RunCommands.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        c.playBtn.Visibility = vis;
        c.chevronBtn.Visibility = vis;

        // Rebuild chips strip.
        c.chipsPanel.Children.Clear();
        var instances = vm.Runner.Instances;
        foreach (var (_, inst) in instances)
        {
            var chip = BuildRunChip(vm, inst);
            c.chipsPanel.Children.Add(chip);
        }
        c.chipsStrip.Visibility = instances.Count > 0 ? Visibility.Visible : Visibility.Collapsed;

        // Update drawer if a viewed item exists.
        if (_drawerItemBySession.TryGetValue(sessionId, out var viewedItemId) &&
            vm.Runner.GetInstance(viewedItemId) is { } viewedInst)
        {
            c.drawerHeader.Text = $"{viewedInst.Label} — {DescribeState(viewedInst)}";
            c.drawerText.Text = viewedInst.SnapshotOutput();
            // Auto-scroll to the end while the run is active.
            if (viewedInst.State == RunState.Running)
                c.drawerText.ScrollToEnd();
            c.drawerStopBtn.IsEnabled = viewedInst.State == RunState.Running;
        }
        else
        {
            // Viewed item disappeared (was dismissed). Hide the drawer.
            c.drawer.Visibility = Visibility.Collapsed;
            _drawerItemBySession.Remove(sessionId);
        }
    }

    private static string DescribeState(RunInstance inst) => inst.State switch
    {
        RunState.Idle => "idle",
        RunState.Running => "running…",
        RunState.ExitedOk => $"finished (exit 0, {inst.Duration?.TotalSeconds:F1}s)",
        RunState.ExitedFailed => $"failed (exit {inst.ExitCode?.ToString() ?? "?"})",
        _ => "?",
    };

    private Border BuildRunChip(SessionViewModel vm, RunInstance inst)
    {
        (Color fill, Color text) ColorsFor(RunState s) => s switch
        {
            RunState.Running       => (Color.FromRgb(0x89, 0xb4, 0xfa), Color.FromRgb(0x18, 0x18, 0x25)),
            RunState.ExitedOk      => (Color.FromRgb(0xa6, 0xe3, 0xa1), Color.FromRgb(0x18, 0x18, 0x25)),
            RunState.ExitedFailed  => (Color.FromRgb(0xf3, 0x8b, 0xa8), Color.FromRgb(0x18, 0x18, 0x25)),
            _                      => (Color.FromRgb(0x45, 0x47, 0x5a), Color.FromRgb(0xcd, 0xd6, 0xf4)),
        };
        string Icon(RunState s) => s switch
        {
            RunState.Running => "●",
            RunState.ExitedOk => "✓",
            RunState.ExitedFailed => "✗",
            _ => "▶",
        };
        var (fillC, textC) = ColorsFor(inst.State);

        var chip = new Border
        {
            Background = new SolidColorBrush(fillC),
            CornerRadius = new CornerRadius(10),
            Padding = new Thickness(8, 2, 4, 2),
            Margin = new Thickness(0, 0, 6, 0),
            Cursor = System.Windows.Input.Cursors.Hand,
        };
        var sp = new StackPanel { Orientation = Orientation.Horizontal };
        sp.Children.Add(new TextBlock
        {
            Text = $"{Icon(inst.State)} {inst.Label}",
            Foreground = new SolidColorBrush(textC),
            FontSize = 11,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 4, 0),
        });
        var dismiss = new WpfButton
        {
            Content = "✕",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(textC),
            FontSize = 9,
            Padding = new Thickness(2, 0, 2, 0),
            Cursor = System.Windows.Input.Cursors.Hand,
            ToolTip = "Dismiss",
        };
        dismiss.Click += (_, _) => vm.Runner.Dismiss(inst.ItemId);
        sp.Children.Add(dismiss);
        chip.Child = sp;

        chip.MouseLeftButtonUp += (_, _) => ToggleDrawer(vm, inst.ItemId);
        return chip;
    }

    private void ToggleDrawer(SessionViewModel vm, string itemId)
    {
        if (!_runControls.TryGetValue(vm.Id, out var c)) return;
        if (_drawerItemBySession.TryGetValue(vm.Id, out var current) && current == itemId
            && c.drawer.Visibility == Visibility.Visible)
        {
            c.drawer.Visibility = Visibility.Collapsed;
            _drawerItemBySession.Remove(vm.Id);
        }
        else
        {
            _drawerItemBySession[vm.Id] = itemId;
            c.drawer.Visibility = Visibility.Visible;
            RefreshTerminalRunControls(vm.Id);
        }
    }

    private void RunDefaultCommand(SessionViewModel vm)
    {
        var def = vm.Session.RunCommands.FirstOrDefault(i => i.IsDefault);
        if (def == null) return;
        vm.Runner.Run(def);
    }

    private void ShowRunCommandsDropdown(SessionViewModel vm, WpfButton anchor)
    {
        var menu = new System.Windows.Controls.ContextMenu
        {
            PlacementTarget = anchor,
            Placement = System.Windows.Controls.Primitives.PlacementMode.Bottom,
        };
        foreach (var item in vm.Session.RunCommands)
        {
            var label = item.IsDefault ? $"▶ {item.Label} (default)" : $"▶ {item.Label}";
            var mi = new System.Windows.Controls.MenuItem { Header = label };
            mi.Click += (_, _) => vm.Runner.Run(item);
            menu.Items.Add(mi);
        }
        menu.Items.Add(new System.Windows.Controls.Separator());
        var edit = new System.Windows.Controls.MenuItem { Header = "Edit commands…" };
        edit.Click += (_, _) => OpenRunCommandsEditor(vm);
        menu.Items.Add(edit);
        menu.IsOpen = true;
    }

    private void OpenRunCommandsEditor(SessionViewModel vm)
    {
        var dlg = new Views.SessionRunCommandsDialog(vm.DisplayName, vm.Session.RunCommands)
        {
            Owner = this,
        };
        if (dlg.ShowDialog() == true && dlg.Result != null)
        {
            vm.Session.RunCommands.Clear();
            foreach (var item in dlg.Result)
                vm.Session.RunCommands.Add(item);
            _ = _vm.SaveStateAsync();
            RefreshTerminalRunControls(vm.Id);
        }
    }

    private void SendRunOutputToTerminal(SessionViewModel vm, WpfTextBox drawerText)
    {
        if (!_drawerItemBySession.TryGetValue(vm.Id, out var itemId)) return;
        var inst = vm.Runner.GetInstance(itemId);
        if (inst == null) return;

        string text = !string.IsNullOrEmpty(drawerText.SelectedText)
            ? drawerText.SelectedText
            : inst.SnapshotOutput();
        if (string.IsNullOrWhiteSpace(text)) return;

        bool isClaude = ClaudeSessionService.IsClaudeCommand(vm.Session.Command);
        if (isClaude && vm.Bridge != null)
        {
            string exit = inst.ExitCode is { } code ? $" (exit code {code})" : "";
            // No trailing \r — leave it in Claude's input box for the user to submit.
            string wrapped = $"\nOutput of `{inst.CommandLine}`{exit}:\n```\n{text}\n```\n";
            vm.Bridge.SendToTerminal(wrapped);
            ToastHelper.Show("Sent to Claude", $"{text.Length} chars wrapped in fence");
        }
        else
        {
            // Non-Claude shell: clipboard fallback to avoid auto-execution.
            try { System.Windows.Clipboard.SetText(text); } catch { }
            ToastHelper.Show("Sent to clipboard", "Paste with Ctrl+V to be safe");
        }
    }

    private static void Log(string msg)
    {
        try
        {
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(App.LogPath)!);
            System.IO.File.AppendAllText(App.LogPath, $"[{DateTime.Now:HH:mm:ss.fff}] {msg}\n");
        }
        catch { }
    }

    private async Task LaunchSessionAsync(ShellSession session, bool restoring = false)
    {
        Log($"LaunchSession START: cmd='{session.Command}' args='{session.Args}' folder='{session.WorkingFolder}' restoring={restoring}");
        var vm = new SessionViewModel(session);

        // Set up alert detection
        var alertDetector = new AlertDetector(session.Id, vm.DisplayName);
        vm.AlertDetector = alertDetector;

        // Create WebView2 — each instance needs its own user data folder;
        // WebView2 locks the folder exclusively and throws E_ACCESSDENIED otherwise.
        string wv2DataDir = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            "CodeShellManager", "webview2", session.Id);
        var webView = new WebView2
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Stretch,
            DefaultBackgroundColor = System.Drawing.Color.FromArgb(30, 30, 46),
            CreationProperties = new Microsoft.Web.WebView2.Wpf.CoreWebView2CreationProperties
            {
                UserDataFolder = wv2DataDir
            }
        };

        // Build the persistent wrapper NOW (webView has no parent yet — this is safe).
        var terminalWrapper = BuildTerminalWrapper(vm, webView);
        terminalWrapper.Visibility = Visibility.Collapsed;
        TerminalGrid.Children.Add(terminalWrapper);   // in tree → WebView2 can init
        terminalWrapper.Visibility = Visibility.Visible; // show spinner immediately

        // Create bridge and initialize
        var bridge = new TerminalBridge(webView);
        vm.Bridge = bridge;
        bridge.AcceleratorKeyPressed += OnBridgeAcceleratorKey;
        // Diagnostics — bridge logs per-keystroke / per-output-chunk timing when
        // AppSettings.DebugTerminalTrace is on. Shares the live settings ref so
        // toggling in the Settings dialog takes effect on existing sessions.
        bridge.DebugSettings = _vm.Settings;
        bridge.DebugSessionId = session.Id.Length >= 8 ? session.Id[..8] : session.Id;

        // Wire output indexer and alert detector
        if (_db != null)
        {
            var indexer = new OutputIndexer(_db, session.Id, vm.DisplayName, _vm.Settings);
            vm.OutputIndexer = indexer;
            bridge.RawOutputReceived += raw =>
            {
                indexer.Feed(raw);
                alertDetector.Feed(raw);
            };
        }
        else
        {
            bridge.RawOutputReceived += alertDetector.Feed;
        }

        string assetsDir = Path.Combine(AppContext.BaseDirectory, "Assets");
        bool wantTransparent = session.ProfileBackgroundOpacity is < 1.0;
        string htmlFile = wantTransparent ? "terminal-transparent.html" : "terminal.html";
        string htmlPath = new Uri(Path.Combine(assetsDir, htmlFile)).AbsoluteUri;

        string bootLabel = session.IsRemote
            ? $"Connecting to {session.SshHost}…"
            : $"Starting {(string.IsNullOrWhiteSpace(session.Command) ? "session" : session.Command)}…";
        bridge.SetBootContext(bootLabel, GetAccentForSession(session));
        await bridge.InitializeAsync(htmlPath);
        bridge.ApplyFontSettings(_vm.Settings);
        bridge.ApplyProfileOverrides(session);

        // Start PTY now that bridge is ready
        var pty = new PseudoTerminal();
        vm.Pty = pty;
        string usageCommandKey = "";
        DateTime sessionStartUtc = DateTime.MinValue;
        pty.Exited += () =>
        {
            if (_searchService != null)
            {
                _ = _searchService.RecordSessionHistoryAsync(
                    session.Id, session.Name, session.WorkingFolder,
                    session.Command, session.Args, session.GroupId);
                if (sessionStartUtc != DateTime.MinValue && !string.IsNullOrEmpty(usageCommandKey))
                {
                    long secs = (long)(DateTime.UtcNow - sessionStartUtc).TotalSeconds;
                    _ = _searchService.RecordSessionDurationAsync(usageCommandKey, secs);
                }
            }
            Dispatcher.Invoke(() =>
            {
                _sessionManager.UpdateStatus(session.Id, SessionStatus.Exited);
                RefreshSidebarItem(session.Id);
            });
        };

        string effectiveCommand;
        string effectiveArgs;
        string workDir;

        if (session.Kind == Models.SessionKind.Ssh)
        {
            effectiveCommand = "ssh";
            effectiveArgs = session.BuildSshArgs();
            workDir = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }
        else if (session.Kind == Models.SessionKind.Wsl)
        {
            // wsl.exe handles its own cwd via --cd inside BuildWslArgs; pass the user
            // profile as the launching process's cwd so CreateProcess never sees a UNC
            // path it might reject.
            effectiveCommand = "wsl.exe";
            effectiveArgs = session.BuildWslArgs();
            workDir = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }
        else
        {
            workDir = Directory.Exists(session.WorkingFolder)
                ? session.WorkingFolder
                : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);

            effectiveArgs = session.Args;
            if (restoring && _vm.Settings.AutoResumeClaude
                && ClaudeSessionService.IsClaudeCommand(session.Command)
                && !effectiveArgs.Contains("--resume")
                && !effectiveArgs.Contains("--continue"))
            {
                string? sessionId = ClaudeSessionService.GetLastSessionId(session.WorkingFolder);
                if (sessionId != null)
                {
                    string resumeFlag = $"--resume {sessionId}";
                    effectiveArgs = string.IsNullOrEmpty(effectiveArgs)
                        ? resumeFlag
                        : $"{resumeFlag} {effectiveArgs}";
                    Log($"Auto-resume: using '{resumeFlag}' for claude session in '{session.WorkingFolder}'");
                }
                else
                {
                    Log($"Auto-resume: no prior session found for '{session.WorkingFolder}', starting fresh");
                }
            }

            if (vm.OutputIndexer != null &&
                (effectiveArgs.Contains("--continue") || effectiveArgs.Contains("--resume")))
            {
                vm.OutputIndexer.SkipUntil = DateTime.UtcNow.AddSeconds(20);
            }

            effectiveCommand = session.Command;
        }

        Log($"Starting PTY: workDir='{workDir}'");
        try
        {
            var (cols, rows) = bridge.TerminalSize;
            pty.Start(effectiveCommand, effectiveArgs, workDir, cols, rows);
            Log("PTY started OK");
            bridge.AttachPty(pty);
            _sessionManager.UpdateStatus(session.Id, SessionStatus.Running);

            usageCommandKey = effectiveCommand;
            sessionStartUtc = DateTime.UtcNow;
            if (_searchService != null)
                _ = _searchService.RecordSessionStartAsync(effectiveCommand);
        }
        catch (Exception ex)
        {
            Log($"PTY start FAILED: {ex}");
            TerminalGrid.Children.Remove(terminalWrapper);
            MessageBox.Show($"Failed to start '{session.FullCommandLine}':\n{ex.Message}",
                "Launch Error", MessageBoxButton.OK, MessageBoxImage.Error);
            vm.Dispose();
            _sessionManager.RemoveSession(session.Id);
            // Drop any launching placeholder so it doesn't linger after a failed restore.
            if (_launchingSidebarItems.Remove(session.Id))
                RebuildSidebarOrder();
            return;
        }

        Log($"terminalWrapper visible, TerminalGrid children={TerminalGrid.Children.Count}");

        // Build sidebar entry
        var sidebarItem = BuildSidebarItem(vm);
        _sessionUi[session.Id] = (webView, terminalWrapper, sidebarItem);
        // Once the real sidebar item is registered, the launching placeholder for this
        // session is no longer rendered by Resolve(); drop it so it doesn't leak.
        _launchingSidebarItems.Remove(session.Id);

        _vm.RegisterSession(vm);
        // RebuildSidebarOrder applies the active group filter; the explicit call here ensures
        // a newly-launched session that doesn't match the filter is correctly hidden.
        RebuildSidebarOrder();
        RefreshTerminalLayout();
        bridge.FitTerminal();
        UpdateAlertBadge();
        EmptyState.Visibility = Visibility.Collapsed;
    }

    // ── Sidebar ───────────────────────────────────────────────────────────────

    private Border BuildSidebarItem(SessionViewModel vm)
    {
        string accent = vm.AccentColor;
        var accentBrush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(accent));

        // Note: the container carries a constant 2px BorderThickness with a Transparent
        // BorderBrush by default. UpdateSidebarActiveState toggles BorderBrush to the
        // session accent color when active, so layout doesn't shift as items become
        // active and the active session stays visually distinct from the multi-select tint.
        var container = new Border
        {
            Margin = new Thickness(0, 2, 0, 2),
            Background = Brushes.Transparent,
            BorderBrush = Brushes.Transparent,
            BorderThickness = new Thickness(2),
            Cursor = System.Windows.Input.Cursors.Hand,
            CornerRadius = new CornerRadius(6),
            Tag = vm.Id
        };

        var inner = new Grid();
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(6) });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto }); // status dot
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto }); // buttons

        // Accent stripe
        var stripe = new Border
        {
            Background = accentBrush,
            CornerRadius = new CornerRadius(4, 0, 0, 4),
            Width = 6
        };
        Grid.SetColumn(stripe, 0);

        // Text area
        var textPanel = new StackPanel { Margin = new Thickness(8, 6, 4, 6) };

        var nameText = new TextBlock
        {
            Text = vm.DisplayName,
            Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            FontSize = 13,
            FontWeight = FontWeights.Medium,
            TextTrimming = TextTrimming.CharacterEllipsis,
            ToolTip = "Double-click to rename"
        };

        var renameBox = new WpfTextBox
        {
            Background = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
            Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            BorderBrush = new SolidColorBrush(Color.FromRgb(0x89, 0xb4, 0xfa)),
            CaretBrush = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            BorderThickness = new Thickness(1),
            Padding = new Thickness(4, 2, 4, 2),
            FontSize = 13,
            Visibility = Visibility.Collapsed
        };

        void CommitRename()
        {
            string newName = renameBox.Text.Trim();
            if (!string.IsNullOrEmpty(newName))
            {
                vm.Rename(newName);
                nameText.Text = vm.DisplayName;
            }
            renameBox.Visibility = Visibility.Collapsed;
            nameText.Visibility = Visibility.Visible;
            _ = _vm.SaveStateAsync();
        }

        void StartRename()
        {
            renameBox.Text = vm.DisplayName;
            nameText.Visibility = Visibility.Collapsed;
            renameBox.Visibility = Visibility.Visible;
            renameBox.SelectAll();
            renameBox.Focus();
        }

        renameBox.LostFocus += (_, _) => CommitRename();
        renameBox.KeyDown += (_, ke) =>
        {
            if (ke.Key == Key.Enter) { CommitRename(); ke.Handled = true; }
            else if (ke.Key == Key.Escape)
            {
                renameBox.Visibility = Visibility.Collapsed;
                nameText.Visibility = Visibility.Visible;
                ke.Handled = true;
            }
        };
        nameText.MouseLeftButtonDown += (_, me) =>
        {
            if (me.ClickCount == 2) { StartRename(); me.Handled = true; }
        };

        var folderText = new TextBlock
        {
            Text = vm.FolderShort,
            Foreground = new SolidColorBrush(Color.FromRgb(0x93, 0x99, 0xb2)),
            FontSize = 10,
            Margin = new Thickness(0, 1, 0, 0),
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        // Git branch indicator
        var gitText = new TextBlock
        {
            Foreground = new SolidColorBrush(Color.FromRgb(0x93, 0x99, 0xb2)),
            FontSize = 10,
            Margin = new Thickness(0, 2, 0, 0),
            TextTrimming = TextTrimming.CharacterEllipsis,
            Visibility = Visibility.Collapsed
        };

        // Worktree-siblings subtitle — appears when 2+ live sessions share the same repo root.
        var worktreeText = new TextBlock
        {
            Foreground = new SolidColorBrush(Color.FromRgb(0x93, 0x99, 0xb2)),
            FontSize = 10,
            FontStyle = FontStyles.Italic,
            Margin = new Thickness(0, 1, 0, 0),
            TextTrimming = TextTrimming.CharacterEllipsis,
            Visibility = Visibility.Collapsed,
            ToolTip = "This session shares a git repo with other open sessions."
        };

        void UpdateWorktreeText()
        {
            if (vm.HasWorktreeSiblings && !string.IsNullOrEmpty(vm.RepoRoot))
            {
                worktreeText.Text = vm.WorktreeSubtitle;
                worktreeText.Visibility = Visibility.Visible;
            }
            else
            {
                worktreeText.Visibility = Visibility.Collapsed;
            }
        }
        UpdateWorktreeText();

        static void UpdateGitText(TextBlock tb, SessionViewModel svm)
        {
            if (!svm.GitInfoLoaded || string.IsNullOrEmpty(svm.GitBranch))
            {
                tb.Visibility = Visibility.Collapsed;
                return;
            }
            tb.Inlines.Clear();
            if (svm.GitIsDirty)
            {
                tb.Inlines.Add(new System.Windows.Documents.Run("● ")
                    { Foreground = new SolidColorBrush(Color.FromRgb(0xfe, 0xb3, 0x86)) });
            }
            tb.Inlines.Add(new System.Windows.Documents.Run($"\u2387 {svm.GitBranch}"));
            tb.Visibility = Visibility.Visible;
        }

        UpdateGitText(gitText, vm);

        // Claude session tag (sidebar)
        if (ClaudeSessionService.IsClaudeCommand(vm.Command))
        {
            var sidebarClaudeBadge = new Border
            {
                Background = new SolidColorBrush(Color.FromArgb(0x28, 0x89, 0xb4, 0xfa)),
                BorderBrush = new SolidColorBrush(Color.FromArgb(0x60, 0x89, 0xb4, 0xfa)),
                BorderThickness = new Thickness(1),
                CornerRadius = new CornerRadius(3),
                Padding = new Thickness(4, 1, 4, 1),
                Margin = new Thickness(0, 2, 0, 0),
                HorizontalAlignment = HorizontalAlignment.Left
            };
            sidebarClaudeBadge.Child = new TextBlock
            {
                Text = "claude",
                Foreground = new SolidColorBrush(Color.FromRgb(0x89, 0xb4, 0xfa)),
                FontSize = 9,
                FontWeight = FontWeights.SemiBold
            };
            textPanel.Children.Add(sidebarClaudeBadge);
        }

        // Alert badge
        var alertBadge = new Border
        {
            Background = new SolidColorBrush(Color.FromRgb(0xf3, 0x8b, 0xa8)),
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(5, 1, 5, 1),
            Margin = new Thickness(0, 2, 0, 0),
            Visibility = Visibility.Collapsed,
            HorizontalAlignment = HorizontalAlignment.Left,
            Tag = "alertBadge"
        };
        alertBadge.Child = new TextBlock
        {
            Text = "needs attention",
            Foreground = new SolidColorBrush(Color.FromRgb(0x1e, 0x1e, 0x2e)),
            FontSize = 9
        };

        textPanel.Children.Add(nameText);
        textPanel.Children.Add(renameBox);
        textPanel.Children.Add(folderText);
        textPanel.Children.Add(gitText);
        textPanel.Children.Add(worktreeText);
        textPanel.Children.Add(alertBadge);

        // Status dot (waiting indicator)
        var statusDot = new Ellipse
        {
            Width = 8,
            Height = 8,
            Fill = Brushes.Transparent,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(4, 0, 4, 0),
            Visibility = Visibility.Collapsed
        };
        Grid.SetColumn(statusDot, 2);

        // Action buttons — reduced set (#29). Secondary actions (Open in Explorer, PowerShell,
        // Rename) live on the right-click context menu instead. The icons are surfaced based
        // on AppSettings.SidebarActionIconsMode (default OnHover) so the sidebar stays calm
        // unless the user is actively reaching for an action.
        var btnPanel = new StackPanel
        {
            Orientation = Orientation.Vertical,
            Margin = new Thickness(0, 4, 4, 4),
            VerticalAlignment = VerticalAlignment.Center
        };

        var spawnBtn = MakeMiniButton("➕", "New session here (inherits group + profile)",
                            () => OpenNewSessionDialogFromParent(vm));
        var sleepBtn = MakeMiniButton("💤", "Sleep session (keep it but stop the terminal)", () => SleepSession(vm));
        var closeBtn = MakeMiniButton("✕", "Close session", () => vm.CloseCommand.Execute(null));

        btnPanel.Children.Add(spawnBtn);
        btnPanel.Children.Add(sleepBtn);
        btnPanel.Children.Add(closeBtn);

        _sidebarActionPanels[vm.Id] = btnPanel;
        _sidebarRenameActions[vm.Id] = StartRename;
        ApplyActionIconsMode(btnPanel, container, _vm.Settings.SidebarActionIconsMode, isHovered: false);

        Grid.SetColumn(textPanel, 1);
        Grid.SetColumn(btnPanel, 3);
        inner.Children.Add(stripe);
        inner.Children.Add(textPanel);
        inner.Children.Add(statusDot);
        inner.Children.Add(btnPanel);

        container.Child = inner;

        // ── Drag-and-drop reorder ─────────────────────────────────────────────
        System.Windows.Point dragStartPos = default;
        bool dragPending = false;

        container.PreviewMouseLeftButtonDown += (_, me) =>
        {
            dragStartPos = me.GetPosition(null);
            dragPending = true;
        };
        container.PreviewMouseLeftButtonUp += (_, _) => dragPending = false;
        container.PreviewMouseMove += (_, me) =>
        {
            if (!dragPending || me.LeftButton != MouseButtonState.Pressed) return;
            var diff = me.GetPosition(null) - dragStartPos;
            if (Math.Abs(diff.X) > SystemParameters.MinimumHorizontalDragDistance ||
                Math.Abs(diff.Y) > SystemParameters.MinimumVerticalDragDistance)
            {
                dragPending = false;
                System.Windows.DragDrop.DoDragDrop(container, vm.Id, System.Windows.DragDropEffects.Move);
            }
        };

        // Click to activate. Ctrl/Shift modifiers drive multi-select instead of activation.
        container.MouseLeftButtonDown += (_, me) =>
        {
            var mods = Keyboard.Modifiers;
            if ((mods & ModifierKeys.Shift) != 0)
            {
                // Range selection must walk only real session items — header tags like
                // "groupheader:" (inline mode) and "cluster:" (worktree clusters) are not
                // sessions and would otherwise leak into SelectedSessionIds.
                var visibleIds = SidebarSessionList.Children.OfType<Border>()
                    .Select(b => b.Tag as string)
                    .Where(t => !string.IsNullOrEmpty(t)
                                && !t.StartsWith("dormant:")
                                && !t.StartsWith("groupheader:")
                                && !t.StartsWith("cluster:"))
                    .Select(t => t!)
                    .ToList();
                _vm.SetRangeSelection(visibleIds, _selectionAnchorId, vm.Id);
                me.Handled = true;
                return;
            }
            if ((mods & ModifierKeys.Control) != 0)
            {
                _vm.ToggleSelection(vm.Id);
                _selectionAnchorId = vm.Id;
                me.Handled = true;
                return;
            }
            _vm.ClearSelection();
            _selectionAnchorId = vm.Id;
            _vm.FocusSessionCommand.Execute(vm);
            UpdateSidebarActiveState();
        };

        // Right-click context menu — supports multi-target actions when 2+ sessions are selected.
        container.ContextMenu = BuildSessionContextMenu(vm);
        container.ContextMenuOpening += (_, _) =>
        {
            container.ContextMenu = BuildSessionContextMenu(vm);
        };

        // Hover effect
        // Hover effect — must not clobber multi-select tint. Selected-but-not-active items
        // keep their blue background on hover and on mouse leave; only plain, unselected,
        // non-active items show the muted hover background and clear to transparent.
        // Also drives the OnHover SidebarActionIconsMode: action buttons fade in on enter,
        // out on leave. Hidden and Always modes ignore the hover transition.
        container.MouseEnter += (_, _) =>
        {
            ApplyActionIconsMode(btnPanel, container, _vm.Settings.SidebarActionIconsMode, isHovered: true);
            if (vm.Id == _vm.ActiveSession?.Id) return;
            if (_vm.IsSelected(vm.Id)) return;
            container.Background = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44));
        };
        container.MouseLeave += (_, _) =>
        {
            ApplyActionIconsMode(btnPanel, container, _vm.Settings.SidebarActionIconsMode, isHovered: false);
            if (vm.Id == _vm.ActiveSession?.Id) return;
            container.Background = _vm.IsSelected(vm.Id)
                ? new SolidColorBrush(Color.FromArgb(0x55, 0x89, 0xb4, 0xfa))
                : Brushes.Transparent;
        };

        // Subscribe to property changes
        vm.PropertyChanged += (_, args) =>
        {
            Dispatcher.Invoke(() =>
            {
                switch (args.PropertyName)
                {
                    case nameof(SessionViewModel.DisplayName):
                        nameText.Text = vm.DisplayName;
                        break;

                    case nameof(SessionViewModel.NeedsAttention):
                        alertBadge.Visibility = vm.NeedsAttention ? Visibility.Visible : Visibility.Collapsed;
                        UpdateAlertBadge();
                        UpdateGroupTabIndicators();
                        break;

                    case nameof(SessionViewModel.IsWaitingForInput):
                    case nameof(SessionViewModel.IsWaitingForApproval):
                        if (vm.IsWaitingForInput)
                        {
                            statusDot.Fill = new SolidColorBrush(Color.FromRgb(0xa6, 0xe3, 0xa1)); // green
                            statusDot.ToolTip = "Waiting for input";
                            statusDot.Visibility = Visibility.Visible;
                        }
                        else if (vm.IsWaitingForApproval)
                        {
                            statusDot.Fill = new SolidColorBrush(Color.FromRgb(0xff, 0xb7, 0x4d)); // orange
                            statusDot.ToolTip = "Tool approval needed";
                            statusDot.Visibility = Visibility.Visible;
                        }
                        else
                        {
                            statusDot.Visibility = Visibility.Collapsed;
                        }
                        UpdateGroupTabIndicators();
                        break;

                    case nameof(SessionViewModel.GitBranch):
                    case nameof(SessionViewModel.GitIsDirty):
                    case nameof(SessionViewModel.GitInfoLoaded):
                        UpdateGitText(gitText, vm);
                        UpdateWorktreeText();
                        break;

                    case nameof(SessionViewModel.RepoRoot):
                        RecomputeWorktreeSiblings();
                        UpdateWorktreeText();
                        break;

                    case nameof(SessionViewModel.HasWorktreeSiblings):
                        UpdateWorktreeText();
                        break;

                    case nameof(SessionViewModel.AccentColor):
                        // RepoRoot resolved → repaint sidebar stripe + ring with the shared
                        // color so worktree siblings cluster visually. The terminal pane's
                        // top stripe + ring are repainted by BuildTerminalWrapper's own
                        // AccentColor subscription using its locally-captured references.
                        try
                        {
                            var newAccent = (Color)ColorConverter.ConvertFromString(vm.AccentColor);
                            stripe.Background = new SolidColorBrush(newAccent);
                            if (_vm.ActiveSession?.Id == vm.Id)
                                UpdateSidebarActiveState();
                        }
                        catch { /* invalid hex — ignore */ }
                        break;
                }
            });
        };

        return container;
    }

    private static WpfButton MakeMiniButton(string icon, string tooltip, Action onClick)
    {
        var btn = new WpfButton
        {
            Content = icon,
            ToolTip = tooltip,
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 11,
            Padding = new Thickness(3),
            Cursor = System.Windows.Input.Cursors.Hand,
            Width = 22,
            Height = 22
        };
        btn.Click += (_, _) => onClick();
        return btn;
    }

    /// <summary>
    /// Applies the current <see cref="Models.SidebarActionIconsMode"/> to a single sidebar
    /// row's action-button stack. Hidden collapses the panel entirely (the row reclaims the
    /// horizontal space). OnHover keeps the panel laid out but transparent + non-interactive
    /// until the row is hovered. Always shows it at full opacity. Called from BuildSidebarItem
    /// at construction, from the row's MouseEnter/Leave handlers, and after a settings save.
    /// </summary>
    private static void ApplyActionIconsMode(StackPanel btnPanel, Border container,
        Models.SidebarActionIconsMode mode, bool isHovered)
    {
        switch (mode)
        {
            case Models.SidebarActionIconsMode.Hidden:
                btnPanel.Visibility = Visibility.Collapsed;
                btnPanel.IsHitTestVisible = false;
                btnPanel.Opacity = 0;
                break;
            case Models.SidebarActionIconsMode.Always:
                btnPanel.Visibility = Visibility.Visible;
                btnPanel.IsHitTestVisible = true;
                btnPanel.Opacity = 1;
                break;
            case Models.SidebarActionIconsMode.OnHover:
            default:
                btnPanel.Visibility = Visibility.Visible;  // reserves layout space
                btnPanel.Opacity = isHovered ? 1 : 0;
                btnPanel.IsHitTestVisible = isHovered;
                break;
        }
    }

    private void UpdateSidebarActiveState()
    {
        foreach (Border item in SidebarSessionList.Children)
        {
            string? id = item.Tag as string;
            if (id == null || id.StartsWith("dormant:") || id.StartsWith("cluster:") || id.StartsWith("groupheader:")) continue;
            bool isActive = id == _vm.ActiveSession?.Id;
            bool isSelected = _vm.IsSelected(id);

            // Background: selection takes precedence over active (so a multi-selected
            // active session still shows it belongs to the action set). Active-only items
            // get Catppuccin Surface1 — dark enough that white text still has clear
            // contrast (~6.3:1) while the accent-coloured ring carries the active signal.
            if (isSelected)
                item.Background = new SolidColorBrush(Color.FromArgb(0x55, 0x89, 0xb4, 0xfa));
            else if (isActive)
                item.Background = new SolidColorBrush(Color.FromRgb(0x45, 0x47, 0x5a));
            else
                item.Background = Brushes.Transparent;

            // Active gets an accent-colored ring around the whole item — the unique signal
            // that survives any selection state, mirroring the active-terminal ring.
            if (isActive)
            {
                string accentHex = "#89b4fa";
                var vm = _vm.Sessions.FirstOrDefault(s => s.Id == id);
                if (vm != null) accentHex = vm.AccentColor;
                try { item.BorderBrush = new SolidColorBrush((Color)ColorConverter.ConvertFromString(accentHex)); }
                catch { item.BorderBrush = new SolidColorBrush(Color.FromRgb(0x89, 0xb4, 0xfa)); }
            }
            else
            {
                item.BorderBrush = Brushes.Transparent;
            }
        }
        UpdateActiveTerminalHighlight();
    }

    private void UpdateActiveTerminalHighlight()
    {
        string? activeId = _vm.ActiveSession?.Id;
        foreach (var (id, ui) in _sessionUi)
        {
            if (id == activeId)
            {
                // Look up the live AccentColor from the VM rather than the Tag stashed
                // at build time — RepoRoot is populated asynchronously by GitService,
                // and AccentColor changes when it lands. A cached Tag goes stale and
                // would no longer match the sidebar ring.
                var vm = _vm.Sessions.FirstOrDefault(s => s.Id == id);
                string accentHex = vm?.AccentColor ?? (ui.terminalWrapper.Tag as string ?? "#89b4fa");
                try
                {
                    var accent = (Color)ColorConverter.ConvertFromString(accentHex);
                    ui.terminalWrapper.BorderBrush = new SolidColorBrush(accent);
                }
                catch
                {
                    ui.terminalWrapper.BorderBrush = new SolidColorBrush(Color.FromRgb(0x89, 0xb4, 0xfa));
                }
            }
            else
            {
                ui.terminalWrapper.BorderBrush = Brushes.Transparent;
            }
        }
    }

    // ── Sidebar quick-menu (right-click on empty sidebar / tab / placeholder area) ─

    /// <summary>
    /// Attaches the sidebar quick-action ContextMenu to every empty-space surface in the
    /// sidebar column (header, session list, group strip) and to the TerminalGrid empty
    /// state. Children with their own ContextMenu — session rows, group tabs — shadow this
    /// one, so the menu only opens when right-clicking actual empty space.
    /// </summary>
    private void AttachSidebarQuickMenus()
    {
        var menu = BuildSidebarQuickMenu();
        SidebarHeader.ContextMenu = menu;
        SidebarSessionList.ContextMenu = menu;
        GroupStripBorder.ContextMenu = menu;
        TerminalGrid.ContextMenu = menu;

        // The sort button has its own menu — left-click drops it down rather than
        // requiring a right-click on a small target.
        SortSessionsButton.ContextMenu = BuildSortSessionsMenu();
    }

    private System.Windows.Controls.ContextMenu BuildSortSessionsMenu()
    {
        var menu = new System.Windows.Controls.ContextMenu();

        var byName = new System.Windows.Controls.MenuItem();
        byName.Click += (_, _) => ApplySort(SortField.Name);
        menu.Items.Add(byName);

        var byFolder = new System.Windows.Controls.MenuItem();
        byFolder.Click += (_, _) => ApplySort(SortField.Folder);
        menu.Items.Add(byFolder);

        var byActive = new System.Windows.Controls.MenuItem();
        byActive.Click += (_, _) => ApplySort(SortField.LastActive);
        menu.Items.Add(byActive);

        // Git submenu. Sessions without live git info (dormant or non-git folders) sink to
        // the end of every git-based ordering — the comparison emits "empty" keys last.
        var gitMenu = new System.Windows.Controls.MenuItem { Header = "Git" };
        var byBranch = new System.Windows.Controls.MenuItem();
        byBranch.Click += (_, _) => ApplySort(SortField.Branch);
        gitMenu.Items.Add(byBranch);

        var byDirty = new System.Windows.Controls.MenuItem();
        byDirty.Click += (_, _) => ApplySort(SortField.Dirty);
        gitMenu.Items.Add(byDirty);

        var byRepo = new System.Windows.Controls.MenuItem();
        byRepo.Click += (_, _) => ApplySort(SortField.Repo);
        gitMenu.Items.Add(byRepo);
        menu.Items.Add(gitMenu);

        // Repopulate headers each open so the active field shows its current direction
        // arrow and the others show their natural default direction as a preview.
        menu.Opened += (_, _) =>
        {
            SetSortHeader(byName,   "Name",        SortField.Name,       ascendingDefault: true);
            SetSortHeader(byFolder, "Folder",      SortField.Folder,     ascendingDefault: true);
            SetSortHeader(byActive, "Last active", SortField.LastActive, ascendingDefault: false);
            SetSortHeader(byBranch, "Branch",      SortField.Branch,     ascendingDefault: true);
            SetSortHeader(byDirty,  "Dirty",       SortField.Dirty,      ascendingDefault: false);
            SetSortHeader(byRepo,   "Repo",        SortField.Repo,       ascendingDefault: true);
        };

        return menu;
    }

    /// <summary>
    /// Writes the sort menu item's label + direction glyph. The arrow goes into the Icon
    /// slot (not the header text) so it sits in the menu's icon column with consistent
    /// alignment regardless of label length, and we can size/dim it independently of the
    /// label font. Active fields render at full opacity in SemiBold; inactive fields show
    /// the field's natural default direction at 45% opacity as a preview.
    /// </summary>
    private void SetSortHeader(System.Windows.Controls.MenuItem item, string label, SortField field, bool ascendingDefault)
    {
        bool active = _currentSortField == field;
        bool descending = active ? _sortDescending : !ascendingDefault;
        item.Header = label;
        item.FontWeight = active ? FontWeights.SemiBold : FontWeights.Normal;
        item.Icon = new TextBlock
        {
            Text = descending ? "↓" : "↑",
            FontSize = 11,
            Opacity = active ? 1.0 : 0.45,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center
        };
    }

    private void ApplySort(SortField field)
    {
        // Re-clicking the active field flips direction; switching field picks its natural
        // starting direction (text fields ascend, timestamps + dirty newest-/dirty-first).
        if (_currentSortField == field)
        {
            _sortDescending = !_sortDescending;
        }
        else
        {
            _currentSortField = field;
            _sortDescending = field is SortField.LastActive or SortField.Dirty;
        }

        // Live VMs hold git info; dormant sessions don't. Build a snapshot lookup so each
        // comparison call is O(1) and stable for the duration of the sort.
        var live = _vm.Sessions.ToDictionary(v => v.Id);

        string RepoName(SessionViewModel? vm) =>
            string.IsNullOrEmpty(vm?.RepoRoot)
                ? ""
                : System.IO.Path.GetFileName(vm!.RepoRoot!.TrimEnd('/', '\\')) ?? "";

        // For text-key git fields, empty keys (no live VM, or no git) sink to the bottom of
        // the ascending order — that way users always see the "real" data grouped at top.
        int CompareEmptyLast(string a, string b)
        {
            bool aEmpty = string.IsNullOrEmpty(a);
            bool bEmpty = string.IsNullOrEmpty(b);
            if (aEmpty && bEmpty) return 0;
            if (aEmpty) return 1;
            if (bEmpty) return -1;
            return string.Compare(a, b, StringComparison.OrdinalIgnoreCase);
        }

        Comparison<Models.ShellSession> baseCmp = field switch
        {
            SortField.Name       => (a, b) => string.Compare(a.Name, b.Name, StringComparison.OrdinalIgnoreCase),
            SortField.Folder     => (a, b) => string.Compare(a.WorkingFolder, b.WorkingFolder, StringComparison.OrdinalIgnoreCase),
            SortField.LastActive => (a, b) => a.LastActivityAt.CompareTo(b.LastActivityAt),
            SortField.Branch     => (a, b) =>
            {
                live.TryGetValue(a.Id, out var va);
                live.TryGetValue(b.Id, out var vb);
                int c = CompareEmptyLast(va?.GitBranch ?? "", vb?.GitBranch ?? "");
                if (c != 0) return c;
                // Same branch: keep worktrees adjacent by folder.
                return string.Compare(a.WorkingFolder, b.WorkingFolder, StringComparison.OrdinalIgnoreCase);
            },
            SortField.Dirty      => (a, b) =>
            {
                live.TryGetValue(a.Id, out var va);
                live.TryGetValue(b.Id, out var vb);
                // bool.CompareTo: false < true. Ascending = clean first; descending = dirty first.
                int c = (va?.GitIsDirty ?? false).CompareTo(vb?.GitIsDirty ?? false);
                if (c != 0) return c;
                return string.Compare(a.Name, b.Name, StringComparison.OrdinalIgnoreCase);
            },
            SortField.Repo       => (a, b) =>
            {
                live.TryGetValue(a.Id, out var va);
                live.TryGetValue(b.Id, out var vb);
                int c = CompareEmptyLast(RepoName(va), RepoName(vb));
                if (c != 0) return c;
                return string.Compare(a.WorkingFolder, b.WorkingFolder, StringComparison.OrdinalIgnoreCase);
            },
            _                    => (_, _) => 0
        };
        Comparison<Models.ShellSession> cmp = _sortDescending
            ? (a, b) => baseCmp(b, a)
            : baseCmp;

        _sessionManager.SortSessions(cmp);
        _ = _vm.SaveStateAsync();
        RebuildSidebarOrder();
    }

    private void SortSessionsButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is System.Windows.Controls.Button btn && btn.ContextMenu != null)
        {
            btn.ContextMenu.PlacementTarget = btn;
            btn.ContextMenu.Placement = System.Windows.Controls.Primitives.PlacementMode.Bottom;
            btn.ContextMenu.IsOpen = true;
        }
    }

    private System.Windows.Controls.ContextMenu BuildSidebarQuickMenu()
    {
        var menu = new System.Windows.Controls.ContextMenu();

        var newSession = new System.Windows.Controls.MenuItem
        {
            Header = "＋ New session",
            InputGestureText = "Ctrl+T"
        };
        newSession.Click += (_, _) => OpenNewSessionDialog();
        menu.Items.Add(newSession);

        var newGroup = new System.Windows.Controls.MenuItem { Header = "＋ New group…" };
        newGroup.Click += (_, _) => PromptCreateGroup();
        menu.Items.Add(newGroup);

        menu.Items.Add(new System.Windows.Controls.Separator());

        // Global bulk actions — operate on every live or dormant session, not just one group.
        var bulkActions = new System.Windows.Controls.MenuItem { Header = "Bulk actions" };
        var wakeAllDormant = new System.Windows.Controls.MenuItem { Header = "Wake all dormant" };
        wakeAllDormant.Click += async (_, _) =>
        {
            var dormant = _sessionManager.Sessions.Where(s => s.IsDormant).ToList();
            foreach (var s in dormant)
                await WakeSessionAsync(s);
        };
        bulkActions.Items.Add(wakeAllDormant);

        var sleepAllGlobal = new System.Windows.Controls.MenuItem { Header = "Sleep all" };
        sleepAllGlobal.Click += (_, _) =>
        {
            foreach (var vm in _vm.Sessions.ToList())
                SleepSession(vm);
        };
        bulkActions.Items.Add(sleepAllGlobal);

        var closeAllGlobal = new System.Windows.Controls.MenuItem { Header = "Close all…" };
        closeAllGlobal.Click += (_, _) =>
        {
            var targets = _vm.Sessions.ToList();
            if (targets.Count == 0) return;
            var r = MessageBox.Show(
                $"Close all {targets.Count} live session(s)? They will be removed permanently.",
                "Close all", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No);
            if (r != MessageBoxResult.Yes) return;
            foreach (var vm in targets)
                vm.CloseCommand.Execute(null);
        };
        bulkActions.Items.Add(closeAllGlobal);
        menu.Items.Add(bulkActions);

        menu.Items.Add(new System.Windows.Controls.Separator());

        // Group display submenu — None / FilterStrip / InlineHeaders.
        var groupDisplay = new System.Windows.Controls.MenuItem { Header = "Group display" };
        var modeNone   = new System.Windows.Controls.MenuItem { Header = "None (flat list)",       IsCheckable = true };
        var modeStrip  = new System.Windows.Controls.MenuItem { Header = "Vertical filter strip",   IsCheckable = true };
        var modeInline = new System.Windows.Controls.MenuItem { Header = "Inline group headers",    IsCheckable = true };
        modeNone.Click   += (_, _) => SetGroupDisplayMode(Models.GroupDisplayMode.None);
        modeStrip.Click  += (_, _) => SetGroupDisplayMode(Models.GroupDisplayMode.FilterStrip);
        modeInline.Click += (_, _) => SetGroupDisplayMode(Models.GroupDisplayMode.InlineHeaders);
        groupDisplay.Items.Add(modeNone);
        groupDisplay.Items.Add(modeStrip);
        groupDisplay.Items.Add(modeInline);
        menu.Items.Add(groupDisplay);

        // Expand / collapse all group sections — only meaningful in InlineHeaders mode.
        var expandAll = new System.Windows.Controls.MenuItem { Header = "Expand all groups" };
        expandAll.Click += (_, _) => SetAllGroupsExpanded(true);
        menu.Items.Add(expandAll);

        var collapseAll = new System.Windows.Controls.MenuItem { Header = "Collapse all groups" };
        collapseAll.Click += (_, _) => SetAllGroupsExpanded(false);
        menu.Items.Add(collapseAll);

        // Sidebar action-icons submenu — OnHover / Always / Hidden.
        var rowIcons = new System.Windows.Controls.MenuItem { Header = "Session row icons" };
        var iconHover  = new System.Windows.Controls.MenuItem { Header = "On hover",       IsCheckable = true };
        var iconAlways = new System.Windows.Controls.MenuItem { Header = "Always visible", IsCheckable = true };
        var iconHidden = new System.Windows.Controls.MenuItem { Header = "Hidden",         IsCheckable = true };
        iconHover.Click  += (_, _) => SetSidebarActionIconsMode(Models.SidebarActionIconsMode.OnHover);
        iconAlways.Click += (_, _) => SetSidebarActionIconsMode(Models.SidebarActionIconsMode.Always);
        iconHidden.Click += (_, _) => SetSidebarActionIconsMode(Models.SidebarActionIconsMode.Hidden);
        rowIcons.Items.Add(iconHover);
        rowIcons.Items.Add(iconAlways);
        rowIcons.Items.Add(iconHidden);
        menu.Items.Add(rowIcons);

        menu.Items.Add(new System.Windows.Controls.Separator());

        var showGit = new System.Windows.Controls.MenuItem { Header = "Show git branch", IsCheckable = true };
        showGit.Click += (_, _) =>
        {
            _vm.Settings.ShowGitBranch = !_vm.Settings.ShowGitBranch;
            _ = _vm.SaveStateAsync();
            RebuildSidebarOrder();
        };
        menu.Items.Add(showGit);

        var showClusters = new System.Windows.Controls.MenuItem { Header = "Show worktree clusters", IsCheckable = true };
        showClusters.Click += (_, _) =>
        {
            _vm.Settings.ShowWorktreeClusters = !_vm.Settings.ShowWorktreeClusters;
            _ = _vm.SaveStateAsync();
            RebuildSidebarOrder();
        };
        menu.Items.Add(showClusters);

        menu.Items.Add(new System.Windows.Controls.Separator());

        var allSettings = new System.Windows.Controls.MenuItem { Header = "All settings…" };
        allSettings.Click += (s, e) => SettingsButton_Click(s, e);
        menu.Items.Add(allSettings);

        menu.Opened += (_, _) =>
        {
            var mode = _vm.Settings.GroupDisplayMode;
            modeNone.IsChecked   = mode == Models.GroupDisplayMode.None;
            modeStrip.IsChecked  = mode == Models.GroupDisplayMode.FilterStrip;
            modeInline.IsChecked = mode == Models.GroupDisplayMode.InlineHeaders;

            var iconMode = _vm.Settings.SidebarActionIconsMode;
            iconHover.IsChecked  = iconMode == Models.SidebarActionIconsMode.OnHover;
            iconAlways.IsChecked = iconMode == Models.SidebarActionIconsMode.Always;
            iconHidden.IsChecked = iconMode == Models.SidebarActionIconsMode.Hidden;

            showGit.IsChecked      = _vm.Settings.ShowGitBranch;
            showClusters.IsChecked = _vm.Settings.ShowWorktreeClusters;

            int liveCount = _vm.Sessions.Count;
            int dormantCount = _sessionManager.Sessions.Count(s => s.IsDormant);
            wakeAllDormant.IsEnabled = dormantCount > 0;
            sleepAllGlobal.IsEnabled = liveCount > 0;
            closeAllGlobal.IsEnabled = liveCount > 0;
            bulkActions.IsEnabled    = liveCount > 0 || dormantCount > 0;

            bool inlineHeaders = mode == Models.GroupDisplayMode.InlineHeaders;
            bool anyGroups = _sessionManager.Groups.Count > 0;
            expandAll.IsEnabled   = inlineHeaders && anyGroups;
            collapseAll.IsEnabled = inlineHeaders && anyGroups;
        };

        return menu;
    }

    private void SetGroupDisplayMode(Models.GroupDisplayMode mode)
    {
        if (_vm.Settings.GroupDisplayMode == mode) return;
        _vm.Settings.GroupDisplayMode = mode;
        // ActiveGroupId only makes sense in FilterStrip mode — same reset SettingsButton_Click does.
        if (mode != Models.GroupDisplayMode.FilterStrip)
            _vm.ActiveGroupId = null;
        _ = _vm.SaveStateAsync();
        UpdateGroupStripVisibility();
        RebuildSidebarOrder();
    }

    private void SetAllGroupsExpanded(bool expanded)
    {
        foreach (var g in _sessionManager.Groups)
            g.IsExpanded = expanded;
        _vm.Settings.UngroupedSectionExpanded = expanded;
        _ = _vm.SaveStateAsync();
        RebuildSidebarOrder();
    }

    private void SetSidebarActionIconsMode(Models.SidebarActionIconsMode mode)
    {
        if (_vm.Settings.SidebarActionIconsMode == mode) return;
        _vm.Settings.SidebarActionIconsMode = mode;
        _ = _vm.SaveStateAsync();
        foreach (var (id, panel) in _sidebarActionPanels)
        {
            if (_sessionUi.TryGetValue(id, out var ui))
                ApplyActionIconsMode(panel, ui.sidebarItem, mode, isHovered: ui.sidebarItem.IsMouseOver);
        }
    }

    // ── Group strip (categories) ──────────────────────────────────────────────

    private void UpdateGroupStripVisibility()
    {
        bool show = _vm.Settings.GroupDisplayMode == Models.GroupDisplayMode.FilterStrip
            && _sessionManager.Groups.Count > 0;
        GroupStripBorder.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        GroupStripCol.Width = new GridLength(show ? 44 : 0);
    }

    private void RebuildGroupStrip()
    {
        GroupStripPanel.Children.Clear();
        _groupTabIndicators.Clear();
        if (_sessionManager.Groups.Count == 0) return;

        GroupStripPanel.Children.Add(BuildGroupTab(null, "All", "▦"));
        GroupStripPanel.Children.Add(BuildGroupTab(GroupFilter.Ungrouped, "Ungrouped", "□"));
        GroupStripPanel.Children.Add(new Border
        {
            Height = 1,
            Background = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
            Margin = new Thickness(8, 6, 8, 6)
        });
        foreach (var g in _sessionManager.Groups.OrderBy(g => g.SortOrder))
            GroupStripPanel.Children.Add(BuildGroupTab(g.Id, g.Name, GroupInitials(g.Name)));

        UpdateGroupStripActiveState();
        UpdateGroupTabIndicators();
    }

    /// <summary>
    /// Refreshes the small status indicator on each group tab. Priority: alert count
    /// (pink badge with N) > tool-approval (orange dot) > input-required (green dot) >
    /// hidden. The "All" tab aggregates every live session; "Ungrouped" aggregates only
    /// sessions with an empty GroupId.
    /// </summary>
    private void UpdateGroupTabIndicators()
    {
        if (_groupTabIndicators.Count == 0) return;

        var pink = new SolidColorBrush(Color.FromRgb(0xf3, 0x8b, 0xa8));
        var orange = new SolidColorBrush(Color.FromRgb(0xff, 0xb7, 0x4d));
        var green = new SolidColorBrush(Color.FromRgb(0xa6, 0xe3, 0xa1));
        var inkOnLight = new SolidColorBrush(Color.FromRgb(0x1e, 0x1e, 0x2e));

        foreach (var (key, (badge, text)) in _groupTabIndicators)
        {
            IEnumerable<SessionViewModel> set = key switch
            {
                "__ALL__" => _vm.Sessions,
                _ when key == GroupFilter.Ungrouped =>
                    _vm.Sessions.Where(s => string.IsNullOrEmpty(s.GroupId)),
                _ => _vm.Sessions.Where(s => s.GroupId == key)
            };

            int alerts = 0;
            bool anyApproval = false, anyInput = false;
            foreach (var s in set)
            {
                if (s.NeedsAttention) alerts++;
                if (s.IsWaitingForApproval) anyApproval = true;
                if (s.IsWaitingForInput) anyInput = true;
            }

            if (alerts > 0)
            {
                badge.Background = pink;
                badge.MinWidth = 14;
                text.Text = alerts.ToString();
                text.Foreground = inkOnLight;
                badge.ToolTip = alerts == 1
                    ? "1 session needs attention"
                    : $"{alerts} sessions need attention";
                badge.Visibility = Visibility.Visible;
            }
            else if (anyApproval)
            {
                badge.Background = orange;
                badge.MinWidth = 10;
                text.Text = "";
                badge.ToolTip = "Tool approval needed";
                badge.Visibility = Visibility.Visible;
            }
            else if (anyInput)
            {
                badge.Background = green;
                badge.MinWidth = 10;
                text.Text = "";
                badge.ToolTip = "Waiting for input";
                badge.Visibility = Visibility.Visible;
            }
            else
            {
                badge.Visibility = Visibility.Collapsed;
            }
        }
    }

    private static string GroupInitials(string name)
    {
        if (string.IsNullOrWhiteSpace(name)) return "?";
        var parts = name.Split(new[] { ' ', '-', '_' }, StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0) return name[..1].ToUpperInvariant();
        if (parts.Length == 1) return parts[0][..1].ToUpperInvariant();
        return (parts[0][0].ToString() + parts[^1][0]).ToUpperInvariant();
    }

    /// <summary>
    /// Builds one tab in the group strip. <paramref name="groupId"/> can be:
    /// null (the "All" / no-filter tab), <see cref="GroupFilter.Ungrouped"/>, or a real Id.
    /// </summary>
    private Border BuildGroupTab(string? groupId, string fullName, string label)
    {
        var border = new Border
        {
            Margin = new Thickness(4, 2, 4, 2),
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0, 0, 2, 0),
            BorderBrush = Brushes.Transparent,
            CornerRadius = new CornerRadius(4, 0, 0, 4),
            Cursor = System.Windows.Input.Cursors.Hand,
            ToolTip = fullName,
            Tag = "group:" + (groupId ?? "__ALL__"),
            Height = 36
        };

        var grid = new Grid();
        var labelText = new TextBlock
        {
            Text = label,
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center
        };
        grid.Children.Add(labelText);

        // Notification indicator: pink badge with count when sessions in this group have
        // NeedsAttention; orange/green dot when only waiting for approval/input.
        // Hidden when the group has no active state. UpdateGroupTabIndicators recomputes it.
        var indicatorText = new TextBlock
        {
            Text = "",
            Foreground = new SolidColorBrush(Color.FromRgb(0x1e, 0x1e, 0x2e)),
            FontSize = 8,
            FontWeight = FontWeights.Bold,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center
        };
        var indicator = new Border
        {
            CornerRadius = new CornerRadius(7),
            MinWidth = 10,
            MinHeight = 10,
            Padding = new Thickness(3, 0, 3, 0),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Top,
            Margin = new Thickness(0, 3, 4, 0),
            Visibility = Visibility.Collapsed,
            Child = indicatorText,
            IsHitTestVisible = false
        };
        grid.Children.Add(indicator);
        border.Child = grid;

        _groupTabIndicators[groupId ?? "__ALL__"] = (indicator, indicatorText);

        border.MouseLeftButtonDown += (_, _) =>
        {
            _vm.ActiveGroupId = groupId;
        };

        // Real groups get a right-click menu + drag-to-reorder (the All/Ungrouped pseudo-tabs don't).
        if (groupId != null && groupId != GroupFilter.Ungrouped)
        {
            var menu = new System.Windows.Controls.ContextMenu();

            var moveUp = new System.Windows.Controls.MenuItem { Header = "Move up" };
            moveUp.Click += (_, _) =>
            {
                int idx = IndexOfUserGroup(groupId);
                if (idx > 0) _vm.MoveGroup(groupId, idx - 1);
            };
            menu.Items.Add(moveUp);
            var moveDown = new System.Windows.Controls.MenuItem { Header = "Move down" };
            moveDown.Click += (_, _) =>
            {
                int idx = IndexOfUserGroup(groupId);
                if (idx >= 0 && idx < _sessionManager.Groups.Count - 1)
                    _vm.MoveGroup(groupId, idx + 1);
            };
            menu.Items.Add(moveDown);
            AddGroupBulkActionItems(menu, groupId, fullName);

            var rename = new System.Windows.Controls.MenuItem { Header = "Rename group…" };
            rename.Click += (_, _) => PromptRenameGroup(groupId, fullName);
            menu.Items.Add(rename);
            var delete = new System.Windows.Controls.MenuItem { Header = "Delete group" };
            delete.Click += (_, _) =>
            {
                var r = MessageBox.Show(
                    $"Delete group '{fullName}'? Sessions in this group will revert to Ungrouped.",
                    "Delete group", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No);
                if (r == MessageBoxResult.Yes) _vm.RemoveGroup(groupId);
            };
            menu.Items.Add(delete);
            menu.Opened += (_, _) =>
            {
                int idx = IndexOfUserGroup(groupId);
                moveUp.IsEnabled = idx > 0;
                moveDown.IsEnabled = idx >= 0 && idx < _sessionManager.Groups.Count - 1;
            };
            border.ContextMenu = menu;

            // Drag-to-reorder. The strip's Drop handler (SetupGroupStripDrop) resolves
            // the new index from the drop position.
            System.Windows.Point dragStartPos = default;
            bool dragPending = false;
            border.PreviewMouseLeftButtonDown += (_, me) =>
            {
                dragStartPos = me.GetPosition(null);
                dragPending = true;
            };
            border.PreviewMouseLeftButtonUp += (_, _) => dragPending = false;
            border.PreviewMouseMove += (_, me) =>
            {
                if (!dragPending || me.LeftButton != MouseButtonState.Pressed) return;
                var diff = me.GetPosition(null) - dragStartPos;
                if (Math.Abs(diff.X) > SystemParameters.MinimumHorizontalDragDistance ||
                    Math.Abs(diff.Y) > SystemParameters.MinimumVerticalDragDistance)
                {
                    dragPending = false;
                    System.Windows.DragDrop.DoDragDrop(border, "group:" + groupId,
                        System.Windows.DragDropEffects.Move);
                }
            };
        }

        // Drop target: assign dragged session(s) to this group. The "All" tab (groupId=null)
        // is a view, not a real group — don't accept drops there. "Ungrouped" accepts drops
        // and clears the GroupId. Multi-select drops the whole selection when the dragged
        // session is part of it.
        if (groupId != null)
        {
            border.AllowDrop = true;
            border.DragEnter += (_, e) =>
            {
                if (!IsSessionDragPayload(e.Data)) return;
                // Highlight the tab as the active drop target.
                border.Background = new SolidColorBrush(
                    Color.FromArgb(0x88, 0x89, 0xb4, 0xfa));
                e.Handled = true;
            };
            border.DragLeave += (_, _) =>
            {
                // Restore the normal active/inactive state; UpdateGroupStripActiveState
                // recomputes Background from _vm.ActiveGroupId.
                UpdateGroupStripActiveState();
            };
            border.DragOver += (_, e) =>
            {
                if (!IsSessionDragPayload(e.Data))
                {
                    // Let the parent GroupStripPanel handler take group-tab reorder drags.
                    return;
                }
                e.Effects = System.Windows.DragDropEffects.Move;
                // Handled = true prevents the strip's DragOver from overriding Effects to None.
                e.Handled = true;
            };
            border.Drop += (_, e) =>
            {
                if (!IsSessionDragPayload(e.Data))
                {
                    UpdateGroupStripActiveState();
                    return;
                }
                string sessionId = (string)e.Data.GetData(System.Windows.DataFormats.StringFormat);
                string? targetGroupId = groupId == GroupFilter.Ungrouped ? null : groupId;
                var targets = _vm.ResolveActionTargets(sessionId);
                _vm.AssignSessionsToGroup(targets, targetGroupId);
                e.Handled = true;
                UpdateGroupStripActiveState();
            };
        }

        return border;
    }

    private int IndexOfUserGroup(string groupId)
    {
        for (int i = 0; i < _sessionManager.Groups.Count; i++)
            if (_sessionManager.Groups[i].Id == groupId) return i;
        return -1;
    }

    /// <summary>
    /// Inline-mode header for a group section. <paramref name="group"/> = null renders the
    /// implicit "Ungrouped" header. Click toggles expand/collapse; right-click opens a
    /// rename/delete/move menu (real groups only); the header is both a drag source for
    /// group reorder and a drop target for session reassignment.
    /// </summary>
    private Border BuildInlineGroupHeader(Models.SessionGroup? group, int count, bool expanded)
    {
        string label = group?.Name ?? "Ungrouped";
        string headerTagId = group?.Id ?? GroupFilter.Ungrouped;

        var border = new Border
        {
            Margin = new Thickness(0, 8, 0, 2),
            Padding = new Thickness(8, 4, 8, 4),
            Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25)),
            BorderBrush = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Cursor = System.Windows.Input.Cursors.Hand,
            Tag = "groupheader:" + headerTagId,
            ToolTip = group == null
                ? "Sessions not assigned to a group"
                : $"Group: {group.Name}"
        };

        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var caret = new TextBlock
        {
            Text = expanded ? "▼" : "▶",
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 9,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 8, 0)
        };
        Grid.SetColumn(caret, 0);

        var labelText = new TextBlock
        {
            Text = label,
            Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis
        };
        Grid.SetColumn(labelText, 1);

        var countText = new TextBlock
        {
            Text = count.ToString(),
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 10,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(8, 0, 0, 0)
        };
        Grid.SetColumn(countText, 2);

        grid.Children.Add(caret);
        grid.Children.Add(labelText);
        grid.Children.Add(countText);
        border.Child = grid;

        // Click toggles expand/collapse. Use MouseLeftButtonUp + a dragPending flag so a
        // drag operation doesn't also fire the toggle.
        System.Windows.Point dragStartPos = default;
        bool dragPending = false;
        border.PreviewMouseLeftButtonDown += (_, me) =>
        {
            dragStartPos = me.GetPosition(null);
            dragPending = true;
        };
        border.PreviewMouseLeftButtonUp += (_, _) =>
        {
            if (!dragPending) return;
            dragPending = false;
            if (group != null)
            {
                group.IsExpanded = !group.IsExpanded;
            }
            else
            {
                _vm.Settings.UngroupedSectionExpanded = !_vm.Settings.UngroupedSectionExpanded;
            }
            _ = _vm.SaveStateAsync();
            RebuildSidebarOrder();
        };

        // Real groups: drag source for reorder + right-click menu mirroring the strip tab.
        if (group != null)
        {
            border.PreviewMouseMove += (_, me) =>
            {
                if (!dragPending || me.LeftButton != MouseButtonState.Pressed) return;
                var diff = me.GetPosition(null) - dragStartPos;
                if (Math.Abs(diff.X) > SystemParameters.MinimumHorizontalDragDistance ||
                    Math.Abs(diff.Y) > SystemParameters.MinimumVerticalDragDistance)
                {
                    dragPending = false;
                    System.Windows.DragDrop.DoDragDrop(border, "group:" + group.Id,
                        System.Windows.DragDropEffects.Move);
                }
            };

            var menu = new System.Windows.Controls.ContextMenu();
            var moveUp = new System.Windows.Controls.MenuItem { Header = "Move up" };
            moveUp.Click += (_, _) =>
            {
                int idx = IndexOfUserGroup(group.Id);
                if (idx > 0) _vm.MoveGroup(group.Id, idx - 1);
            };
            menu.Items.Add(moveUp);
            var moveDown = new System.Windows.Controls.MenuItem { Header = "Move down" };
            moveDown.Click += (_, _) =>
            {
                int idx = IndexOfUserGroup(group.Id);
                if (idx >= 0 && idx < _sessionManager.Groups.Count - 1)
                    _vm.MoveGroup(group.Id, idx + 1);
            };
            menu.Items.Add(moveDown);
            AddGroupBulkActionItems(menu, group.Id, group.Name);
            var rename = new System.Windows.Controls.MenuItem { Header = "Rename group…" };
            rename.Click += (_, _) => PromptRenameGroup(group.Id, group.Name);
            menu.Items.Add(rename);
            var delete = new System.Windows.Controls.MenuItem { Header = "Delete group" };
            delete.Click += (_, _) =>
            {
                var r = MessageBox.Show(
                    $"Delete group '{group.Name}'? Sessions in this group will revert to Ungrouped.",
                    "Delete group", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No);
                if (r == MessageBoxResult.Yes) _vm.RemoveGroup(group.Id);
            };
            menu.Items.Add(delete);
            menu.Opened += (_, _) =>
            {
                int idx = IndexOfUserGroup(group.Id);
                moveUp.IsEnabled = idx > 0;
                moveDown.IsEnabled = idx >= 0 && idx < _sessionManager.Groups.Count - 1;
            };
            border.ContextMenu = menu;
        }

        // Drop target: sessions get assigned to this group; another group dropped here
        // reorders to before this group.
        border.AllowDrop = true;
        border.DragEnter += (_, e) =>
        {
            if (IsSessionDragPayload(e.Data) || IsGroupDragPayload(e.Data, exceptGroupId: group?.Id))
            {
                border.Background = new SolidColorBrush(Color.FromArgb(0x55, 0x89, 0xb4, 0xfa));
                e.Handled = true;
            }
        };
        border.DragLeave += (_, _) =>
        {
            border.Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25));
        };
        border.DragOver += (_, e) =>
        {
            if (IsSessionDragPayload(e.Data)
                || (group != null && IsGroupDragPayload(e.Data, exceptGroupId: group.Id)))
            {
                e.Effects = System.Windows.DragDropEffects.Move;
                e.Handled = true;
            }
        };
        border.Drop += (_, e) =>
        {
            border.Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25));
            if (IsSessionDragPayload(e.Data))
            {
                string sessionId = (string)e.Data.GetData(System.Windows.DataFormats.StringFormat);
                var targets = _vm.ResolveActionTargets(sessionId);
                _vm.AssignSessionsToGroup(targets, group?.Id);
                e.Handled = true;
                return;
            }
            if (group != null && IsGroupDragPayload(e.Data, exceptGroupId: group.Id))
            {
                string payload = (string)e.Data.GetData(System.Windows.DataFormats.StringFormat);
                string draggedId = payload.Substring("group:".Length);
                int targetIdx = IndexOfUserGroup(group.Id);
                if (targetIdx >= 0) _vm.MoveGroup(draggedId, targetIdx);
                e.Handled = true;
            }
        };

        return border;
    }

    /// <summary>
    /// Appends bulk-action items (Remote control, Sleep all, Wake all, Close all) for
    /// <paramref name="groupId"/>, wrapped in leading + trailing separators. Items are
    /// enabled/disabled in <c>menu.Opened</c> based on live vs. dormant counts in the group.
    /// </summary>
    private void AddGroupBulkActionItems(System.Windows.Controls.ContextMenu menu, string groupId, string groupName)
    {
        menu.Items.Add(new System.Windows.Controls.Separator());

        var remoteControl = new System.Windows.Controls.MenuItem { Header = "Remote control all in group" };
        remoteControl.Click += (_, _) =>
        {
            foreach (var vm in _vm.Sessions.Where(v => v.Session.GroupId == groupId).ToList())
            {
                vm.Bridge?.SendToTerminal("/remote-control\r");
                vm.AlertDetector?.NotifyUserInteracted();
            }
        };
        menu.Items.Add(remoteControl);

        var sleepAll = new System.Windows.Controls.MenuItem { Header = "Sleep all" };
        sleepAll.Click += (_, _) =>
        {
            foreach (var vm in _vm.Sessions.Where(v => v.Session.GroupId == groupId).ToList())
                SleepSession(vm);
        };
        menu.Items.Add(sleepAll);

        var wakeAll = new System.Windows.Controls.MenuItem { Header = "Wake all" };
        wakeAll.Click += async (_, _) =>
        {
            var dormant = _sessionManager.Sessions
                .Where(s => s.GroupId == groupId && s.IsDormant)
                .ToList();
            foreach (var session in dormant)
                await WakeSessionAsync(session);
        };
        menu.Items.Add(wakeAll);

        var closeAll = new System.Windows.Controls.MenuItem { Header = "Close all…" };
        closeAll.Click += (_, _) =>
        {
            var targets = _vm.Sessions.Where(v => v.Session.GroupId == groupId).ToList();
            if (targets.Count == 0) return;
            var r = MessageBox.Show(
                $"Close {targets.Count} session(s) in group '{groupName}'? They will be removed permanently.",
                "Close all", MessageBoxButton.YesNo, MessageBoxImage.Question, MessageBoxResult.No);
            if (r != MessageBoxResult.Yes) return;
            foreach (var vm in targets)
                vm.CloseCommand.Execute(null);
        };
        menu.Items.Add(closeAll);

        menu.Items.Add(new System.Windows.Controls.Separator());

        menu.Opened += (_, _) =>
        {
            int liveCount = _vm.Sessions.Count(v => v.Session.GroupId == groupId);
            int dormantCount = _sessionManager.Sessions.Count(s => s.GroupId == groupId && s.IsDormant);
            remoteControl.IsEnabled = liveCount > 0;
            sleepAll.IsEnabled = liveCount > 0;
            wakeAll.IsEnabled = dormantCount > 0;
            closeAll.IsEnabled = liveCount > 0;
        };
    }

    /// <summary>True when the drag payload is "group:<id>" and (if specified) not the excepted id.</summary>
    private static bool IsGroupDragPayload(System.Windows.IDataObject data, string? exceptGroupId = null)
    {
        if (!data.GetDataPresent(System.Windows.DataFormats.StringFormat)) return false;
        var payload = data.GetData(System.Windows.DataFormats.StringFormat) as string;
        if (string.IsNullOrEmpty(payload) || !payload!.StartsWith("group:")) return false;
        string id = payload.Substring("group:".Length);
        if (id == "__ALL__" || id == GroupFilter.Ungrouped) return false;
        if (exceptGroupId != null && id == exceptGroupId) return false;
        return true;
    }

    /// <summary>
    /// True when the drag payload is a session id (the raw vm.Id used by BuildSidebarItem),
    /// not a group-reorder payload (prefixed with "group:") or some unrelated data.
    /// </summary>
    private static bool IsSessionDragPayload(System.Windows.IDataObject data)
    {
        if (!data.GetDataPresent(System.Windows.DataFormats.StringFormat)) return false;
        var payload = data.GetData(System.Windows.DataFormats.StringFormat) as string;
        return !string.IsNullOrEmpty(payload) && !payload!.StartsWith("group:");
    }

    private void UpdateGroupStripActiveState()
    {
        string activeKey = "group:" + (_vm.ActiveGroupId ?? "__ALL__");
        foreach (Border tab in GroupStripPanel.Children.OfType<Border>())
        {
            if (tab.Tag is not string key || !key.StartsWith("group:")) continue;
            bool isActive = key == activeKey;
            tab.Background = isActive
                ? new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44))
                : Brushes.Transparent;
            tab.BorderBrush = isActive
                ? new SolidColorBrush(Color.FromRgb(0x89, 0xb4, 0xfa))
                : Brushes.Transparent;
        }
    }

    private void PromptCreateGroup()
    {
        string? name = InputBoxDialog.Prompt(this, "New group", "Group name:", "");
        if (string.IsNullOrWhiteSpace(name)) return;
        _vm.CreateGroup(name.Trim());
    }

    private void PromptRenameGroup(string groupId, string currentName)
    {
        string? name = InputBoxDialog.Prompt(this, "Rename group", "Group name:", currentName);
        if (string.IsNullOrWhiteSpace(name) || name.Trim() == currentName) return;
        _vm.RenameGroup(groupId, name.Trim());
    }

    // ── Worktree siblings ─────────────────────────────────────────────────────

    /// <summary>Sets HasWorktreeSiblings = true on every live session that shares its RepoRoot with another.</summary>
    private void RecomputeWorktreeSiblings()
    {
        var byRoot = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        foreach (var s in _vm.Sessions)
        {
            if (string.IsNullOrEmpty(s.RepoRoot)) continue;
            byRoot[s.RepoRoot] = byRoot.GetValueOrDefault(s.RepoRoot) + 1;
        }
        bool anyChanged = false;
        foreach (var s in _vm.Sessions)
        {
            bool siblings = !string.IsNullOrEmpty(s.RepoRoot) && byRoot[s.RepoRoot] > 1;
            if (s.HasWorktreeSiblings != siblings)
            {
                s.HasWorktreeSiblings = siblings;
                anyChanged = true;
            }
        }

        // Auto-cluster: pull every session in a multi-sibling repo next to its anchor so
        // siblings always group up, even when added/removed out of order or imported from
        // a non-worktree creation path. This runs on Add/Remove/Reset and on RepoRoot
        // resolve — but NOT on Move (the CollectionChanged filter skips it) so user
        // drag-reorder is preserved.
        bool reordered = ApplyClusteredOrder();

        if ((anyChanged || reordered) && _vm.Settings.ShowWorktreeClusters)
            RebuildSidebarOrder();
        if (reordered)
            _ = _vm.SaveStateAsync();
    }

    /// <summary>
    /// Reorders <see cref="MainViewModel.Sessions"/> (and the underlying SessionManager
    /// list) so every session sharing a RepoRoot sits adjacent to its first-seen anchor.
    /// First-occurrence order is preserved between clusters and for solo sessions, so a
    /// non-worktree session never gets shuffled past unrelated ones. Returns true when
    /// at least one Move happened.
    /// </summary>
    private bool ApplyClusteredOrder()
    {
        if (_vm.Sessions.Count < 2) return false;

        // Compute the desired order: stable group-by RepoRoot, anchored at first occurrence.
        var desired = new List<SessionViewModel>(_vm.Sessions.Count);
        var anchorIdx = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        static string ClusterKey(SessionViewModel s) =>
            s.RepoRoot is { Length: > 0 } root ? root : "__solo:" + s.Id;

        foreach (var s in _vm.Sessions)
        {
            string key = ClusterKey(s);
            if (!anchorIdx.TryGetValue(key, out _))
            {
                anchorIdx[key] = desired.Count;
                desired.Add(s);
            }
            else
            {
                // Insert immediately after the last existing member of this cluster.
                int insertAt = anchorIdx[key];
                for (int i = anchorIdx[key]; i < desired.Count; i++)
                {
                    if (ClusterKey(desired[i]) == key) insertAt = i + 1;
                }
                desired.Insert(insertAt, s);
            }
        }

        // Apply minimal Move operations to align current order with desired.
        bool moved = false;
        for (int i = 0; i < desired.Count; i++)
        {
            if (_vm.Sessions[i].Id == desired[i].Id) continue;
            int j = -1;
            for (int k = i + 1; k < _vm.Sessions.Count; k++)
                if (_vm.Sessions[k].Id == desired[i].Id) { j = k; break; }
            if (j <= i) continue;
            // Mirror in the SessionManager model so state.json persists the new order.
            _sessionManager.MoveSession(_vm.Sessions[j].Id, i);
            _vm.Sessions.Move(j, i);
            moved = true;
        }
        return moved;
    }

    // ── Per-session context menu ──────────────────────────────────────────────

    private System.Windows.Controls.ContextMenu BuildSessionContextMenu(SessionViewModel vm)
    {
        var menu = new System.Windows.Controls.ContextMenu();
        var targetIds = _vm.ResolveActionTargets(vm.Id);
        bool isMulti = targetIds.Count > 1;
        string countSuffix = isMulti ? $" ({targetIds.Count})" : "";

        // Spawn-near-parent + worktree actions — single-target only
        if (!isMulti)
        {
            // Rename — in-place editor inside the sidebar row (same path as double-click).
            // Stored in _sidebarRenameActions when the sidebar row is built.
            if (_sidebarRenameActions.TryGetValue(vm.Id, out var renameAction))
            {
                var renameItem = new System.Windows.Controls.MenuItem { Header = "Rename session" };
                renameItem.Click += (_, _) => renameAction();
                menu.Items.Add(renameItem);
            }

            // Folder actions — only when there's a local working folder to open.
            if (!vm.Session.IsRemote && !string.IsNullOrEmpty(vm.Session.WorkingFolder))
            {
                var explorerItem = new System.Windows.Controls.MenuItem { Header = "Open in Explorer" };
                explorerItem.Click += (_, _) => vm.OpenInExplorerCommand.Execute(null);
                menu.Items.Add(explorerItem);

                var psItem = new System.Windows.Controls.MenuItem { Header = "Open PowerShell here" };
                psItem.Click += (_, _) => LaunchPowerShellInFolder(vm.WorkingFolder, vm.GroupId);
                menu.Items.Add(psItem);

                if (vm.Session.IsWsl)
                {
                    var wslConsoleItem = new System.Windows.Controls.MenuItem { Header = "Open WSL console here" };
                    wslConsoleItem.Click += (_, _) => LaunchWslConsoleFromSession(vm.Session);
                    menu.Items.Add(wslConsoleItem);
                }
            }

            menu.Items.Add(new System.Windows.Controls.Separator());

            var dupItem = new System.Windows.Controls.MenuItem { Header = "Duplicate session" };
            dupItem.Click += async (_, _) => await DuplicateSessionAsync(vm);
            menu.Items.Add(dupItem);

            if (!vm.Session.IsRemote && !string.IsNullOrEmpty(vm.Session.WorkingFolder))
            {
                var newHere = new System.Windows.Controls.MenuItem { Header = "New session here…" };
                newHere.Click += (_, _) => OpenNewSessionDialogFromParent(vm);
                menu.Items.Add(newHere);

                var wtItem = new System.Windows.Controls.MenuItem { Header = "New worktree from this branch…" };
                wtItem.Click += async (_, _) => await OpenNewWorktreeDialogAsync(vm);
                menu.Items.Add(wtItem);

                // Sibling worktree submenu — populated on demand so we don't shell out to git
                // for every right-click on a non-worktree session.
                var siblingMenu = new System.Windows.Controls.MenuItem { Header = "New session in sibling worktree" };
                siblingMenu.Items.Add(new System.Windows.Controls.MenuItem
                {
                    Header = "(loading…)",
                    IsEnabled = false
                });
                bool populated = false;
                siblingMenu.SubmenuOpened += async (_, _) =>
                {
                    if (populated) return;
                    populated = true;
                    var worktrees = await GitService.ListWorktreesAsync(vm.Session.WorkingFolder);
                    siblingMenu.Items.Clear();
                    var liveFolders = new HashSet<string>(
                        _vm.Sessions.Select(s => NormalizePath(s.Session.WorkingFolder)),
                        StringComparer.OrdinalIgnoreCase);
                    string selfNorm = NormalizePath(vm.Session.WorkingFolder);
                    int added = 0;
                    foreach (var w in worktrees)
                    {
                        if (w.IsBare) continue;
                        string wn = NormalizePath(w.Path);
                        if (wn == selfNorm) continue;
                        if (liveFolders.Contains(wn)) continue;
                        string label = string.IsNullOrEmpty(w.Branch)
                            ? System.IO.Path.GetFileName(w.Path)
                            : $"{System.IO.Path.GetFileName(w.Path)}  ⎇ {w.Branch}";
                        var mi = new System.Windows.Controls.MenuItem { Header = label, Tag = w.Path };
                        mi.Click += async (_, _) => await LaunchSessionInSiblingWorktreeAsync(vm, w.Path);
                        siblingMenu.Items.Add(mi);
                        added++;
                    }
                    if (added == 0)
                    {
                        siblingMenu.Items.Add(new System.Windows.Controls.MenuItem
                        {
                            Header = "(no other worktrees available)",
                            IsEnabled = false
                        });
                    }
                };
                menu.Items.Add(siblingMenu);
            }

            // Session commands submenu — only for single targets.
            var runMenu = new System.Windows.Controls.MenuItem { Header = "Session commands" };
            if (vm.Session.RunCommands.Count == 0)
            {
                runMenu.Items.Add(new System.Windows.Controls.MenuItem
                {
                    Header = "(none configured)",
                    IsEnabled = false,
                });
            }
            else
            {
                foreach (var item in vm.Session.RunCommands)
                {
                    string lbl = item.IsDefault ? $"▶ {item.Label} (default)" : $"▶ {item.Label}";
                    var mi = new System.Windows.Controls.MenuItem { Header = lbl };
                    mi.Click += (_, _) => vm.Runner.Run(item);
                    runMenu.Items.Add(mi);
                }
            }
            runMenu.Items.Add(new System.Windows.Controls.Separator());
            var editMi = new System.Windows.Controls.MenuItem { Header = "Edit commands…" };
            editMi.Click += (_, _) => OpenRunCommandsEditor(vm);
            runMenu.Items.Add(editMi);
            menu.Items.Add(runMenu);

            menu.Items.Add(new System.Windows.Controls.Separator());
        }

        // Add to group submenu — always available
        var addTo = new System.Windows.Controls.MenuItem { Header = $"Add to group{countSuffix}" };
        foreach (var g in _sessionManager.Groups.OrderBy(g => g.SortOrder))
        {
            var gid = g.Id; // capture
            var item = new System.Windows.Controls.MenuItem { Header = g.Name };
            item.Click += (_, _) => _vm.AssignSessionsToGroup(targetIds, gid);
            addTo.Items.Add(item);
        }
        if (_sessionManager.Groups.Count > 0)
            addTo.Items.Add(new System.Windows.Controls.Separator());
        var newGroup = new System.Windows.Controls.MenuItem { Header = "New group…" };
        newGroup.Click += (_, _) =>
        {
            string? name = InputBoxDialog.Prompt(this, "New group", "Group name:", "");
            if (string.IsNullOrWhiteSpace(name)) return;
            var g = _vm.CreateGroup(name.Trim());
            _vm.AssignSessionsToGroup(targetIds, g.Id);
        };
        addTo.Items.Add(newGroup);
        menu.Items.Add(addTo);

        var removeFrom = new System.Windows.Controls.MenuItem { Header = $"Remove from group{countSuffix}" };
        removeFrom.Click += (_, _) => _vm.AssignSessionsToGroup(targetIds, null);
        menu.Items.Add(removeFrom);

        menu.Items.Add(new System.Windows.Controls.Separator());
        var sleepItem = new System.Windows.Controls.MenuItem { Header = $"Sleep{countSuffix}" };
        sleepItem.Click += (_, _) =>
        {
            foreach (var id in targetIds)
            {
                var target = _vm.Sessions.FirstOrDefault(s => s.Id == id);
                if (target != null) SleepSession(target);
            }
        };
        menu.Items.Add(sleepItem);
        var closeItem = new System.Windows.Controls.MenuItem { Header = $"Close{countSuffix}" };
        closeItem.Click += (_, _) =>
        {
            foreach (var id in targetIds.ToArray())
            {
                var target = _vm.Sessions.FirstOrDefault(s => s.Id == id);
                target?.CloseCommand.Execute(null);
            }
        };
        menu.Items.Add(closeItem);

        return menu;
    }

    private static string NormalizePath(string p)
    {
        if (string.IsNullOrEmpty(p)) return "";
        try { return System.IO.Path.TrimEndingDirectorySeparator(System.IO.Path.GetFullPath(p)).Replace('\\', '/'); }
        catch { return p; }
    }

    // ── Worktree creation ─────────────────────────────────────────────────────

    private async Task OpenNewWorktreeDialogAsync(SessionViewModel source)
    {
        string? repoRoot = source.RepoRoot
            ?? await GitService.GetRepoRootAsync(source.Session.WorkingFolder);
        if (string.IsNullOrEmpty(repoRoot))
        {
            MessageBox.Show(this,
                $"'{source.Session.WorkingFolder}' is not inside a git repository.",
                "Not a git repo", MessageBoxButton.OK, MessageBoxImage.Information);
            return;
        }

        var branches = await GitService.ListBranchesAsync(repoRoot);
        string currentBranch = source.GitBranch ?? "";
        var dlg = new NewWorktreeDialog(repoRoot, currentBranch, branches) { Owner = this };
        if (dlg.ShowDialog() != true) return;

        var (ok, err) = await GitService.CreateWorktreeAsync(
            repoRoot, dlg.TargetPath, dlg.BranchOrRef, dlg.CreateBranch);
        if (!ok)
        {
            MessageBox.Show(this, "git worktree add failed:\n\n" + err,
                "Worktree error", MessageBoxButton.OK, MessageBoxImage.Error);
            return;
        }

        // Clone the source session config into a new session pointing at the worktree.
        var newSession = _sessionManager.CreateSession(
            dlg.SessionName,
            dlg.TargetPath,
            source.Session.Command,
            source.Session.Args,
            string.IsNullOrEmpty(source.Session.GroupId) ? null : source.Session.GroupId,
            source.Session.ColorOverride);
        InheritSessionKindFrom(newSession, source.Session);
        newSession.ProfileFontFamily = source.Session.ProfileFontFamily;
        newSession.ProfileFontSize = source.Session.ProfileFontSize;
        newSession.ProfileFontWeight = source.Session.ProfileFontWeight;
        newSession.ProfileFontLigatures = source.Session.ProfileFontLigatures;
        newSession.ProfileCursorShape = source.Session.ProfileCursorShape;
        newSession.ProfileCursorBlink = source.Session.ProfileCursorBlink;
        newSession.ProfilePadding = source.Session.ProfilePadding;
        newSession.ProfileBackgroundOpacity = source.Session.ProfileBackgroundOpacity;
        newSession.ProfileRetroEffect = source.Session.ProfileRetroEffect;
        newSession.ProfileColorSchemeJson = source.Session.ProfileColorSchemeJson;

        SeedRunCommandsAsync(newSession);
        await LaunchSessionAsync(newSession);
    }

    // ── Sidebar drag-and-drop ─────────────────────────────────────────────────

    // Called once from constructor after SidebarSessionList is accessible.
    private void SetupSidebarDrop()
    {
        SidebarSessionList.AllowDrop = true;
        SidebarSessionList.DragOver += (_, e) =>
        {
            bool inlineMode = _vm.Settings.GroupDisplayMode == Models.GroupDisplayMode.InlineHeaders;
            bool ok = IsSessionDragPayload(e.Data)
                || (inlineMode && IsGroupDragPayload(e.Data));
            e.Effects = ok
                ? System.Windows.DragDropEffects.Move
                : System.Windows.DragDropEffects.None;
            e.Handled = true;
        };
        SidebarSessionList.Drop += (_, e) =>
        {
            var mode = _vm.Settings.GroupDisplayMode;

            if (IsSessionDragPayload(e.Data))
            {
                string draggedId = (string)e.Data.GetData(System.Windows.DataFormats.StringFormat);
                var pos = e.GetPosition(SidebarSessionList);

                // Inline mode: detect which group section the cursor fell in. If different
                // from the dragged session's current group, reassign — otherwise the move
                // would just "snap back" once RebuildSidebarOrder regrouped by GroupId.
                if (mode == Models.GroupDisplayMode.InlineHeaders
                    && _sessionManager.Groups.Count > 0)
                {
                    var (targetGroupId, sessionsIndex) = ResolveInlineSessionDropTarget(pos.Y);
                    string normalizedTarget = targetGroupId ?? "";
                    var dragged = _vm.Sessions.FirstOrDefault(s => s.Id == draggedId);
                    if (dragged != null && (dragged.GroupId ?? "") != normalizedTarget)
                    {
                        // Apply the cross-section reassign to the whole selection set when
                        // the dragged session is part of one — mirrors the header-drop UX.
                        var targets = _vm.ResolveActionTargets(draggedId);
                        _vm.AssignSessionsToGroup(targets, targetGroupId);
                    }
                    _vm.MoveSession(draggedId, sessionsIndex);
                    RebuildSidebarOrder();
                    return;
                }

                int targetIndex = GetSidebarDropIndex(pos);
                _vm.MoveSession(draggedId, targetIndex);
                RebuildSidebarOrder();
                return;
            }
            // Inline mode: group reorder drops onto the sidebar resolve to a position
            // among the visible group headers. The fixed FilterStrip's own drop handler
            // is in SetupGroupStripDrop — that's the path when the strip is showing.
            if (mode == Models.GroupDisplayMode.InlineHeaders
                && IsGroupDragPayload(e.Data))
            {
                string payload = (string)e.Data.GetData(System.Windows.DataFormats.StringFormat);
                string draggedGroupId = payload.Substring("group:".Length);
                int targetIdx = GetInlineGroupDropIndex(e.GetPosition(SidebarSessionList));
                if (targetIdx >= 0) _vm.MoveGroup(draggedGroupId, targetIdx);
            }
        };
    }

    private int GetSidebarDropIndex(System.Windows.Point pos)
    {
        var children = SidebarSessionList.Children.OfType<Border>().ToList();
        for (int i = 0; i < children.Count; i++)
        {
            var itemPos = children[i].TranslatePoint(new System.Windows.Point(0, 0), SidebarSessionList);
            double midY = itemPos.Y + children[i].ActualHeight / 2;
            if (pos.Y < midY) return i;
        }
        return children.Count;
    }

    /// <summary>
    /// In inline-headers mode, maps a drop Y to (target group, _vm.Sessions index).
    /// Walks SidebarSessionList in order: each "groupheader:" item switches the current
    /// section; session items inside the section define insertion midpoints. If the drop
    /// falls past every section's midline, the session lands at the end of whatever
    /// section the cursor was last inside.
    /// </summary>
    private (string? targetGroupId, int sessionsIndex) ResolveInlineSessionDropTarget(double y)
    {
        // Pass 1: build per-section bounds + session lists by walking the visible children.
        // A section's vertical bounds are [its-header-bottom .. next-header-top] (or [0..]/[..MaxValue]
        // for the Ungrouped implicit section / the final section).
        var sections = new List<(string? groupId, double endY, List<Border> sessions)>();
        string? currentGroupId = null;
        double currentEndY = double.MaxValue;
        var currentSessions = new List<Border>();

        foreach (System.Windows.UIElement child in SidebarSessionList.Children)
        {
            if (child is not Border item) continue;
            string? tag = item.Tag as string;
            if (tag == null) continue;
            var itemPos = item.TranslatePoint(new System.Windows.Point(0, 0), SidebarSessionList);

            if (tag.StartsWith("groupheader:"))
            {
                // Close out the current section at the new header's top.
                currentEndY = itemPos.Y;
                // Skip empty sections: if no sessions appeared before this header
                // (e.g. drop above the first header in a no-ungrouped sidebar), an
                // empty entry would let Pass 2 match the section but fall through to
                // the "past every session" tail and silently retarget to ungrouped.
                if (currentSessions.Count > 0)
                    sections.Add((currentGroupId, currentEndY, currentSessions));
                // Start a new section.
                string id = tag.Substring("groupheader:".Length);
                currentGroupId = id == GroupFilter.Ungrouped ? null : id;
                currentSessions = new List<Border>();
                continue;
            }
            if (tag.StartsWith("cluster:") || tag.StartsWith("dormant:")) continue;
            currentSessions.Add(item);
        }
        if (currentSessions.Count > 0)
            sections.Add((currentGroupId, double.MaxValue, currentSessions));

        // Pass 2: find the section whose end-Y is past the drop Y, then resolve the
        // insertion point within it.
        foreach (var sec in sections)
        {
            if (y >= sec.endY) continue;

            foreach (var sItem in sec.sessions)
            {
                var sp = sItem.TranslatePoint(new System.Windows.Point(0, 0), SidebarSessionList);
                if (y < sp.Y + sItem.ActualHeight / 2)
                {
                    string? sid = sItem.Tag as string;
                    if (!string.IsNullOrEmpty(sid))
                    {
                        for (int j = 0; j < _vm.Sessions.Count; j++)
                            if (_vm.Sessions[j].Id == sid) return (sec.groupId, j);
                    }
                }
            }

            // Past every session in this section — insert at the section's tail, i.e. just
            // after the last existing member of this group in _vm.Sessions.
            int lastIdx = -1;
            for (int j = 0; j < _vm.Sessions.Count; j++)
            {
                if ((_vm.Sessions[j].GroupId ?? "") == (sec.groupId ?? ""))
                    lastIdx = j;
            }
            return (sec.groupId, lastIdx < 0 ? _vm.Sessions.Count : lastIdx + 1);
        }

        // Past every section (shouldn't normally happen — last section's endY is MaxValue).
        return (currentGroupId, _vm.Sessions.Count);
    }

    /// <summary>
    /// In inline-headers mode, maps a Y coordinate within SidebarSessionList to a user-group
    /// insertion index (0-based within SessionManager.Groups). The implicit Ungrouped header
    /// is skipped — only real-group headers are valid targets.
    /// </summary>
    private int GetInlineGroupDropIndex(System.Windows.Point pos)
    {
        var headers = SidebarSessionList.Children.OfType<Border>()
            .Where(b => b.Tag is string t && t.StartsWith("groupheader:")
                && t != "groupheader:" + GroupFilter.Ungrouped)
            .ToList();
        for (int i = 0; i < headers.Count; i++)
        {
            var itemPos = headers[i].TranslatePoint(new System.Windows.Point(0, 0), SidebarSessionList);
            double midY = itemPos.Y + headers[i].ActualHeight / 2;
            if (pos.Y < midY) return i;
        }
        return headers.Count;
    }

    /// <summary>
    /// Wires the GroupStripPanel as a drop target for group-tab drags. Drop position is
    /// resolved relative to the user-group tabs only — "All" and "Ungrouped" stay pinned
    /// at the top of the strip and aren't valid drop targets.
    /// </summary>
    private void SetupGroupStripDrop()
    {
        GroupStripPanel.AllowDrop = true;
        GroupStripPanel.DragOver += (_, e) =>
        {
            bool ok = e.Data.GetDataPresent(System.Windows.DataFormats.StringFormat)
                && ((string)e.Data.GetData(System.Windows.DataFormats.StringFormat))
                    .StartsWith("group:");
            e.Effects = ok ? System.Windows.DragDropEffects.Move : System.Windows.DragDropEffects.None;
            e.Handled = true;
        };
        GroupStripPanel.Drop += (_, e) =>
        {
            if (!e.Data.GetDataPresent(System.Windows.DataFormats.StringFormat)) return;
            string payload = (string)e.Data.GetData(System.Windows.DataFormats.StringFormat);
            if (!payload.StartsWith("group:")) return;
            string draggedId = payload.Substring("group:".Length);
            if (draggedId == "__ALL__" || draggedId == GroupFilter.Ungrouped) return;
            int targetIndex = GetGroupStripDropIndex(e.GetPosition(GroupStripPanel));
            _vm.MoveGroup(draggedId, targetIndex);
        };
    }

    /// <summary>
    /// Maps a Y-coordinate within GroupStripPanel to a user-group insertion index (0-based
    /// within SessionManager.Groups). The "All" tab is at child 0, "Ungrouped" at child 1,
    /// and the "+" footer trails the user groups — only children in [2 .. 2+N-1] are tabs.
    /// </summary>
    private int GetGroupStripDropIndex(System.Windows.Point pos)
    {
        var allTabs = GroupStripPanel.Children.OfType<Border>().ToList();
        // Skip the fixed "All" and "Ungrouped" pseudo-tabs at indices 0 and 1, and the "+"
        // footer at the end (it has no group: tag).
        var groupTabs = allTabs
            .Where(b => b.Tag is string t
                && t.StartsWith("group:")
                && t != "group:__ALL__"
                && t != "group:" + GroupFilter.Ungrouped)
            .ToList();
        for (int i = 0; i < groupTabs.Count; i++)
        {
            var itemPos = groupTabs[i].TranslatePoint(new System.Windows.Point(0, 0), GroupStripPanel);
            double midY = itemPos.Y + groupTabs[i].ActualHeight / 2;
            if (pos.Y < midY) return i;
        }
        return groupTabs.Count;
    }

    private void RebuildSidebarOrder()
    {
        SidebarSessionList.Children.Clear();
        var mode = _vm.Settings.GroupDisplayMode;
        bool inlineMode = mode == Models.GroupDisplayMode.InlineHeaders
            && _sessionManager.Groups.Count > 0;

        // Snapshot _vm.Sessions into a dictionary up front so Resolve is O(1) per call.
        // RebuildSidebarOrder fires on group filter / membership / drag-reorder / launch,
        // so the previous FirstOrDefault-in-a-loop was O(n²) for the saved-session list.
        var liveById = new Dictionary<string, SessionViewModel>(_vm.Sessions.Count);
        foreach (var v in _vm.Sessions) liveById[v.Id] = v;

        // Resolve a saved ShellSession to either its live sidebar item + VM, or a
        // launching placeholder (vm == null). Returns null if the session is dormant
        // or has no rendered representation yet (no UI built).
        (Border item, SessionViewModel? vm)? Resolve(ShellSession s)
        {
            if (s.IsDormant) return null;
            if (liveById.TryGetValue(s.Id, out var liveVm)
                && _sessionUi.TryGetValue(liveVm.Id, out var ui))
                return (ui.sidebarItem, liveVm);
            if (_launchingSidebarItems.TryGetValue(s.Id, out var ph))
                return (ph, null);
            return null;
        }

        bool MatchesActiveGroupForSession(ShellSession s)
        {
            var activeGroupId = _vm.ActiveGroupId;
            if (activeGroupId == null) return true;
            if (activeGroupId == GroupFilter.Ungrouped) return string.IsNullOrEmpty(s.GroupId);
            return s.GroupId == activeGroupId;
        }

        if (inlineMode)
        {
            // Ungrouped section first (only shown when it has members or there are groups).
            var ungrouped = _sessionManager.Sessions
                .Where(s => string.IsNullOrEmpty(s.GroupId) && !s.IsDormant)
                .Select(s => Resolve(s))
                .Where(r => r.HasValue)
                .Select(r => r!.Value)
                .ToList();
            if (ungrouped.Count > 0)
            {
                bool ungroupedExpanded = _vm.Settings.UngroupedSectionExpanded;
                SidebarSessionList.Children.Add(BuildInlineGroupHeader(null, ungrouped.Count, ungroupedExpanded));
                if (ungroupedExpanded) AppendSessionsWithClusters(ungrouped);
            }
            // Each user group, in SortOrder.
            foreach (var g in _sessionManager.Groups.OrderBy(g => g.SortOrder))
            {
                var members = _sessionManager.Sessions
                    .Where(s => s.GroupId == g.Id && !s.IsDormant)
                    .Select(s => Resolve(s))
                    .Where(r => r.HasValue)
                    .Select(r => r!.Value)
                    .ToList();
                SidebarSessionList.Children.Add(BuildInlineGroupHeader(g, members.Count, g.IsExpanded));
                if (g.IsExpanded) AppendSessionsWithClusters(members);
            }
        }
        else
        {
            // Flat list mode (None or FilterStrip).
            var visible = new List<(Border item, SessionViewModel? vm)>();
            foreach (var s in _sessionManager.Sessions)
            {
                if (s.IsDormant) continue;
                if (mode == Models.GroupDisplayMode.FilterStrip && !MatchesActiveGroupForSession(s))
                    continue;
                var r = Resolve(s);
                if (r.HasValue) visible.Add(r.Value);
            }
            AppendSessionsWithClusters(visible);
        }

        // Dormant entries always render at the bottom of the sidebar regardless of filter
        // or display mode so they remain reachable (and a user filtering by category isn't
        // surprised by missing entries).
        foreach (var item in _dormantSidebarItems.Values)
            SidebarSessionList.Children.Add(item);
        UpdateSidebarActiveState();
        RefreshTerminalLayout();
    }

    /// <summary>
    /// Appends a list of session sidebar items to <see cref="SidebarSessionList"/>, inserting
    /// worktree cluster headers above runs of 2+ adjacent siblings (when enabled). Items with
    /// a null VM are launching placeholders — they're rendered inline but skipped by the
    /// cluster detector (their RepoRoot isn't known yet).
    /// </summary>
    private void AppendSessionsWithClusters(List<(Border item, SessionViewModel? vm)> items)
    {
        var clusters = ComputeWorktreeClusters(items);
        int clusterIdx = 0;
        for (int i = 0; i < items.Count; i++)
        {
            var (item, vm) = items[i];
            if (clusterIdx < clusters.Count && clusters[clusterIdx].start == i)
            {
                var (s, e, root) = clusters[clusterIdx];
                int count = e - s + 1;
                string accent = vm?.AccentColor ?? "#89b4fa";
                SidebarSessionList.Children.Add(BuildWorktreeClusterHeader(root, count, accent));
                clusterIdx++;
            }
            SidebarSessionList.Children.Add(item);
        }
    }

    /// <summary>
    /// Returns the ranges of <paramref name="visible"/> that should render under a worktree
    /// cluster header — runs of 2+ adjacent sessions sharing a RepoRoot. Empty when
    /// the setting is off. Launching placeholders (vm == null) have no RepoRoot, so they
    /// act as cluster boundaries — adjacent live siblings around a placeholder won't be
    /// detected as a cluster until the placeholder is replaced by a real sidebar item.
    /// </summary>
    private List<(int start, int end, string repoRoot)> ComputeWorktreeClusters(
        IReadOnlyList<(Border item, SessionViewModel? vm)> visible)
    {
        var clusters = new List<(int, int, string)>();
        if (!_vm.Settings.ShowWorktreeClusters) return clusters;

        int runStart = -1;
        string? runRoot = null;
        for (int i = 0; i < visible.Count; i++)
        {
            string? root = visible[i].vm?.RepoRoot;
            if (!string.IsNullOrEmpty(root) && root == runRoot) continue;
            if (runStart >= 0 && (i - runStart) >= 2)
                clusters.Add((runStart, i - 1, runRoot!));
            runStart = string.IsNullOrEmpty(root) ? -1 : i;
            runRoot = root;
        }
        if (runStart >= 0 && (visible.Count - runStart) >= 2)
            clusters.Add((runStart, visible.Count - 1, runRoot!));
        return clusters;
    }

    /// <summary>
    /// Tiny banner inserted above an adjacent run of worktree siblings: shows the shared
    /// repo name and the count of siblings in this view, tinted with the cluster's
    /// accent color. Tag is prefixed with "cluster:" so UpdateSidebarActiveState skips it.
    /// </summary>
    private Border BuildWorktreeClusterHeader(string repoRoot, int count, string accentHex)
    {
        string repoName = System.IO.Path.GetFileName(repoRoot.TrimEnd('/', '\\'));
        if (string.IsNullOrEmpty(repoName)) repoName = "worktrees";

        Color accentColor;
        try { accentColor = (Color)ColorConverter.ConvertFromString(accentHex); }
        catch { accentColor = Color.FromRgb(0x89, 0xb4, 0xfa); }

        var header = new Border
        {
            Margin = new Thickness(0, 8, 0, 0),
            Padding = new Thickness(10, 3, 8, 3),
            Background = new SolidColorBrush(Color.FromArgb(0x33, accentColor.R, accentColor.G, accentColor.B)),
            BorderBrush = new SolidColorBrush(accentColor),
            BorderThickness = new Thickness(0, 0, 0, 1),
            CornerRadius = new CornerRadius(4, 4, 0, 0),
            Tag = "cluster:" + repoRoot,
            ToolTip = $"{count} worktrees of {repoName} are open"
        };

        var text = new TextBlock
        {
            FontSize = 10,
            FontWeight = FontWeights.SemiBold
        };
        text.Inlines.Add(new System.Windows.Documents.Run($"\U0001F4C1 {repoName}")
        {
            Foreground = new SolidColorBrush(accentColor)
        });
        text.Inlines.Add(new System.Windows.Documents.Run($"  · {count}")
        {
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86))
        });
        header.Child = text;
        return header;
    }

    private void RefreshSidebarItem(string sessionId) => UpdateAlertBadge();

    private void UpdateAlertBadge()
    {
        int count = _vm.Sessions.Count(s => s.NeedsAttention);
        AlertBadge.Visibility = count > 0 ? Visibility.Visible : Visibility.Collapsed;
        AlertCountText.Text = count > 0 ? $"{count} alert{(count > 1 ? "s" : "")}" : "";
    }

    // ── Layout ────────────────────────────────────────────────────────────────

    private void Layout_Single_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.Single);
    private void Layout_Two_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.TwoColumn);
    private void Layout_Three_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.ThreeColumn);
    private void Layout_Grid_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.TwoByTwo);
    private void Layout_TwoRow_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.TwoRow);
    private void Layout_Four_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.FourColumn);
    private void Layout_Six_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.SixColumn);
    private void Layout_SixTwo_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.SixByTwo);
    private void Layout_SixThree_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.SixByThree);
    private void Layout_ThreeByThree_Click(object s, RoutedEventArgs e) => SetLayout(LayoutMode.ThreeByThree);

    private void SetLayout(LayoutMode mode)
    {
        _currentLayout = mode;
        _vm.Layout = mode;
        _layoutViewportOffset = 0;
        RefreshTerminalLayout();
    }

    private void RefreshTerminalLayout()
    {
        TerminalGrid.Children.Clear();
        TerminalGrid.RowDefinitions.Clear();
        TerminalGrid.ColumnDefinitions.Clear();

        // When FilterGridByActiveGroup is on, restrict the panes to the effective group:
        //   FilterStrip mode → the explicitly selected tab (ActiveGroupId)
        //   InlineHeaders mode → the ActiveSession's group (no tab strip exists, so the
        //     focused session is the implicit "current group" selector)
        // In None mode there is no group concept, so no filter applies.
        IEnumerable<SessionViewModel> source = _vm.Sessions;
        if (_vm.Settings.FilterGridByActiveGroup && _vm.EffectiveActiveGroupId != null)
        {
            source = source.Where(_vm.SessionMatchesEffectiveGroup);
        }
        var sessions = source.ToList();
        if (sessions.Count == 0)
        {
            EmptyState.Visibility = Visibility.Visible;
            return;
        }
        EmptyState.Visibility = Visibility.Collapsed;

        switch (_currentLayout)
        {
            case LayoutMode.TwoColumn:
            {
                var view = GetViewportSessions(sessions, 2);
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                PlaceTerminal(view, 0, 0, 0);
                PlaceTerminal(view, 1, 0, 1);
                break;
            }

            case LayoutMode.ThreeColumn:
            {
                var view = GetViewportSessions(sessions, 3);
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                PlaceTerminal(view, 0, 0, 0);
                PlaceTerminal(view, 1, 0, 1);
                PlaceTerminal(view, 2, 0, 2);
                break;
            }

            case LayoutMode.TwoByTwo:
            {
                var view = GetViewportSessions(sessions, 4);
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                PlaceTerminal(view, 0, 0, 0);
                PlaceTerminal(view, 1, 0, 1);
                PlaceTerminal(view, 2, 1, 0);
                PlaceTerminal(view, 3, 1, 1);
                break;
            }

            case LayoutMode.TwoRow:
            {
                var view = GetViewportSessions(sessions, 2);
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                PlaceTerminal(view, 0, 0, 0);
                PlaceTerminal(view, 1, 1, 0);
                break;
            }

            case LayoutMode.FourColumn:
            {
                var view = GetViewportSessions(sessions, 4);
                for (int i = 0; i < 4; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                for (int i = 0; i < 4; i++) PlaceTerminal(view, i, 0, i);
                break;
            }

            case LayoutMode.SixColumn:
            {
                var view = GetViewportSessions(sessions, 6);
                for (int i = 0; i < 6; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                for (int i = 0; i < 6; i++) PlaceTerminal(view, i, 0, i);
                break;
            }

            case LayoutMode.SixByTwo:
            {
                var view = GetViewportSessions(sessions, 12);
                for (int i = 0; i < 6; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                for (int i = 0; i < 12; i++) PlaceTerminal(view, i, i / 6, i % 6);
                break;
            }

            case LayoutMode.SixByThree:
            {
                var view = GetViewportSessions(sessions, 18);
                for (int i = 0; i < 6; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                for (int r = 0; r < 3; r++) TerminalGrid.RowDefinitions.Add(new RowDefinition());
                for (int i = 0; i < 18; i++) PlaceTerminal(view, i, i / 6, i % 6);
                break;
            }

            case LayoutMode.ThreeByThree:
            {
                var view = GetViewportSessions(sessions, 9);
                for (int i = 0; i < 3; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                for (int r = 0; r < 3; r++) TerminalGrid.RowDefinitions.Add(new RowDefinition());
                for (int i = 0; i < 9; i++) PlaceTerminal(view, i, i / 3, i % 3);
                break;
            }

            default: // Single
            {
                // If the active session is filtered out by the group filter, fall back
                // to the first visible session so the pane doesn't show a hidden tab.
                var target = (_vm.ActiveSession != null && sessions.Contains(_vm.ActiveSession))
                    ? _vm.ActiveSession
                    : sessions.FirstOrDefault();
                if (target != null && _sessionUi.TryGetValue(target.Id, out var ui))
                {
                    TerminalGrid.Children.Add(ui.terminalWrapper);
                    target.Bridge?.FitTerminal();
                }
                break;
            }
        }
    }

    /// <summary>
    /// Returns a sublist of sessions sized to <paramref name="slotCount"/>, sliding the viewport
    /// so the active session is always visible. The viewport offset is persisted across calls
    /// and only moves minimally to bring the active session into view.
    /// </summary>
    private List<SessionViewModel> GetViewportSessions(List<SessionViewModel> sessions, int slotCount)
    {
        if (sessions.Count <= slotCount)
        {
            _layoutViewportOffset = 0;
            return sessions;
        }

        if (_vm.ActiveSession != null)
        {
            int activeIdx = sessions.IndexOf(_vm.ActiveSession);
            if (activeIdx >= 0)
            {
                // Scroll left if active session is before the current viewport
                if (activeIdx < _layoutViewportOffset)
                    _layoutViewportOffset = activeIdx;
                // Scroll right if active session is past the current viewport
                else if (activeIdx >= _layoutViewportOffset + slotCount)
                    _layoutViewportOffset = activeIdx - slotCount + 1;
            }
        }

        _layoutViewportOffset = Math.Clamp(_layoutViewportOffset, 0, sessions.Count - slotCount);
        return sessions.Skip(_layoutViewportOffset).Take(slotCount).ToList();
    }

    private void PlaceTerminal(List<SessionViewModel> sessions, int index, int row, int col)
    {
        if (index >= sessions.Count) return;
        var session = sessions[index];
        if (!_sessionUi.TryGetValue(session.Id, out var ui)) return;

        Grid.SetRow(ui.terminalWrapper, row);
        Grid.SetColumn(ui.terminalWrapper, col);
        TerminalGrid.Children.Add(ui.terminalWrapper);
    }

    private Border BuildTerminalWrapper(SessionViewModel vm, WebView2 webView)
    {
        string accent = vm.AccentColor;
        var accentColor = (Color)ColorConverter.ConvertFromString(accent);

        var wrapper = new Border
        {
            BorderThickness = new Thickness(0, 3, 0, 0),
            BorderBrush = new SolidColorBrush(accentColor),
            Background = new SolidColorBrush(Color.FromRgb(30, 30, 46))
        };

        // Outer "active ring" — constant 2px frame; transparent unless this session is active
        // (constant thickness avoids layout shift when activating/deactivating)
        var activeRing = new Border
        {
            BorderThickness = new Thickness(2),
            BorderBrush = Brushes.Transparent,
            Tag = accent
        };

        var outer = new DockPanel();

        // Terminal toolbar
        var toolbar = new Border
        {
            Background = new SolidColorBrush(Color.FromRgb(24, 24, 37)),
            Padding = new Thickness(8, 3, 8, 3)
        };
        var toolbarContent = new DockPanel();

        // Claude badge — visible only for claude sessions
        var claudeBadge = new Border
        {
            Background = new SolidColorBrush(Color.FromArgb(0x28, 0x89, 0xb4, 0xfa)), // subtle blue tint
            BorderBrush = new SolidColorBrush(Color.FromArgb(0x60, 0x89, 0xb4, 0xfa)),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(3),
            Padding = new Thickness(4, 1, 4, 1),
            Margin = new Thickness(0, 0, 6, 0),
            VerticalAlignment = VerticalAlignment.Center,
            Visibility = ClaudeSessionService.IsClaudeCommand(vm.Command)
                ? Visibility.Visible : Visibility.Collapsed
        };
        claudeBadge.Child = new TextBlock
        {
            Text = "claude",
            Foreground = new SolidColorBrush(Color.FromRgb(0x89, 0xb4, 0xfa)),
            FontSize = 9,
            FontWeight = FontWeights.SemiBold
        };

        var titleBlock = new TextBlock
        {
            Text = vm.DisplayName,
            Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 8, 0)
        };

        var folderBlock = new TextBlock
        {
            Text = vm.WorkingFolder,
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 10,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        // Status dot for terminal toolbar
        var termStatusDot = new Ellipse
        {
            Width = 8,
            Height = 8,
            Fill = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)), // gray = running
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 6, 0),
            ToolTip = "Running"
        };

        var explorerBtn = new WpfButton
        {
            Content = "📁",
            ToolTip = "Open folder in Explorer",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 12,
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(4, 2, 4, 2),
            Margin = new Thickness(0, 0, 4, 0)
        };
        explorerBtn.Click += (_, _) => vm.OpenInExplorerCommand.Execute(null);

        // PowerShell quick-launch button
        var toolbarPsBtn = new WpfButton
        {
            Content = ">_",
            ToolTip = "Open PowerShell in this folder",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 11,
            FontFamily = new FontFamily("Consolas"),
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(5, 2, 5, 2),
            Margin = new Thickness(0, 0, 4, 0)
        };
        toolbarPsBtn.Click += (_, _) => LaunchPowerShellInFolder(vm.WorkingFolder, vm.GroupId);

        // Notes button
        var notesBtn = new WpfButton
        {
            Content = "📝",
            ToolTip = "Project notes",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 12,
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(4, 2, 4, 2),
            Margin = new Thickness(0, 0, 4, 0)
        };

        // ── Play (run) button + chevron ──────────────────────────────────────────
        var playBtn = new WpfButton
        {
            Content = "▶",
            ToolTip = "Run the default command (F5)",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xe3, 0xa1)),  // green ▶
            FontSize = 12,
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(4, 2, 2, 2),
            Margin = new Thickness(0, 0, 0, 0),
            Visibility = vm.Session.RunCommands.Count == 0 ? Visibility.Collapsed : Visibility.Visible,
        };
        playBtn.Click += (_, _) => RunDefaultCommand(vm);
        playBtn.MouseRightButtonUp += (_, e) => { OpenRunCommandsEditor(vm); e.Handled = true; };

        var chevronBtn = new WpfButton
        {
            Content = "▼",
            ToolTip = "Run commands…",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 9,
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(2, 2, 4, 2),
            Margin = new Thickness(0, 0, 4, 0),
            Visibility = playBtn.Visibility,
        };
        chevronBtn.Click += (_, _) => ShowRunCommandsDropdown(vm, chevronBtn);

        // Sleep (dormant) button — keeps the session in the sidebar but stops the PTY
        var sleepBtn = new WpfButton
        {
            Content = "💤",
            ToolTip = "Sleep session (keep it but stop the terminal)",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 12,
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(4, 2, 4, 2),
            Margin = new Thickness(0, 0, 4, 0)
        };
        sleepBtn.Click += (_, _) => SleepSession(vm);

        // Close button — tied to the same path as the sidebar ✕ (vm.CloseCommand).
        // Sits at the far right of the toolbar so the terminal toolbar is a complete,
        // canonical home for per-session actions (#29). Right margin nudges it slightly
        // inside the toolbar padding so the ✕ doesn't kiss the toolbar edge.
        var closeBtn = new WpfButton
        {
            Content = "✕",
            ToolTip = "Close session",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 12,
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(4, 2, 4, 2),
            Margin = new Thickness(0)
        };
        closeBtn.Click += (_, _) => vm.CloseCommand.Execute(null);

        DockPanel.SetDock(closeBtn, Dock.Right);
        DockPanel.SetDock(termStatusDot, Dock.Right);
        DockPanel.SetDock(explorerBtn, Dock.Right);
        DockPanel.SetDock(toolbarPsBtn, Dock.Right);
        DockPanel.SetDock(notesBtn, Dock.Right);
        DockPanel.SetDock(sleepBtn, Dock.Right);
        DockPanel.SetDock(chevronBtn, Dock.Right);
        DockPanel.SetDock(playBtn, Dock.Right);
        DockPanel.SetDock(claudeBadge, Dock.Left);
        DockPanel.SetDock(titleBlock, Dock.Left);
        // Dock.Right children are stacked right-to-left in declaration order, so this
        // puts closeBtn at the far right edge, then termStatusDot to its left, etc.
        toolbarContent.Children.Add(closeBtn);
        toolbarContent.Children.Add(termStatusDot);
        toolbarContent.Children.Add(explorerBtn);
        toolbarContent.Children.Add(toolbarPsBtn);
        toolbarContent.Children.Add(notesBtn);
        toolbarContent.Children.Add(sleepBtn);
        toolbarContent.Children.Add(chevronBtn);
        toolbarContent.Children.Add(playBtn);
        toolbarContent.Children.Add(claudeBadge);
        toolbarContent.Children.Add(titleBlock);
        toolbarContent.Children.Add(folderBlock);
        toolbar.Child = toolbarContent;

        // Notes panel (collapsible, docked between toolbar and terminal)
        var notesBox = new WpfTextBox
        {
            Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25)),
            Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            BorderThickness = new Thickness(0),
            AcceptsReturn = true,
            AcceptsTab = true,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            FontFamily = new FontFamily("Consolas"),
            FontSize = 12,
            Padding = new Thickness(12, 8, 12, 8),
            TextWrapping = TextWrapping.Wrap,
            CaretBrush = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4))
        };
        var notesPanel = new Border
        {
            Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25)),
            BorderBrush = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Height = 160,
            Visibility = Visibility.Collapsed
        };
        notesPanel.Child = notesBox;

        bool notesLoaded = false;
        notesBtn.Click += async (_, _) =>
        {
            if (notesPanel.Visibility == Visibility.Collapsed)
            {
                if (!notesLoaded && _searchService != null && !string.IsNullOrEmpty(vm.WorkingFolder))
                {
                    notesBox.Text = await _searchService.GetNoteAsync(vm.WorkingFolder) ?? "";
                    notesLoaded = true;
                }
                notesPanel.Visibility = Visibility.Visible;
                notesBox.Focus();
            }
            else
            {
                notesPanel.Visibility = Visibility.Collapsed;
            }
        };

        // Debounce save on each keystroke
        System.Threading.Timer? noteDebounce = null;
        notesBox.TextChanged += (_, _) =>
        {
            noteDebounce?.Dispose();
            noteDebounce = new System.Threading.Timer(_ =>
                Dispatcher.Invoke(() =>
                {
                    if (_searchService != null && !string.IsNullOrEmpty(vm.WorkingFolder))
                        _ = _searchService.SaveNoteAsync(vm.WorkingFolder, notesBox.Text);
                }),
                null, 1000, System.Threading.Timeout.Infinite);
        };

        // ── Chips strip ──────────────────────────────────────────────────────────
        var chipsPanel = new StackPanel
        {
            Orientation = Orientation.Horizontal,
        };
        var chipsStrip = new Border
        {
            Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25)),
            BorderBrush = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Padding = new Thickness(8, 2, 8, 2),
            Visibility = Visibility.Collapsed,  // shown only when at least one RunInstance exists
            Child = new ScrollViewer
            {
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
                Content = chipsPanel,
            },
        };

        // ── Drawer ──────────────────────────────────────────────────────────────
        var drawerHeader = new TextBlock
        {
            Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 8, 0),
        };
        var drawerStopBtn = MakeDrawerActionButton("⏹ Stop");
        var drawerCopyBtn = MakeDrawerActionButton("📋 Copy");
        var drawerSendBtn = MakeDrawerActionButton("↗ Send to terminal");

        var drawerActions = new DockPanel { LastChildFill = false };
        DockPanel.SetDock(drawerHeader, Dock.Left);
        DockPanel.SetDock(drawerStopBtn, Dock.Right);
        DockPanel.SetDock(drawerCopyBtn, Dock.Right);
        DockPanel.SetDock(drawerSendBtn, Dock.Right);
        drawerActions.Children.Add(drawerHeader);
        drawerActions.Children.Add(drawerSendBtn);
        drawerActions.Children.Add(drawerCopyBtn);
        drawerActions.Children.Add(drawerStopBtn);

        var drawerText = new WpfTextBox
        {
            Background = new SolidColorBrush(Color.FromRgb(0x18, 0x18, 0x25)),
            Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            BorderThickness = new Thickness(0),
            IsReadOnly = true,
            AcceptsReturn = true,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            FontFamily = new FontFamily("Consolas"),
            FontSize = 12,
            Padding = new Thickness(8, 6, 8, 6),
            TextWrapping = TextWrapping.NoWrap,
        };

        var drawerInner = new DockPanel();
        DockPanel.SetDock(drawerActions, Dock.Top);
        drawerInner.Children.Add(drawerActions);
        drawerInner.Children.Add(drawerText);

        var drawer = new Border
        {
            Background = new SolidColorBrush(Color.FromRgb(0x11, 0x11, 0x1b)),
            BorderBrush = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
            BorderThickness = new Thickness(0, 0, 0, 1),
            Height = 200,
            Visibility = Visibility.Collapsed,
            Child = drawerInner,
        };

        drawerStopBtn.Click += (_, _) =>
        {
            if (_drawerItemBySession.TryGetValue(vm.Id, out var itemId))
                vm.Runner.Stop(itemId);
        };
        drawerCopyBtn.Click += (_, _) =>
        {
            if (_drawerItemBySession.TryGetValue(vm.Id, out var itemId) &&
                vm.Runner.GetInstance(itemId) is { } inst)
            {
                string text = !string.IsNullOrEmpty(drawerText.SelectedText)
                    ? drawerText.SelectedText
                    : inst.SnapshotOutput();
                try { System.Windows.Clipboard.SetText(text); } catch { }
            }
        };
        drawerSendBtn.Click += (_, _) => SendRunOutputToTerminal(vm, drawerText);

        DockPanel.SetDock(toolbar, Dock.Top);
        DockPanel.SetDock(chipsStrip, Dock.Top);
        DockPanel.SetDock(drawer, Dock.Top);
        DockPanel.SetDock(notesPanel, Dock.Top);
        outer.Children.Add(toolbar);
        outer.Children.Add(chipsStrip);
        outer.Children.Add(drawer);
        outer.Children.Add(notesPanel);
        outer.Children.Add(webView);
        wrapper.Child = outer;
        activeRing.Child = wrapper;

        // Subscribe to waiting state changes for the status dot
        vm.PropertyChanged += (_, args) =>
        {
            if (args.PropertyName is nameof(SessionViewModel.IsWaitingForInput)
                                  or nameof(SessionViewModel.IsWaitingForApproval))
            {
                Dispatcher.Invoke(() =>
                {
                    if (vm.IsWaitingForInput)
                    {
                        termStatusDot.Fill = new SolidColorBrush(Color.FromRgb(0xa6, 0xe3, 0xa1));
                        termStatusDot.ToolTip = "Waiting for input";
                    }
                    else if (vm.IsWaitingForApproval)
                    {
                        termStatusDot.Fill = new SolidColorBrush(Color.FromRgb(0xff, 0xb7, 0x4d));
                        termStatusDot.ToolTip = "Tool approval needed";
                    }
                    else
                    {
                        termStatusDot.Fill = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86));
                        termStatusDot.ToolTip = "Running";
                    }
                });
            }
        };

        _runControls[vm.Id] = (playBtn, chevronBtn, chipsStrip, chipsPanel, drawer,
            drawerText, drawerHeader, drawerStopBtn, drawerCopyBtn, drawerSendBtn);

        vm.Runner.InstancesChanged += () => Dispatcher.Invoke(() => RefreshTerminalRunControls(vm.Id));

        // RepoRoot lands async after GitService probes — when AccentColor shifts, repaint
        // the top stripe and refresh the active-ring lookup so terminal and sidebar agree.
        vm.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName != nameof(SessionViewModel.AccentColor)) return;
            Dispatcher.Invoke(() =>
            {
                try
                {
                    var c = (Color)ColorConverter.ConvertFromString(vm.AccentColor);
                    wrapper.BorderBrush = new SolidColorBrush(c);
                    activeRing.Tag = vm.AccentColor;
                }
                catch { }
                UpdateActiveTerminalHighlight();
            });
        };

        return activeRing;
    }

    // ── Session close ─────────────────────────────────────────────────────────

    private void OnSessionVmClosed(SessionViewModel vm)
    {
        if (_sessionUi.TryGetValue(vm.Id, out var ui))
        {
            SidebarSessionList.Children.Remove(ui.sidebarItem);
            _sessionUi.Remove(vm.Id);
        }
        _runControls.Remove(vm.Id);
        _drawerItemBySession.Remove(vm.Id);
        _sidebarActionPanels.Remove(vm.Id);
        _sidebarRenameActions.Remove(vm.Id);
        if (_selectionAnchorId == vm.Id) _selectionAnchorId = null;
        _sessionManager.RemoveSession(vm.Id);
        RefreshTerminalLayout();
        UpdateAlertBadge();

        if (_vm.Sessions.Count == 0 && _dormantSidebarItems.Count == 0)
            EmptyState.Visibility = Visibility.Visible;
    }

    // ── Sleep / wake (dormant sessions) ───────────────────────────────────────

    /// <summary>
    /// Sleeps a session: tears down the live PTY/terminal but keeps the session
    /// definition in state so it can be woken later. Replaces the active sidebar
    /// item with a muted "dormant" entry at the bottom of the list.
    /// </summary>
    private void SleepSession(SessionViewModel vm)
    {
        // Defensive — vm.Dispose() at the bottom of this method also kills runs via
        // Runner.Dispose, but stopping them explicitly here ensures child processes
        // die BEFORE UI teardown, avoiding any chance of an orphan output event
        // racing the disposing UI controls.
        vm.Runner.StopAll();
        var session = vm.Session;
        session.IsDormant = true;

        if (_selectionAnchorId == vm.Id) _selectionAnchorId = null;
        if (_sessionUi.TryGetValue(vm.Id, out var ui))
        {
            if (TerminalGrid.Children.Contains(ui.terminalWrapper))
                TerminalGrid.Children.Remove(ui.terminalWrapper);
            SidebarSessionList.Children.Remove(ui.sidebarItem);
            _sessionUi.Remove(vm.Id);
        }
        _runControls.Remove(vm.Id);
        _drawerItemBySession.Remove(vm.Id);
        _sidebarActionPanels.Remove(vm.Id);
        _sidebarRenameActions.Remove(vm.Id);

        // Remove the VM directly — bypass CloseRequested so the ShellSession is
        // NOT removed from the SessionManager (we want to keep it for wake-up).
        _vm.Sessions.Remove(vm);
        if (_vm.ActiveSession == vm)
            _vm.ActiveSession = _vm.Sessions.LastOrDefault();
        vm.Dispose();

        AddDormantSidebarItem(session);

        RefreshTerminalLayout();
        UpdateAlertBadge();
        EmptyState.Visibility = _vm.Sessions.Count == 0 && _dormantSidebarItems.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

        _ = _vm.SaveStateAsync();
    }

    /// <summary>
    /// Wakes a dormant session: removes the dormant sidebar entry and relaunches
    /// the session via LaunchSessionAsync (same path as restore-on-startup).
    /// </summary>
    private async Task WakeSessionAsync(ShellSession session)
    {
        session.IsDormant = false;
        if (_dormantSidebarItems.TryGetValue(session.Id, out var dormantItem))
        {
            SidebarSessionList.Children.Remove(dormantItem);
            _dormantSidebarItems.Remove(session.Id);
        }

        try
        {
            await LaunchSessionAsync(session, restoring: true);
        }
        catch (Exception ex)
        {
            Log($"Wake FAILED for '{session.Name}': {ex}");
            // Restore the dormant entry so the user doesn't lose access to the session
            session.IsDormant = true;
            AddDormantSidebarItem(session);
            MessageBox.Show($"Failed to wake '{session.Name}': {ex.Message}",
                "Wake Error", MessageBoxButton.OK, MessageBoxImage.Warning);
        }
        _ = _vm.SaveStateAsync();
    }

    private void AddDormantSidebarItem(ShellSession session)
    {
        var item = BuildDormantSidebarItem(session);
        _dormantSidebarItems[session.Id] = item;
        SidebarSessionList.Children.Add(item);
        EmptyState.Visibility = Visibility.Collapsed;
    }

    /// <summary>
    /// Stages a "launching" placeholder sidebar entry for <paramref name="session"/>. The
    /// placeholder is rendered by <see cref="RebuildSidebarOrder"/> in saved order alongside
    /// live items, and removed automatically when <see cref="LaunchSessionAsync"/> registers
    /// the real sidebar item for the same session id.
    /// </summary>
    private void AddLaunchingSidebarItem(ShellSession session)
    {
        var item = BuildLaunchingSidebarItem(session);
        _launchingSidebarItems[session.Id] = item;
        EmptyState.Visibility = Visibility.Collapsed;
    }

    /// <summary>
    /// Builds a muted sidebar row visually similar to the dormant entry (so layout is
    /// stable when it's later replaced) but with an animated dot indicating "launching".
    /// No buttons — interaction is disabled until the session has actually started.
    /// </summary>
    private Border BuildLaunchingSidebarItem(ShellSession session)
    {
        string accentHex = GetAccentForSession(session);
        var accentColor = (Color)ColorConverter.ConvertFromString(accentHex);

        var container = new Border
        {
            Margin = new Thickness(0, 2, 0, 2),
            Background = Brushes.Transparent,
            BorderBrush = Brushes.Transparent,
            BorderThickness = new Thickness(2),
            CornerRadius = new CornerRadius(6),
            Tag = "launching:" + session.Id,
            Opacity = 0.75,
            ToolTip = "Launching…"
        };

        var inner = new Grid();
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(6) });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var stripe = new Border
        {
            Background = new SolidColorBrush(Color.FromArgb(0x80, accentColor.R, accentColor.G, accentColor.B)),
            CornerRadius = new CornerRadius(4, 0, 0, 4),
            Width = 6
        };
        Grid.SetColumn(stripe, 0);

        var textPanel = new StackPanel { Margin = new Thickness(8, 6, 4, 6) };

        string displayName = string.IsNullOrWhiteSpace(session.Name)
            ? session.DefaultDisplayName
            : session.Name;

        var nameText = new TextBlock
        {
            Text = displayName,
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 13,
            FontStyle = FontStyles.Italic,
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        string folderShort = session.FolderShort;

        var folderText = new TextBlock
        {
            Text = "Launching… · " + folderShort,
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 10,
            Margin = new Thickness(0, 1, 0, 0),
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        textPanel.Children.Add(nameText);
        textPanel.Children.Add(folderText);
        Grid.SetColumn(textPanel, 1);

        // Pulsing dot using a DoubleAnimation on the dot's Opacity. Color matches the
        // accent so it's clear which session this row represents.
        var spinner = new Ellipse
        {
            Width = 8,
            Height = 8,
            Fill = new SolidColorBrush(accentColor),
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(4, 0, 8, 0)
        };
        var anim = new System.Windows.Media.Animation.DoubleAnimation
        {
            From = 0.25,
            To = 1.0,
            Duration = TimeSpan.FromMilliseconds(900),
            AutoReverse = true,
            RepeatBehavior = System.Windows.Media.Animation.RepeatBehavior.Forever
        };
        spinner.BeginAnimation(System.Windows.UIElement.OpacityProperty, anim);
        Grid.SetColumn(spinner, 2);

        inner.Children.Add(stripe);
        inner.Children.Add(textPanel);
        inner.Children.Add(spinner);
        container.Child = inner;

        return container;
    }

    private Border BuildDormantSidebarItem(ShellSession session)
    {
        string accentHex = GetAccentForSession(session);
        var accentColor = (Color)ColorConverter.ConvertFromString(accentHex);

        var container = new Border
        {
            Margin = new Thickness(0, 2, 0, 2),
            Background = Brushes.Transparent,
            Cursor = System.Windows.Input.Cursors.Hand,
            CornerRadius = new CornerRadius(6),
            Tag = "dormant:" + session.Id,
            Opacity = 0.55,
            ToolTip = "Click to wake this session"
        };

        var inner = new Grid();
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(6) });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var stripe = new Border
        {
            Background = new SolidColorBrush(Color.FromArgb(0x60, accentColor.R, accentColor.G, accentColor.B)),
            CornerRadius = new CornerRadius(4, 0, 0, 4),
            Width = 6
        };
        Grid.SetColumn(stripe, 0);

        var textPanel = new StackPanel { Margin = new Thickness(8, 6, 4, 6) };

        string displayName = string.IsNullOrWhiteSpace(session.Name)
            ? session.DefaultDisplayName
            : session.Name;

        var nameText = new TextBlock
        {
            Text = displayName,
            Foreground = new SolidColorBrush(Color.FromRgb(0x93, 0x99, 0xb2)),
            FontSize = 13,
            FontStyle = FontStyles.Italic,
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        string folderShort = session.FolderShort;

        var folderText = new TextBlock
        {
            Text = folderShort,
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 10,
            Margin = new Thickness(0, 1, 0, 0),
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        textPanel.Children.Add(nameText);
        textPanel.Children.Add(folderText);
        Grid.SetColumn(textPanel, 1);

        // Sleep icon
        var sleepIcon = new TextBlock
        {
            Text = "💤",
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(4, 0, 4, 0),
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86))
        };
        Grid.SetColumn(sleepIcon, 2);

        // Permanent-delete button — only way to fully remove a dormant session
        var deleteBtn = MakeMiniButton("✕", "Delete this dormant session", () =>
        {
            var result = MessageBox.Show(
                $"Permanently delete dormant session '{displayName}'?",
                "Delete session", MessageBoxButton.YesNo, MessageBoxImage.Question,
                MessageBoxResult.No);
            if (result != MessageBoxResult.Yes) return;
            SidebarSessionList.Children.Remove(container);
            _dormantSidebarItems.Remove(session.Id);
            _sessionManager.RemoveSession(session.Id);
            if (_vm.Sessions.Count == 0 && _dormantSidebarItems.Count == 0)
                EmptyState.Visibility = Visibility.Visible;
            _ = _vm.SaveStateAsync();
        });
        var btnPanel = new StackPanel
        {
            Orientation = Orientation.Vertical,
            Margin = new Thickness(0, 4, 4, 4),
            VerticalAlignment = VerticalAlignment.Center
        };
        btnPanel.Children.Add(deleteBtn);
        Grid.SetColumn(btnPanel, 3);

        inner.Children.Add(stripe);
        inner.Children.Add(textPanel);
        inner.Children.Add(sleepIcon);
        inner.Children.Add(btnPanel);
        container.Child = inner;

        // Hover effect
        container.MouseEnter += (_, _) =>
        {
            container.Opacity = 0.85;
            container.Background = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44));
        };
        container.MouseLeave += (_, _) =>
        {
            container.Opacity = 0.55;
            container.Background = Brushes.Transparent;
        };

        // Click anywhere on the row → wake
        container.MouseLeftButtonDown += async (_, e) =>
        {
            // Don't trigger wake when clicking the delete button
            if (e.OriginalSource is System.Windows.DependencyObject dep
                && IsDescendantOf(dep, btnPanel)) return;
            await WakeSessionAsync(session);
        };

        return container;
    }

    private static bool IsDescendantOf(System.Windows.DependencyObject node, System.Windows.DependencyObject ancestor)
    {
        for (var n = node; n != null; n = System.Windows.Media.VisualTreeHelper.GetParent(n))
            if (n == ancestor) return true;
        return false;
    }

    private static string GetAccentForSession(ShellSession s) =>
        s.ColorOverride ?? ColorService.GetHexColor(s.AccentKey);

    // ── Search ────────────────────────────────────────────────────────────────

    private void ToggleSearch_Click(object s, RoutedEventArgs e)
    {
        bool show = SearchPanelHost.Visibility != Visibility.Visible;
        SearchPanelHost.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        _vm.ShowSearch = show;
        if (show) SearchBox.Focus();
    }

    private void CloseSearchButton_Click(object s, RoutedEventArgs e)
    {
        SearchPanelHost.Visibility = Visibility.Collapsed;
        _vm.ShowSearch = false;
    }

    private void SearchBox_KeyDown(object s, WpfKeyEventArgs e)
    {
        if (e.Key == Key.Enter)
            DoSearch();
        else if (e.Key == Key.Escape)
            CloseSearchButton_Click(s, new RoutedEventArgs());
    }

    private void Search_Click(object s, RoutedEventArgs e) => DoSearch();

    private async void DoSearch()
    {
        if (_searchService == null) return;
        var results = await _searchService.SearchAsync(SearchBox.Text, _vm.Settings.MaxSearchResults);
        SearchResults.ItemsSource = results;
    }

    private async void SearchResult_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not SearchResult result)
            return;

        // Note results: open folder in Explorer
        if (result.IsNote)
        {
            if (!string.IsNullOrEmpty(result.FolderPath) && Directory.Exists(result.FolderPath))
                System.Diagnostics.Process.Start("explorer.exe", result.FolderPath);
            return;
        }

        // Try to find matching live session
        var session = _vm.Sessions.FirstOrDefault(s => s.Id == result.SessionId);
        if (session == null)
        {
            await TryRelaunchFromHistoryAsync(result.SessionId, null, result.SessionName);
            return;
        }

        _vm.FocusSessionCommand.Execute(session);
        UpdateSidebarActiveState();
        RefreshTerminalLayout();

        if (_vm.Settings.SearchCollapseAfterNavigate)
        {
            SearchPanelHost.Visibility = Visibility.Collapsed;
            _vm.ShowSearch = false;
        }
    }

    private async Task TryRelaunchFromHistoryAsync(string? sessionId, string? folderPath, string? sessionName)
    {
        if (_searchService == null) return;

        SessionHistoryEntry? entry = null;
        if (!string.IsNullOrEmpty(sessionId))
            entry = await _searchService.GetSessionHistoryAsync(sessionId);
        if (entry == null && !string.IsNullOrEmpty(folderPath))
            entry = await _searchService.GetLatestSessionHistoryForFolderAsync(folderPath);

        if (entry == null)
        {
            string folder = folderPath ?? "";
            if (!string.IsNullOrEmpty(folder))
            {
                var r = MessageBox.Show(
                    $"Session '{sessionName}' is no longer open.\nOpen a new session in that folder?",
                    "Session Not Found", MessageBoxButton.YesNo, MessageBoxImage.Question);
                if (r == MessageBoxResult.Yes)
                    OpenNewSessionDialog(folder);
            }
            return;
        }

        var answer = MessageBox.Show(
            $"Session '{entry.SessionName}' ({entry.WorkingFolder}) is not currently open.\nRelaunch it?",
            "Relaunch Session?", MessageBoxButton.YesNo, MessageBoxImage.Question);
        if (answer != MessageBoxResult.Yes) return;

        var newSession = _sessionManager.CreateSession(
            entry.SessionName, entry.WorkingFolder, entry.Command, entry.Args, entry.GroupId);
        SeedRunCommandsAsync(newSession);
        await LaunchSessionAsync(newSession);
    }

    // ── Command helper ────────────────────────────────────────────────────────

    private void ToggleCommandHelper_Click(object s, RoutedEventArgs e)
    {
        CommandHelperPanel.Visibility = CommandHelperPanel.Visibility == Visibility.Visible
            ? Visibility.Collapsed : Visibility.Visible;
    }

    private void BuildShortcutPanel()
    {
        foreach (var preset in CommandPresetsService.InSessionShortcuts)
        {
            var btn = new WpfButton
            {
                Content = preset.Label,
                ToolTip = preset.Description,
                Margin = new Thickness(0, 0, 6, 4),
                Padding = new Thickness(8, 3, 8, 3),
                Background = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44)),
                Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
                BorderThickness = new Thickness(0),
                FontSize = 12,
                Cursor = System.Windows.Input.Cursors.Hand,
                FontFamily = new FontFamily("Consolas")
            };
            btn.Click += (_, _) =>
            {
                _vm.ActiveSession?.Bridge?.SendToTerminal(preset.Label + "\r");
                _vm.ActiveSession?.AlertDetector?.NotifyUserInteracted();
            };
            ShortcutPanel.Children.Add(btn);
        }
    }

    // ── Update check ─────────────────────────────────────────────────────────

    private async Task CheckForUpdatesAsync()
    {
        try
        {
            // Small delay so the window finishes rendering before we hit the network
            await Task.Delay(4000);
            var result = await UpdateService.CheckAsync();
            if (result == null) return;

            Dispatcher.Invoke(() =>
            {
                _updateReleaseUrl = result.ReleaseUrl;
                UpdateBadgeText.Text = $"↑ v{result.LatestVersion} available";
                UpdateBadge.Visibility = Visibility.Visible;

                if (_vm.Settings.ShowToastNotifications)
                    ToastHelper.Show("Update available",
                        $"CodeShellManager v{result.LatestVersion} is ready to download.",
                        _vm.Settings.ShowNotificationSound);
            });
        }
        catch { /* never surface update errors to the user */ }
    }

    private void UpdateBadge_Click(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        if (_updateReleaseUrl == null) return;
        System.Diagnostics.Process.Start(
            new System.Diagnostics.ProcessStartInfo(_updateReleaseUrl) { UseShellExecute = true });
    }

    private void UpdateBadgeDismiss_Click(object sender, RoutedEventArgs e)
    {
        UpdateBadge.Visibility = Visibility.Collapsed;
    }

    // ── PowerShell quick-launch ───────────────────────────────────────────────

    private void LaunchPowerShellInFolder(string workingFolder, string groupId)
    {
        // Prefer PowerShell 7+ (pwsh); fall back to Windows PowerShell 5.1
        string cmd = ExistsOnPath("pwsh") ? "pwsh" : "powershell";
        string folderName = string.IsNullOrEmpty(workingFolder)
            ? "PS"
            : System.IO.Path.GetFileName(workingFolder.TrimEnd('/', '\\')) + " (PS)";

        var session = _sessionManager.CreateSession(folderName, workingFolder, cmd, "", groupId);
        SeedRunCommandsAsync(session);
        _ = LaunchSessionAsync(session);
    }

    /// <summary>
    /// WSL counterpart of <see cref="LaunchPowerShellInFolder"/>: spawns a bare bash
    /// session inside the same distro + Linux folder as <paramref name="parent"/>.
    /// Used by the "Open WSL console here" context-menu item.
    /// </summary>
    private void LaunchWslConsoleFromSession(Models.ShellSession parent)
    {
        if (!parent.IsWsl) return;
        string leaf = string.IsNullOrEmpty(parent.WslWorkingFolder)
            ? parent.WslDistro
            : System.IO.Path.GetFileName(parent.WslWorkingFolder.TrimEnd('/'));
        string name = string.IsNullOrEmpty(leaf) ? "bash" : $"{leaf} (bash)";

        var session = _sessionManager.CreateSession(name, parent.WorkingFolder, "bash", "", parent.GroupId);
        session.Kind = Models.SessionKind.Wsl;
        session.WslDistro = parent.WslDistro;
        session.WslUser = parent.WslUser;
        session.WslWorkingFolder = parent.WslWorkingFolder;
        _ = LaunchSessionAsync(session);
    }

    private static bool ExistsOnPath(string executable)
    {
        try
        {
            using var p = System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "where.exe",
                Arguments = executable,
                RedirectStandardOutput = true,
                UseShellExecute = false,
                CreateNoWindow = true
            });
            p?.WaitForExit(2000);
            return p?.ExitCode == 0;
        }
        catch { return false; }
    }

    // ── Settings ──────────────────────────────────────────────────────────────

    private void SettingsButton_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new SettingsWindow(_vm.Settings, _searchService) { Owner = this };
        if (dialog.ShowDialog() == true)
        {
            var edited = dialog.EditedSettings;
            _vm.Settings.AutoRestoreSessions = edited.AutoRestoreSessions;
            _vm.Settings.AutoResumeClaude = edited.AutoResumeClaude;
            _vm.Settings.ClaudeLaunchStaggerMs = edited.ClaudeLaunchStaggerMs;
            _vm.Settings.AutoFocusTerminalOnSelect = edited.AutoFocusTerminalOnSelect;
            _vm.Settings.ShowToastNotifications = edited.ShowToastNotifications;
            _vm.Settings.ShowNotificationSound = edited.ShowNotificationSound;
            _vm.Settings.AnthropicApiKey = edited.AnthropicApiKey;
            _vm.Settings.DefaultCommand = edited.DefaultCommand;
            _vm.Settings.DefaultWorkingFolder = edited.DefaultWorkingFolder;
            _vm.Settings.ShowGitBranch = edited.ShowGitBranch;
            _vm.Settings.ShowGroupsTab = edited.ShowGroupsTab;
            _vm.Settings.GroupDisplayMode = edited.GroupDisplayMode;
            _vm.Settings.FilterGridByActiveGroup = edited.FilterGridByActiveGroup;
            _vm.Settings.PerGroupLayout = edited.PerGroupLayout;
            _vm.Settings.SidebarActionIconsMode = edited.SidebarActionIconsMode;
            _vm.Settings.ShowWorktreeClusters = edited.ShowWorktreeClusters;
            _vm.Settings.SearchCollapseAfterNavigate = edited.SearchCollapseAfterNavigate;

            // ActiveGroupId only makes sense in FilterStrip mode. Reset it so InlineHeaders
            // and None modes start unfiltered (all groups visible / no filter applied).
            if (_vm.Settings.GroupDisplayMode != Models.GroupDisplayMode.FilterStrip)
                _vm.ActiveGroupId = null;
            _vm.Settings.MaxSearchResults = edited.MaxSearchResults;
            _vm.Settings.ShowTerminalStatusDot = edited.ShowTerminalStatusDot;
            _vm.Settings.ImportWindowsTerminalProfiles = edited.ImportWindowsTerminalProfiles;
            _vm.Settings.LaunchCommands = edited.LaunchCommands;
            _vm.Settings.TerminalFontFamily = edited.TerminalFontFamily;
            _vm.Settings.TerminalFontSize = edited.TerminalFontSize;
            _vm.Settings.TerminalFontLigatures = edited.TerminalFontLigatures;
            _vm.Settings.TerminalFontWeight = edited.TerminalFontWeight;
            _vm.Settings.TerminalLetterSpacing = edited.TerminalLetterSpacing;
            _vm.Settings.TerminalLineHeight = edited.TerminalLineHeight;
            _vm.Settings.DebugTerminalTrace = edited.DebugTerminalTrace;
            _ = _vm.SaveStateAsync();

            // Push font settings to all active terminal sessions
            foreach (var vm in _vm.Sessions)
                vm.Bridge?.ApplyFontSettings(_vm.Settings);

            // Push the action-icons mode to every live sidebar row. Hovered state is
            // recomputed via IsMouseOver so the change is visible immediately without
            // requiring the cursor to leave + re-enter the row.
            foreach (var (id, panel) in _sidebarActionPanels)
            {
                if (_sessionUi.TryGetValue(id, out var ui))
                    ApplyActionIconsMode(panel, ui.sidebarItem,
                        _vm.Settings.SidebarActionIconsMode,
                        isHovered: ui.sidebarItem.IsMouseOver);
            }

            UpdateGroupStripVisibility();
            RebuildSidebarOrder();  // also re-runs RefreshTerminalLayout — picks up FilterGridByActiveGroup
        }
    }

    private void AboutButton_Click(object sender, RoutedEventArgs e)
    {
        new AboutDialog { Owner = this }.ShowDialog();
    }

    private async void Export_Click(object sender, RoutedEventArgs e)
    {
        await _vm.SaveStateAsync();

        var dlg = new Microsoft.Win32.SaveFileDialog
        {
            Title = "Export CodeShellManager Setup",
            Filter = "CodeShellManager backup (*.csm)|*.csm",
            DefaultExt = ".csm",
            FileName = $"CodeShellManager-backup-{DateTime.Now:yyyy-MM-dd}"
        };

        if (dlg.ShowDialog(this) != true) return;

        try
        {
            await ImportExportService.ExportAsync(_vm.CurrentState, dlg.FileName);
            MessageBox.Show(
                $"Setup exported successfully to:\n{dlg.FileName}",
                "Export Complete", MessageBoxButton.OK, MessageBoxImage.Information);
        }
        catch (Exception ex)
        {
            MessageBox.Show(
                $"Export failed: {ex.Message}",
                "Export Error", MessageBoxButton.OK, MessageBoxImage.Error);
        }
    }

    private async void Import_Click(object sender, RoutedEventArgs e)
    {
        var dlg = new Microsoft.Win32.OpenFileDialog
        {
            Title = "Import CodeShellManager Setup",
            Filter = "CodeShellManager backup (*.csm)|*.csm",
            DefaultExt = ".csm"
        };

        if (dlg.ShowDialog(this) != true) return;

        var confirm = MessageBox.Show(
            "Importing will replace your current settings and session list.\n\n" +
            "The app will need to restart to apply the imported sessions.\n\n" +
            "Continue?",
            "Import Setup", MessageBoxButton.YesNo, MessageBoxImage.Warning);

        if (confirm != MessageBoxResult.Yes) return;

        AppState? imported;
        try
        {
            imported = await ImportExportService.ImportAsync(dlg.FileName);
        }
        catch (Exception ex)
        {
            MessageBox.Show(
                $"Import failed: {ex.Message}",
                "Import Error", MessageBoxButton.OK, MessageBoxImage.Error);
            return;
        }

        if (imported == null)
        {
            MessageBox.Show(
                "The selected file could not be read as a valid CodeShellManager backup.",
                "Import Error", MessageBoxButton.OK, MessageBoxImage.Error);
            return;
        }

        // Apply settings immediately
        var s = imported.Settings;
        _vm.Settings.AutoRestoreSessions = s.AutoRestoreSessions;
        _vm.Settings.AutoResumeClaude = s.AutoResumeClaude;
        _vm.Settings.ShowToastNotifications = s.ShowToastNotifications;
        _vm.Settings.ShowNotificationSound = s.ShowNotificationSound;
        _vm.Settings.AnthropicApiKey = s.AnthropicApiKey;
        _vm.Settings.DefaultCommand = s.DefaultCommand;
        _vm.Settings.DefaultWorkingFolder = s.DefaultWorkingFolder;
        _vm.Settings.ShowGitBranch = s.ShowGitBranch;
        _vm.Settings.SearchCollapseAfterNavigate = s.SearchCollapseAfterNavigate;
        _vm.Settings.MaxSearchResults = s.MaxSearchResults;
        _vm.Settings.ShowTerminalStatusDot = s.ShowTerminalStatusDot;
        _vm.Settings.LaunchCommands = s.LaunchCommands;
        _vm.Settings.TerminalFontFamily = s.TerminalFontFamily;
        _vm.Settings.TerminalFontSize = s.TerminalFontSize;
        _vm.Settings.TerminalFontLigatures = s.TerminalFontLigatures;
        _vm.Settings.TerminalFontWeight = s.TerminalFontWeight;
        _vm.Settings.TerminalLetterSpacing = s.TerminalLetterSpacing;
        _vm.Settings.TerminalLineHeight = s.TerminalLineHeight;

        // Replace session and group lists in the imported state and save
        imported.Settings = _vm.Settings;
        await ImportExportService.ExportAsync(imported, GetStatePath());

        MessageBox.Show(
            "Setup imported successfully.\n\nPlease restart CodeShellManager to apply the imported sessions and groups.",
            "Import Complete", MessageBoxButton.OK, MessageBoxImage.Information);
    }

    private static string GetStatePath() => StateService.GetPath();

    // ── Keyboard shortcuts ────────────────────────────────────────────────────

    private void OnKeyDown(object s, WpfKeyEventArgs e)
    {
        if (TryHandleGlobalShortcut(e.Key, Keyboard.Modifiers))
            e.Handled = true;
    }

    // Invoked by TerminalBridge when a WebView2 accelerator key fires — routes
    // the same shortcuts that OnKeyDown handles, so they work even when a
    // terminal has focus (WebView2 otherwise swallows its own keys).
    private void OnBridgeAcceleratorKey(object? sender, WpfKeyEventArgs e)
    {
        if (TryHandleGlobalShortcut(e.Key, Keyboard.Modifiers))
            e.Handled = true;
    }

    private bool TryHandleGlobalShortcut(Key key, ModifierKeys mods)
    {
        if (key == Key.T && mods == ModifierKeys.Control) { OpenNewSessionDialog(); return true; }
        // Browser convention: Ctrl+Shift+T reopens the most-recently-closed session.
        // Duplicate moved to Ctrl+Alt+T to free this slot.
        if (key == Key.T && mods == (ModifierKeys.Control | ModifierKeys.Shift))
        {
            // Peek instead of pop — if the reopen fails, the entry stays available
            // for retry. PeekMostRecentlyClosed returns the same entry PopMostRecently
            // would have popped; ReopenAndRemoveOnSuccessAsync removes only on success.
            var entry = _vm.PeekMostRecentlyClosed();
            if (entry != null) _ = ReopenAndRemoveOnSuccessAsync(entry);
            return true;
        }
        if (key == Key.T && mods == (ModifierKeys.Control | ModifierKeys.Alt))
        {
            if (_vm.ActiveSession != null) _ = DuplicateSessionAsync(_vm.ActiveSession);
            return true;
        }
        if (key == Key.W && mods == ModifierKeys.Control) { _vm.ActiveSession?.CloseCommand.Execute(null); return true; }
        if (key == Key.F && mods == ModifierKeys.Control) { ToggleSearch_Click(this, new RoutedEventArgs()); return true; }
        if (key == Key.Tab && mods == ModifierKeys.Control) { CycleSession(forward: true); return true; }
        if (key == Key.Tab && mods == (ModifierKeys.Control | ModifierKeys.Shift)) { CycleSession(forward: false); return true; }
        if (key == Key.F5 && mods == ModifierKeys.None)
        {
            if (_vm.ActiveSession != null) RunDefaultCommand(_vm.ActiveSession);
            return true;
        }
        if (key == Key.F5 && mods == ModifierKeys.Shift)
        {
            if (_vm.ActiveSession is { } vm)
            {
                var def = vm.Session.RunCommands.FirstOrDefault(i => i.IsDefault);
                if (def != null) vm.Runner.Stop(def.Id);
            }
            return true;
        }
        return false;
    }

    // Refocus the last-active terminal when the window regains focus (e.g. Alt+Tab).
    // Without this, focus lands on whichever WPF control happened to hold it last —
    // typically not the WebView2, so keystrokes go nowhere.
    private void OnWindowActivated(object? sender, EventArgs e)
    {
        _vm.ActiveSession?.Bridge?.FocusTerminal();
    }

    private void CycleSession(bool forward)
    {
        if (_vm.Sessions.Count < 2) return;
        int current = _vm.ActiveSession == null ? -1
            : _vm.Sessions.IndexOf(_vm.ActiveSession);
        int next = forward
            ? (current + 1) % _vm.Sessions.Count
            : (current - 1 + _vm.Sessions.Count) % _vm.Sessions.Count;
        _vm.FocusSessionCommand.Execute(_vm.Sessions[next]);
        UpdateSidebarActiveState();
        if (_currentLayout == LayoutMode.Single) UpdateLayout();
    }

    // ── Window close ──────────────────────────────────────────────────────────

    protected override async void OnClosing(System.ComponentModel.CancelEventArgs e)
    {
        // Final entry: cleanup is finished, let WPF tear the window down for real.
        if (_shutdownComplete)
        {
            base.OnClosing(e);
            return;
        }

        // WPF won't wait for async work, so cancel this close. The reclose at the bottom
        // re-enters through the _shutdownComplete branch above.
        e.Cancel = true;

        // Re-entry during async cleanup (e.g. user double-clicks the X) — just suppress.
        if (_isShuttingDown) return;
        _isShuttingDown = true;

        // Show the shutdown overlay so the user sees progress while sessions tear down.
        // The yield lets WPF render the overlay before the synchronous disposal below blocks
        // the UI thread; without it, the overlay would only paint after Close() is reached.
        ShutdownOverlay.Visibility = Visibility.Visible;
        await Dispatcher.InvokeAsync(() => { },
            System.Windows.Threading.DispatcherPriority.Background);

        _windowStateTimer.Stop();
        if (_windowStateReady)
            _vm.UpdateWindowState(WindowState, Left, Top, Width, Height);
        await _vm.SaveStateAsync();

        var all = _vm.Sessions.ToList();

        // Non-Claude sessions don't fight over ~/.claude.json — dispose them in parallel.
        foreach (var vm in all)
        {
            if (!ClaudeSessionService.IsClaudeCommand(vm.Command))
                vm.Dispose();
        }

        // Claude rewrites ~/.claude.json on exit without locking, so two claude.exe
        // processes flushing simultaneously can corrupt it. Dispose claude sessions one
        // at a time, waiting for each process to *actually exit* before starting the next
        // — a fixed time stagger isn't safe because claude's shutdown can take longer
        // than the configured delay on slow disks. Cap each wait at 10s so a stuck claude
        // doesn't hang application shutdown.
        int postExitMs = _vm.Settings.ClaudeLaunchStaggerMs;
        foreach (var vm in all)
        {
            if (!ClaudeSessionService.IsClaudeCommand(vm.Command)) continue;
            await DisposeAndWaitForExitAsync(vm, timeoutMs: 10000);
            // Small post-exit pause as belt-and-braces in case ~/.claude.json's write
            // continues after the parent's shutdown signal but before its handles close.
            if (postExitMs > 0) await Task.Delay(Math.Min(postExitMs, 1000));
        }

        // OutputIndexer.Dispose now drains its worker first, but SqliteConnection.Close
        // has been observed to throw NRE internally on shutdown — swallow + log so it
        // doesn't escape as an unhandled exception during application exit.
        try { _db?.Close(); }
        catch (Exception ex) { Log($"OnClosing _db.Close threw: {ex}"); }
        try { _db?.Dispose(); }
        catch (Exception ex) { Log($"OnClosing _db.Dispose threw: {ex}"); }
        App.TrayIcon?.Dispose();

        _shutdownComplete = true;
        // Queue Close() on the dispatcher rather than calling it inline. If none of
        // the awaits above actually yielded (e.g. --clean mode with no sessions —
        // SaveStateAsync short-circuits, and the foreach loops over an empty list
        // never await), control never returns to WPF between e.Cancel=true and this
        // point, so WPF's internal _isClosing flag is still set and Close() throws
        // "Cannot ... call Close ... while a Window is closing." Posting via
        // BeginInvoke lets OnClosing return first, WPF resets _isClosing, then the
        // queued Close() re-enters cleanly through the _shutdownComplete branch.
        _ = Dispatcher.BeginInvoke(new System.Action(Close));
    }

    /// <summary>
    /// Signals the session's PTY to shut down and waits for its child process to actually
    /// exit (or <paramref name="timeoutMs"/> ms, whichever comes first), then fully
    /// disposes the VM. Used for claude sessions on app close so consecutive
    /// <c>~/.claude.json</c> writes can't overlap.
    /// </summary>
    private static async Task DisposeAndWaitForExitAsync(SessionViewModel vm, int timeoutMs)
    {
        var pty = vm.Pty;
        if (pty == null || !pty.IsRunning)
        {
            vm.Dispose();
            return;
        }

        var tcs = new TaskCompletionSource();
        void OnExit() => tcs.TrySetResult();
        pty.Exited += OnExit;
        try
        {
            // Dispose triggers ClosePseudoConsole, which signals the child to shut down.
            // MonitorExitAsync (already running) will fire Exited once the process exits.
            vm.Dispose();
            await Task.WhenAny(tcs.Task, Task.Delay(timeoutMs));
        }
        finally
        {
            pty.Exited -= OnExit;
        }
    }
}

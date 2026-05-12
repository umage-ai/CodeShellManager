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
    // Anchor for shift-click range selection in the sidebar.
    private string? _selectionAnchorId;
    // Group-tab notification indicators (badge + text), keyed by group id (or "__ALL__"
    // / GroupFilter.Ungrouped sentinels). Repopulated on every RebuildGroupStrip.
    private readonly Dictionary<string, (Border badge, TextBlock badgeText)> _groupTabIndicators = [];
    private SqliteConnection? _db;
    private SearchService? _searchService;
    private LayoutMode _currentLayout = LayoutMode.Single;
    private int _layoutViewportOffset = 0;

    // Window state debounce
    private readonly System.Windows.Threading.DispatcherTimer _windowStateTimer;
    private bool _windowStateReady = false; // don't save before state is loaded

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
            // --clean: skip restore and leave state.json untouched. Drop the
            // saved-session list from the in-memory SessionManager so any new
            // work this run doesn't co-mingle with the persisted set.
            foreach (var s in saved)
                _sessionManager.RemoveSession(s.Id);
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
            // Launch live sessions first, then append dormant entries — keeps the
            // "dormant always at the bottom" invariant that SleepSession and
            // RebuildSidebarOrder enforce at runtime.
            // Stagger consecutive claude launches: claude's CLI does an unlocked
            // read-modify-write on ~/.claude.json at startup, so simultaneous
            // boots can corrupt the user's profile.
            bool lastWasClaude = false;
            foreach (var s in saved)
            {
                if (s.IsDormant) continue;
                bool isClaude = ClaudeSessionService.IsClaudeCommand(s.Command);
                if (isClaude && lastWasClaude)
                    await Task.Delay(2000);
                try { await LaunchSessionAsync(s, restoring: true); }
                catch (Exception ex)
                {
                    Log($"Restore FAILED for '{s.Name}': {ex}");
                    MessageBox.Show($"Failed to restore '{s.Name}': {ex.Message}",
                        "Restore Error", MessageBoxButton.OK, MessageBoxImage.Warning);
                }
                lastWasClaude = isClaude;
            }
            foreach (var s in saved)
            {
                if (s.IsDormant) AddDormantSidebarItem(s);
            }
        }
        else
        {
            foreach (var s in saved)
                _sessionManager.RemoveSession(s.Id);
            await _vm.SaveStateAsync();
        }
    }

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
            defaultArgs: parent?.Session.Args)
        {
            Owner = this
        };

        if (dialog.ShowDialog() != true) return;

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
            session.IsRemote = true;
            session.SshUser = dialog.SshUser;
            session.SshHost = dialog.SshHost;
            session.SshPort = dialog.SshPort;
            session.SshRemoteFolder = dialog.SshRemoteFolder;
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
        string anchorId = primary.Id;
        bool lastWasClaude = ClaudeSessionService.IsClaudeCommand(primary.Command);
        foreach (var path in additionalPaths)
        {
            if (!System.IO.Directory.Exists(path)) continue;
            bool isClaude = ClaudeSessionService.IsClaudeCommand(primary.Command);
            if (isClaude && lastWasClaude) await Task.Delay(2000);
            var sibling = _sessionManager.CreateSession(
                System.IO.Path.GetFileName(path.TrimEnd('/', '\\')) ?? primary.Command,
                path,
                primary.Command,
                primary.Args,
                string.IsNullOrEmpty(primary.GroupId) ? null : primary.GroupId,
                colorOverride: null,
                afterSessionId: anchorId);
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
        if (p.IsRemote)
        {
            clone.IsRemote = true;
            clone.SshUser = p.SshUser;
            clone.SshHost = p.SshHost;
            clone.SshPort = p.SshPort;
            clone.SshRemoteFolder = p.SshRemoteFolder;
        }
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
        if (session.IsRemote) return;
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

        // Create bridge and initialize
        var bridge = new TerminalBridge(webView);
        vm.Bridge = bridge;
        bridge.AcceleratorKeyPressed += OnBridgeAcceleratorKey;

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

        if (session.IsRemote)
        {
            effectiveCommand = "ssh";
            effectiveArgs = session.BuildSshArgs();
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
            return;
        }

        Log($"terminalWrapper visible, TerminalGrid children={TerminalGrid.Children.Count}");
        terminalWrapper.Visibility = Visibility.Visible;

        // Build sidebar entry
        var sidebarItem = BuildSidebarItem(vm);
        _sessionUi[session.Id] = (webView, terminalWrapper, sidebarItem);

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

        // Action buttons
        var btnPanel = new StackPanel
        {
            Orientation = Orientation.Vertical,
            Margin = new Thickness(0, 4, 4, 4),
            VerticalAlignment = VerticalAlignment.Center
        };

        var exploreBtn = MakeMiniButton("📁", "Open in Explorer", () => vm.OpenInExplorerCommand.Execute(null));
        var psBtn      = MakeMiniButton(">_", "Open PowerShell here", () => LaunchPowerShellInFolder(vm.WorkingFolder, vm.GroupId));
        var spawnBtn   = MakeMiniButton("➕", "New session here (inherits group + profile)",
                            () => OpenNewSessionDialogFromParent(vm));
        var renameBtn  = MakeMiniButton("✏", "Rename session", StartRename);
        var sleepBtn   = MakeMiniButton("💤", "Sleep session (keep it but stop the terminal)", () => SleepSession(vm));
        var closeBtn   = MakeMiniButton("✕", "Close session", () => vm.CloseCommand.Execute(null));

        btnPanel.Children.Add(exploreBtn);
        btnPanel.Children.Add(psBtn);
        btnPanel.Children.Add(spawnBtn);
        btnPanel.Children.Add(renameBtn);
        btnPanel.Children.Add(sleepBtn);
        btnPanel.Children.Add(closeBtn);

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
        container.MouseEnter += (_, _) =>
        {
            if (vm.Id == _vm.ActiveSession?.Id) return;
            if (_vm.IsSelected(vm.Id)) return;
            container.Background = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44));
        };
        container.MouseLeave += (_, _) =>
        {
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
        GroupStripPanel.Children.Add(BuildGroupTab(GroupFilter.Ungrouped, "Ungrouped", "·"));
        foreach (var g in _sessionManager.Groups.OrderBy(g => g.SortOrder))
            GroupStripPanel.Children.Add(BuildGroupTab(g.Id, g.Name, GroupInitials(g.Name)));

        // Footer "+" tab to add a new group inline.
        var addBtn = new Border
        {
            Margin = new Thickness(4, 8, 4, 4),
            Background = Brushes.Transparent,
            BorderBrush = new SolidColorBrush(Color.FromRgb(0x45, 0x47, 0x5a)),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(4),
            Cursor = System.Windows.Input.Cursors.Hand,
            ToolTip = "New group"
        };
        addBtn.Child = new TextBlock
        {
            Text = "+",
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xe3, 0xa1)),
            FontSize = 16,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 4, 0, 4)
        };
        addBtn.MouseEnter += (_, _) =>
            addBtn.Background = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44));
        addBtn.MouseLeave += (_, _) => addBtn.Background = Brushes.Transparent;
        addBtn.MouseLeftButtonDown += (_, _) => PromptCreateGroup();
        GroupStripPanel.Children.Add(addBtn);

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
            menu.Items.Add(new System.Windows.Controls.Separator());

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
            menu.Items.Add(new System.Windows.Controls.Separator());
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

        if (inlineMode)
        {
            // Ungrouped section first (only shown when it has members or there are groups).
            var ungrouped = _vm.Sessions
                .Where(s => string.IsNullOrEmpty(s.GroupId) && _sessionUi.ContainsKey(s.Id))
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
                var members = _vm.Sessions
                    .Where(s => s.GroupId == g.Id && _sessionUi.ContainsKey(s.Id))
                    .ToList();
                SidebarSessionList.Children.Add(BuildInlineGroupHeader(g, members.Count, g.IsExpanded));
                if (g.IsExpanded) AppendSessionsWithClusters(members);
            }
        }
        else
        {
            // Flat list mode (None or FilterStrip).
            var visibleSessions = new List<SessionViewModel>();
            foreach (var vm in _vm.Sessions)
            {
                if (mode == Models.GroupDisplayMode.FilterStrip && !_vm.SessionMatchesActiveGroup(vm))
                    continue;
                if (_sessionUi.ContainsKey(vm.Id)) visibleSessions.Add(vm);
            }
            AppendSessionsWithClusters(visibleSessions);
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
    /// worktree cluster headers above runs of 2+ adjacent siblings (when enabled).
    /// </summary>
    private void AppendSessionsWithClusters(List<SessionViewModel> sessions)
    {
        var clusters = ComputeWorktreeClusters(sessions);
        int clusterIdx = 0;
        for (int i = 0; i < sessions.Count; i++)
        {
            var vm = sessions[i];
            if (clusterIdx < clusters.Count && clusters[clusterIdx].start == i)
            {
                var (s, e, root) = clusters[clusterIdx];
                int count = e - s + 1;
                SidebarSessionList.Children.Add(BuildWorktreeClusterHeader(root, count, vm.AccentColor));
                clusterIdx++;
            }
            SidebarSessionList.Children.Add(_sessionUi[vm.Id].sidebarItem);
        }
    }

    /// <summary>
    /// Returns the ranges of <paramref name="visible"/> that should render under a worktree
    /// cluster header — runs of 2+ adjacent sessions sharing a RepoRoot. Empty when
    /// the setting is off.
    /// </summary>
    private List<(int start, int end, string repoRoot)> ComputeWorktreeClusters(
        IReadOnlyList<SessionViewModel> visible)
    {
        var clusters = new List<(int, int, string)>();
        if (!_vm.Settings.ShowWorktreeClusters) return clusters;

        int runStart = -1;
        string? runRoot = null;
        for (int i = 0; i < visible.Count; i++)
        {
            string? root = visible[i].RepoRoot;
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

        var sessions = _vm.Sessions.ToList();
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

            default: // Single
            {
                var target = _vm.ActiveSession ?? sessions.FirstOrDefault();
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

        DockPanel.SetDock(termStatusDot, Dock.Right);
        DockPanel.SetDock(explorerBtn, Dock.Right);
        DockPanel.SetDock(toolbarPsBtn, Dock.Right);
        DockPanel.SetDock(notesBtn, Dock.Right);
        DockPanel.SetDock(sleepBtn, Dock.Right);
        DockPanel.SetDock(chevronBtn, Dock.Right);
        DockPanel.SetDock(playBtn, Dock.Right);
        DockPanel.SetDock(claudeBadge, Dock.Left);
        DockPanel.SetDock(titleBlock, Dock.Left);
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
            ? (session.IsRemote
                ? (string.IsNullOrWhiteSpace(session.SshHost) ? session.Command : session.SshHost)
                : System.IO.Path.GetFileName(session.WorkingFolder.TrimEnd('/', '\\')) ?? session.Command)
            : session.Name;

        var nameText = new TextBlock
        {
            Text = displayName,
            Foreground = new SolidColorBrush(Color.FromRgb(0x93, 0x99, 0xb2)),
            FontSize = 13,
            FontStyle = FontStyles.Italic,
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        string folderShort = session.IsRemote
            ? (string.IsNullOrWhiteSpace(session.SshHost) ? "" : session.SshHost)
            : (string.IsNullOrEmpty(session.WorkingFolder)
                ? ""
                : new System.IO.DirectoryInfo(session.WorkingFolder).Name);

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
        s.ColorOverride ?? ColorService.GetHexColor(
            s.IsRemote
                ? (string.IsNullOrWhiteSpace(s.SshUser) ? s.SshHost : $"{s.SshUser}@{s.SshHost}")
                : s.WorkingFolder);

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
            _vm.Settings.AutoFocusTerminalOnSelect = edited.AutoFocusTerminalOnSelect;
            _vm.Settings.ShowToastNotifications = edited.ShowToastNotifications;
            _vm.Settings.ShowNotificationSound = edited.ShowNotificationSound;
            _vm.Settings.AnthropicApiKey = edited.AnthropicApiKey;
            _vm.Settings.DefaultCommand = edited.DefaultCommand;
            _vm.Settings.DefaultWorkingFolder = edited.DefaultWorkingFolder;
            _vm.Settings.ShowGitBranch = edited.ShowGitBranch;
            _vm.Settings.ShowGroupsTab = edited.ShowGroupsTab;
            _vm.Settings.GroupDisplayMode = edited.GroupDisplayMode;
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
            _ = _vm.SaveStateAsync();

            // Push font settings to all active terminal sessions
            foreach (var vm in _vm.Sessions)
                vm.Bridge?.ApplyFontSettings(_vm.Settings);

            UpdateGroupStripVisibility();
            RebuildSidebarOrder();
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
        if (key == Key.T && mods == (ModifierKeys.Control | ModifierKeys.Shift))
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
        _windowStateTimer.Stop();
        if (_windowStateReady)
            _vm.UpdateWindowState(WindowState, Left, Top, Width, Height);
        await _vm.SaveStateAsync();
        foreach (var vm in _vm.Sessions.ToList())
            vm.Dispose();
        _db?.Close();
        _db?.Dispose();
        App.TrayIcon?.Dispose();
        base.OnClosing(e);
    }
}

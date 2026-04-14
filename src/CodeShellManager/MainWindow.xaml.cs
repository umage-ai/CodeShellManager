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

    private SqliteConnection? _db;
    private SearchService? _searchService;
    private LayoutMode _currentLayout = LayoutMode.Single;

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
        };

        Loaded += OnLoaded;
        KeyDown += OnKeyDown;

        BuildShortcutPanel();
    }

    // ── Startup ───────────────────────────────────────────────────────────────

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        await InitDatabaseAsync();
        await _vm.LoadStateAsync();
        _ = CheckForUpdatesAsync();   // fire-and-forget; never blocks startup

        var saved = _sessionManager.Sessions.ToList();
        Log($"OnLoaded: {saved.Count} saved sessions, AutoRestore={_vm.Settings.AutoRestoreSessions}");
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
            foreach (var s in saved)
            {
                try { await LaunchSessionAsync(s, restoring: true); }
                catch (Exception ex)
                {
                    Log($"Restore FAILED for '{s.Name}': {ex}");
                    MessageBox.Show($"Failed to restore '{s.Name}': {ex.Message}",
                        "Restore Error", MessageBoxButton.OK, MessageBoxImage.Warning);
                }
            }
        }
        else
        {
            foreach (var s in saved)
                _sessionManager.RemoveSession(s.Id);
            await _vm.SaveStateAsync();
        }
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

    private void OpenNewSessionDialog(string defaultFolder = "")
    {
        var dialog = new NewSessionDialog(
            _sessionManager.Groups,
            string.IsNullOrEmpty(defaultFolder) ? _vm.Settings.DefaultWorkingFolder : defaultFolder,
            _vm.Settings.LaunchCommands)
        {
            Owner = this
        };

        if (dialog.ShowDialog() != true) return;

        var session = _sessionManager.CreateSession(
            dialog.SessionName,
            dialog.SelectedFolder,
            dialog.SelectedCommand,
            dialog.SelectedArgs,
            dialog.SelectedGroupId);

        _ = LaunchSessionAsync(session);
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

        // Create WebView2
        var webView = new WebView2
        {
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Stretch,
            DefaultBackgroundColor = System.Drawing.Color.FromArgb(30, 30, 46)
        };

        // Build the persistent wrapper NOW (webView has no parent yet — this is safe).
        var terminalWrapper = BuildTerminalWrapper(vm, webView);
        terminalWrapper.Visibility = Visibility.Collapsed;
        TerminalGrid.Children.Add(terminalWrapper);   // in tree → WebView2 can init

        // Create bridge and initialize
        var bridge = new TerminalBridge(webView);
        vm.Bridge = bridge;

        // Wire output indexer and alert detector
        if (_db != null)
        {
            var indexer = new OutputIndexer(_db, session.Id, vm.DisplayName);
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
        string htmlPath = new Uri(Path.Combine(assetsDir, "terminal.html")).AbsoluteUri;

        await bridge.InitializeAsync(htmlPath);

        // Start PTY now that bridge is ready
        var pty = new PseudoTerminal();
        vm.Pty = pty;
        pty.Exited += () =>
        {
            if (_searchService != null)
                _ = _searchService.RecordSessionHistoryAsync(
                    session.Id, session.Name, session.WorkingFolder,
                    session.Command, session.Args, session.GroupId);
            Dispatcher.Invoke(() =>
            {
                _sessionManager.UpdateStatus(session.Id, SessionStatus.Exited);
                RefreshSidebarItem(session.Id);
            });
        };

        string workDir = Directory.Exists(session.WorkingFolder)
            ? session.WorkingFolder
            : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);

        // Auto-resume Claude Code sessions when restoring — only if a prior session JSONL exists.
        // If no session ID is found we start fresh (no --continue fallback which exits immediately
        // when there are no sessions to resume).
        string effectiveArgs = session.Args;
        if (restoring && ClaudeSessionService.IsClaudeCommand(session.Command)
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

        Log($"Starting PTY: workDir='{workDir}'");
        try
        {
            var (cols, rows) = bridge.TerminalSize;
            pty.Start(session.Command, effectiveArgs, workDir, cols, rows);
            Log("PTY started OK");
            bridge.AttachPty(pty);
            _sessionManager.UpdateStatus(session.Id, SessionStatus.Running);
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
        SidebarSessionList.Children.Add(sidebarItem);
        _sessionUi[session.Id] = (webView, terminalWrapper, sidebarItem);

        _vm.RegisterSession(vm);
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

        var container = new Border
        {
            Margin = new Thickness(0, 2, 0, 2),
            Background = Brushes.Transparent,
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
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 10,
            Margin = new Thickness(0, 1, 0, 0),
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        // Git branch indicator
        var gitText = new TextBlock
        {
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 10,
            Margin = new Thickness(0, 2, 0, 0),
            TextTrimming = TextTrimming.CharacterEllipsis,
            Visibility = Visibility.Collapsed
        };

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
        var renameBtn  = MakeMiniButton("✏", "Rename session", StartRename);
        var closeBtn   = MakeMiniButton("✕", "Close session", () => vm.CloseCommand.Execute(null));

        btnPanel.Children.Add(exploreBtn);
        btnPanel.Children.Add(psBtn);
        btnPanel.Children.Add(renameBtn);
        btnPanel.Children.Add(closeBtn);

        Grid.SetColumn(textPanel, 1);
        Grid.SetColumn(btnPanel, 3);
        inner.Children.Add(stripe);
        inner.Children.Add(textPanel);
        inner.Children.Add(statusDot);
        inner.Children.Add(btnPanel);

        container.Child = inner;

        // Click to activate
        container.MouseLeftButtonDown += (_, _) =>
        {
            _vm.FocusSessionCommand.Execute(vm);
            UpdateSidebarActiveState();
        };

        // Hover effect
        container.MouseEnter += (_, _) =>
        {
            if (vm.Id != _vm.ActiveSession?.Id)
                container.Background = new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44));
        };
        container.MouseLeave += (_, _) =>
        {
            if (vm.Id != _vm.ActiveSession?.Id)
                container.Background = Brushes.Transparent;
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
                        break;

                    case nameof(SessionViewModel.GitBranch):
                    case nameof(SessionViewModel.GitIsDirty):
                    case nameof(SessionViewModel.GitInfoLoaded):
                        UpdateGitText(gitText, vm);
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
            bool isActive = id == _vm.ActiveSession?.Id;
            item.Background = isActive
                ? new SolidColorBrush(Color.FromRgb(0x31, 0x32, 0x44))
                : Brushes.Transparent;
        }
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
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                PlaceTerminal(sessions, 0, 0, 0);
                PlaceTerminal(sessions, 1, 0, 1);
                break;

            case LayoutMode.ThreeColumn:
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                PlaceTerminal(sessions, 0, 0, 0);
                PlaceTerminal(sessions, 1, 0, 1);
                PlaceTerminal(sessions, 2, 0, 2);
                break;

            case LayoutMode.TwoByTwo:
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                PlaceTerminal(sessions, 0, 0, 0);
                PlaceTerminal(sessions, 1, 0, 1);
                PlaceTerminal(sessions, 2, 1, 0);
                PlaceTerminal(sessions, 3, 1, 1);
                break;

            case LayoutMode.TwoRow:
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                PlaceTerminal(sessions, 0, 0, 0);
                PlaceTerminal(sessions, 1, 1, 0);
                break;

            case LayoutMode.FourColumn:
                for (int i = 0; i < 4; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                for (int i = 0; i < 4; i++) PlaceTerminal(sessions, i, 0, i);
                break;

            case LayoutMode.SixColumn:
                for (int i = 0; i < 6; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                for (int i = 0; i < 6; i++) PlaceTerminal(sessions, i, 0, i);
                break;

            case LayoutMode.SixByTwo:
                for (int i = 0; i < 6; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                TerminalGrid.RowDefinitions.Add(new RowDefinition());
                for (int i = 0; i < 12; i++) PlaceTerminal(sessions, i, i / 6, i % 6);
                break;

            case LayoutMode.SixByThree:
                for (int i = 0; i < 6; i++) TerminalGrid.ColumnDefinitions.Add(new ColumnDefinition());
                for (int r = 0; r < 3; r++) TerminalGrid.RowDefinitions.Add(new RowDefinition());
                for (int i = 0; i < 18; i++) PlaceTerminal(sessions, i, i / 6, i % 6);
                break;

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

        DockPanel.SetDock(termStatusDot, Dock.Right);
        DockPanel.SetDock(explorerBtn, Dock.Right);
        DockPanel.SetDock(toolbarPsBtn, Dock.Right);
        DockPanel.SetDock(notesBtn, Dock.Right);
        DockPanel.SetDock(claudeBadge, Dock.Left);
        DockPanel.SetDock(titleBlock, Dock.Left);
        toolbarContent.Children.Add(termStatusDot);
        toolbarContent.Children.Add(explorerBtn);
        toolbarContent.Children.Add(toolbarPsBtn);
        toolbarContent.Children.Add(notesBtn);
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

        DockPanel.SetDock(toolbar, Dock.Top);
        DockPanel.SetDock(notesPanel, Dock.Top);
        outer.Children.Add(toolbar);
        outer.Children.Add(notesPanel);
        outer.Children.Add(webView);
        wrapper.Child = outer;

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

        return wrapper;
    }

    // ── Session close ─────────────────────────────────────────────────────────

    private void OnSessionVmClosed(SessionViewModel vm)
    {
        if (_sessionUi.TryGetValue(vm.Id, out var ui))
        {
            SidebarSessionList.Children.Remove(ui.sidebarItem);
            _sessionUi.Remove(vm.Id);
        }
        _sessionManager.RemoveSession(vm.Id);
        RefreshTerminalLayout();
        UpdateAlertBadge();

        if (_vm.Sessions.Count == 0)
            EmptyState.Visibility = Visibility.Visible;
    }

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
                        $"CodeShellManager v{result.LatestVersion} is ready to download.");
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
        var dialog = new SettingsWindow(_vm.Settings) { Owner = this };
        if (dialog.ShowDialog() == true)
        {
            var edited = dialog.EditedSettings;
            _vm.Settings.AutoRestoreSessions = edited.AutoRestoreSessions;
            _vm.Settings.ShowToastNotifications = edited.ShowToastNotifications;
            _vm.Settings.AnthropicApiKey = edited.AnthropicApiKey;
            _vm.Settings.DefaultCommand = edited.DefaultCommand;
            _vm.Settings.DefaultWorkingFolder = edited.DefaultWorkingFolder;
            _vm.Settings.ShowGitBranch = edited.ShowGitBranch;
            _vm.Settings.SearchCollapseAfterNavigate = edited.SearchCollapseAfterNavigate;
            _vm.Settings.MaxSearchResults = edited.MaxSearchResults;
            _vm.Settings.ShowTerminalStatusDot = edited.ShowTerminalStatusDot;
            _vm.Settings.LaunchCommands = edited.LaunchCommands;
            _ = _vm.SaveStateAsync();
        }
    }

    private void AboutButton_Click(object sender, RoutedEventArgs e)
    {
        new AboutDialog { Owner = this }.ShowDialog();
    }

    // ── Keyboard shortcuts ────────────────────────────────────────────────────

    private void OnKeyDown(object s, WpfKeyEventArgs e)
    {
        if (e.Key == Key.T && Keyboard.Modifiers == ModifierKeys.Control)
        {
            OpenNewSessionDialog();
            e.Handled = true;
        }
        else if (e.Key == Key.W && Keyboard.Modifiers == ModifierKeys.Control)
        {
            _vm.ActiveSession?.CloseCommand.Execute(null);
            e.Handled = true;
        }
        else if (e.Key == Key.F && Keyboard.Modifiers == ModifierKeys.Control)
        {
            ToggleSearch_Click(s, new RoutedEventArgs());
            e.Handled = true;
        }
        else if (e.Key == Key.Tab && Keyboard.Modifiers == ModifierKeys.Control)
        {
            CycleSession(forward: true);
            e.Handled = true;
        }
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
        await _vm.SaveStateAsync();
        foreach (var vm in _vm.Sessions.ToList())
            vm.Dispose();
        _db?.Close();
        _db?.Dispose();
        App.TrayIcon?.Dispose();
        base.OnClosing(e);
    }
}

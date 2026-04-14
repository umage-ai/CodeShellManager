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

namespace CodeShellManager;

public partial class MainWindow : Window
{
    private readonly SessionManager _sessionManager = new();
    private readonly StateService _stateService = new();
    private readonly MainViewModel _vm;
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
            // Sessions are already in _sessionManager from LoadFromState.
            // LaunchSessionAsync will create VMs + PTYs; it must NOT call
            // _sessionManager.CreateSession again (which would duplicate them).
            // We pass the existing ShellSession objects directly.
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
        var dialog = new NewSessionDialog(_sessionManager.Groups,
            string.IsNullOrEmpty(defaultFolder) ? _vm.Settings.DefaultWorkingFolder : defaultFolder)
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
        // The wrapper owns the webView in its visual tree.
        // We keep it hidden and add it to TerminalGrid so WebView2 can initialize.
        var terminalWrapper = BuildTerminalWrapper(vm, webView);
        terminalWrapper.Visibility = Visibility.Collapsed;
        TerminalGrid.Children.Add(terminalWrapper);   // in tree → WebView2 can init

        // Create bridge and initialize (EnsureCoreWebView2Async + Navigate)
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
        pty.Exited += () => Dispatcher.Invoke(() =>
        {
            _sessionManager.UpdateStatus(session.Id, SessionStatus.Exited);
            RefreshSidebarItem(session.Id);
        });

        string workDir = Directory.Exists(session.WorkingFolder)
            ? session.WorkingFolder
            : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);

        Log($"Starting PTY: workDir='{workDir}'");
        try
        {
            var (cols, rows) = bridge.TerminalSize;
            pty.Start(session.Command, session.Args, workDir, cols, rows);
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
        bridge.FitTerminal();   // force xterm.js to resize after becoming visible
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
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(4) });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        inner.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        // Accent stripe
        var stripe = new Border
        {
            Background = accentBrush,
            CornerRadius = new CornerRadius(3, 0, 0, 3),
            Width = 4
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
            TextTrimming = TextTrimming.CharacterEllipsis
        };

        var folderText = new TextBlock
        {
            Text = vm.FolderShort,
            Foreground = new SolidColorBrush(Color.FromRgb(0x6c, 0x70, 0x86)),
            FontSize = 10,
            Margin = new Thickness(0, 1, 0, 0),
            TextTrimming = TextTrimming.CharacterEllipsis
        };

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
        textPanel.Children.Add(folderText);
        textPanel.Children.Add(alertBadge);

        // Action buttons
        var btnPanel = new StackPanel
        {
            Orientation = Orientation.Vertical,
            Margin = new Thickness(0, 4, 4, 4),
            VerticalAlignment = VerticalAlignment.Center
        };

        var exploreBtn = MakeMiniButton("📁", "Open in Explorer", () => vm.OpenInExplorerCommand.Execute(null));
        var closeBtn = MakeMiniButton("✕", "Close session", () => vm.CloseCommand.Execute(null));

        btnPanel.Children.Add(exploreBtn);
        btnPanel.Children.Add(closeBtn);

        Grid.SetColumn(textPanel, 1);
        Grid.SetColumn(btnPanel, 2);
        inner.Children.Add(stripe);
        inner.Children.Add(textPanel);
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

        // Subscribe to alert changes
        vm.PropertyChanged += (_, args) =>
        {
            if (args.PropertyName == nameof(SessionViewModel.NeedsAttention))
            {
                Dispatcher.Invoke(() =>
                {
                    alertBadge.Visibility = vm.NeedsAttention ? Visibility.Visible : Visibility.Collapsed;
                    UpdateAlertBadge();
                });
            }
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
            BorderThickness = new Thickness(0, 2, 0, 0),
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

        DockPanel.SetDock(explorerBtn, Dock.Right);
        DockPanel.SetDock(titleBlock, Dock.Left);
        toolbarContent.Children.Add(explorerBtn);
        toolbarContent.Children.Add(titleBlock);
        toolbarContent.Children.Add(folderBlock);
        toolbar.Child = toolbarContent;

        DockPanel.SetDock(toolbar, Dock.Top);
        outer.Children.Add(toolbar);
        outer.Children.Add(webView);

        wrapper.Child = outer;
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
        if (show) SearchBox.Focus();
    }

    private void SearchBox_KeyDown(object s, WpfKeyEventArgs e)
    {
        if (e.Key == Key.Enter) DoSearch();
    }

    private void Search_Click(object s, RoutedEventArgs e) => DoSearch();

    private async void DoSearch()
    {
        if (_searchService == null) return;
        var results = await _searchService.SearchAsync(SearchBox.Text);
        SearchResults.ItemsSource = results;
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

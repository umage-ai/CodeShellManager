using System;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using CodeShellManager.Models;
using CodeShellManager.Services;
using CodeShellManager.ViewModels;
using Color = System.Windows.Media.Color;
using ColorConverter = System.Windows.Media.ColorConverter;
using ContextMenu = System.Windows.Controls.ContextMenu;
using HorizontalAlignment = System.Windows.HorizontalAlignment;
using MenuItem = System.Windows.Controls.MenuItem;
using Orientation = System.Windows.Controls.Orientation;
using VerticalAlignment = System.Windows.VerticalAlignment;

namespace CodeShellManager;

/// <summary>
/// "🕘 Recent" on the main toolbar (and Ctrl+Shift+E): jump to one of the sessions used
/// most recently — live or sleeping — whatever group tab or layout page it is on.
/// Ordering and wording live in the WPF-free <see cref="RecentSessions"/>.
/// </summary>
public partial class MainWindow
{
    private void RecentSessions_Click(object sender, RoutedEventArgs e)
        => ShowRecentSessionsMenu(sender as FrameworkElement ?? RecentSessionsBtn);

    private void ShowRecentSessionsMenu(FrameworkElement anchor)
    {
        var menu = new ContextMenu
        {
            PlacementTarget = anchor,
            Placement = System.Windows.Controls.Primitives.PlacementMode.Bottom,
        };

        var live = _vm.Sessions.ToDictionary(v => v.Id);
        var picked = RecentSessions.Pick(_sessionManager.Sessions, _vm.ActiveSession?.Id);
        var now = DateTime.UtcNow;

        if (picked.Count == 0)
        {
            menu.Items.Add(new MenuItem
            {
                Header = _sessionManager.Sessions.Count == 0 ? "No sessions yet" : "No other sessions",
                IsEnabled = false,
            });
        }

        for (int i = 0; i < picked.Count; i++)
        {
            var session = picked[i];
            live.TryGetValue(session.Id, out var vm);
            var item = new MenuItem
            {
                Header = BuildRecentHeader(session, vm, now),
                // 1–9 and 0 as access keys, so the menu works from the keyboard alone.
                InputGestureText = i < 10 ? ((i + 1) % 10).ToString() : "",
                ToolTip = string.IsNullOrEmpty(session.WorkingFolder) ? null : session.WorkingFolder,
            };
            // The header is a Grid, so UI Automation would otherwise read the item as unnamed.
            System.Windows.Automation.AutomationProperties.SetName(item, RecentItemName(session, vm));
            item.Click += async (_, _) =>
            {
                if (vm != null) JumpToSession(vm);
                else if (session.IsDormant) await WakeSessionAsync(session);
            };
            menu.Items.Add(item);
        }

        // Digits pick an item directly (shown as each item's gesture text).
        menu.PreviewKeyDown += (_, ke) =>
        {
            int digit = ke.Key switch
            {
                >= System.Windows.Input.Key.D0 and <= System.Windows.Input.Key.D9 => ke.Key - System.Windows.Input.Key.D0,
                >= System.Windows.Input.Key.NumPad0 and <= System.Windows.Input.Key.NumPad9 => ke.Key - System.Windows.Input.Key.NumPad0,
                _ => -1,
            };
            if (digit < 0) return;
            int idx = digit == 0 ? 9 : digit - 1;
            if (idx < picked.Count && menu.Items[idx] is MenuItem mi && mi.IsEnabled)
            {
                ke.Handled = true;
                menu.IsOpen = false;
                mi.RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
            }
        };

        // Keyboard users land on the first item, so Ctrl+Shift+E, Enter flips back to the
        // previous session.
        menu.Opened += (_, _) =>
        {
            if (menu.Items.Count > 0 && menu.Items[0] is MenuItem first)
                first.Focus();
        };
        menu.IsOpen = true;
    }

    /// <summary>
    /// Focuses a live session and makes sure it is on screen: in filter-strip mode a session
    /// outside the selected group tab would otherwise be focused but invisible, so the tab
    /// switches to the session's own group.
    /// </summary>
    private void JumpToSession(SessionViewModel vm)
    {
        if (_vm.Settings.GroupDisplayMode == GroupDisplayMode.FilterStrip
            && _vm.ActiveGroupId is { } groupFilter)
        {
            bool inFilter = groupFilter == GroupFilter.Ungrouped
                ? string.IsNullOrEmpty(vm.Session.GroupId)
                : vm.Session.GroupId == groupFilter;
            if (!inFilter)
                _vm.ActiveGroupId = string.IsNullOrEmpty(vm.Session.GroupId)
                    ? GroupFilter.Ungrouped
                    : vm.Session.GroupId;
        }

        _vm.FocusSessionCommand.Execute(vm);
        UpdateSidebarActiveState();
        RefreshTerminalLayout();
    }

    private static readonly SolidColorBrush RecentMutedBrush = Freeze(Color.FromRgb(0x6c, 0x70, 0x86));
    private static readonly SolidColorBrush RecentWorkingBrush = Freeze(Color.FromRgb(0x89, 0xb4, 0xfa));
    private static readonly SolidColorBrush RecentApprovalBrush = Freeze(Color.FromRgb(0xfa, 0xb3, 0x87));
    private static readonly SolidColorBrush RecentInputBrush = Freeze(Color.FromRgb(0xa6, 0xe3, 0xa1));
    private static readonly SolidColorBrush RecentAlertBrush = Freeze(Color.FromRgb(0xf3, 0x8b, 0xa8));

    private static SolidColorBrush Freeze(Color c)
    {
        var b = new SolidColorBrush(c);
        b.Freeze();
        return b;
    }

    private static string RecentItemName(ShellSession session, SessionViewModel? vm)
        => vm?.DisplayName
           ?? (string.IsNullOrWhiteSpace(session.Name) ? session.DefaultDisplayName : session.Name);

    private FrameworkElement BuildRecentHeader(ShellSession session, SessionViewModel? vm, DateTime nowUtc)
    {
        var grid = new Grid { MinWidth = 320 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });                       // dot
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) }); // name + folder
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });                       // status
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });                       // ago

        Color accent;
        try { accent = (Color)ColorConverter.ConvertFromString(vm?.AccentColor ?? GetAccentForSession(session)); }
        catch { accent = Color.FromRgb(0x6c, 0x70, 0x86); }
        var dot = new System.Windows.Shapes.Ellipse
        {
            Width = 8,
            Height = 8,
            Fill = new SolidColorBrush(accent),
            Opacity = vm == null ? 0.55 : 1.0,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(0, 0, 8, 0),
        };
        Grid.SetColumn(dot, 0);

        string name = RecentItemName(session, vm);
        var text = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(new TextBlock
        {
            Text = name,
            FontWeight = FontWeights.SemiBold,
            MaxWidth = 260,
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        if (!string.IsNullOrEmpty(session.FolderShort) && session.FolderShort != name)
            text.Children.Add(new TextBlock
            {
                Text = "  " + session.FolderShort,
                Foreground = RecentMutedBrush,
                MaxWidth = 200,
                TextTrimming = TextTrimming.CharacterEllipsis,
            });
        Grid.SetColumn(text, 1);

        var (glyph, brush) = vm switch
        {
            null when session.IsDormant => ("💤", RecentMutedBrush),
            { IsWorking: true } => ("⟳", RecentWorkingBrush),
            { IsWaitingForApproval: true } => ("●", RecentApprovalBrush),
            { IsWaitingForInput: true } => ("●", RecentInputBrush),
            { NeedsAttention: true } => ("!", RecentAlertBrush),
            _ => ("", RecentMutedBrush),
        };
        var status = new TextBlock
        {
            Text = glyph,
            Foreground = brush,
            Margin = new Thickness(10, 0, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(status, 2);

        var ago = new TextBlock
        {
            Text = RecentSessions.Ago(session.LastActivityAt, nowUtc),
            Foreground = RecentMutedBrush,
            FontFamily = new System.Windows.Media.FontFamily("Consolas"),
            FontSize = 10.5,
            MinWidth = 28,
            TextAlignment = TextAlignment.Right,
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(10, 0, 0, 0),
        };
        Grid.SetColumn(ago, 3);

        grid.Children.Add(dot);
        grid.Children.Add(text);
        grid.Children.Add(status);
        grid.Children.Add(ago);
        return grid;
    }
}

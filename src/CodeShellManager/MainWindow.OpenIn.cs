using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using CodeShellManager.Models;
using CodeShellManager.Services;
using CodeShellManager.ViewModels;
using WpfButton = System.Windows.Controls.Button;
using Brushes = System.Windows.Media.Brushes;
using Color = System.Windows.Media.Color;
using FontFamily = System.Windows.Media.FontFamily;
using Orientation = System.Windows.Controls.Orientation;
using MenuItem = System.Windows.Controls.MenuItem;
using ContextMenu = System.Windows.Controls.ContextMenu;
using Separator = System.Windows.Controls.Separator;

namespace CodeShellManager;

/// <summary>
/// "Open with…" (issue #178) — the session's folder in an editor / IDE from a user-owned
/// list (<see cref="AppSettings.OpenWith"/>). On the terminal toolbar as <c>{ }</c> + ▼ beside
/// 📁 / >_ (the ▶/▼ run-button pattern: the button opens the default, the chevron lists every
/// entry for this session's kind), and as "Open with ▸" in the sidebar right-click menu.
/// All template, validation and launch rules live in the WPF-free
/// <see cref="OpenWithService"/>; this file is only the UI.
/// </summary>
public partial class MainWindow
{
    /// <summary>Per-session toolbar controls, so a list change can repaint every pane. Pruned on refresh.</summary>
    private readonly Dictionary<string, (StackPanel host, WpfButton btn, System.ComponentModel.PropertyChangedEventHandler onChanged, SessionViewModel vm)> _openWithControls = new();

    private bool _openWithSeeding;

    /// <summary>
    /// Seeds <see cref="AppSettings.OpenWith"/> by auto-detection, once per profile. Detection
    /// is disk + registry I/O and a vswhere spawn, so it runs on the pool; only the hand-back
    /// touches the settings, on the UI thread.
    /// </summary>
    private void EnsureOpenWithSeeded()
    {
        if (_vm.Settings.OpenWithSeeded || _openWithSeeding) return;
        _openWithSeeding = true;
        _ = Task.Run(OpenWithDetector.Detect).ContinueWith(t =>
        {
            Dispatcher.BeginInvoke(() =>
            {
                _openWithSeeding = false;
                // The user may have edited (or imported) the list while detection ran — theirs wins.
                if (_vm.Settings.OpenWithSeeded) return;
                if (t.IsCompletedSuccessfully && t.Result.Count == 0)
                {
                    // Nothing found: stay unseeded, so installing VS Code later still makes
                    // { } appear on the next launch — the pre-#178 zero-config behaviour.
                    OpenWithService.Log("detection found no editors; will try again next launch");
                }
                else if (t.IsCompletedSuccessfully)
                {
                    _vm.Settings.OpenWith = t.Result;
                    _vm.Settings.OpenWithSeeded = true;
                    OpenWithService.Log($"seeded {t.Result.Count} entr{(t.Result.Count == 1 ? "y" : "ies")}: " +
                                        string.Join(", ", t.Result.Select(e => $"{e.Label} ({e.Executable})")));
                    _ = _vm.SaveStateAsync();
                }
                else
                {
                    OpenWithService.Log($"detection failed: {t.Exception?.GetBaseException().Message}");
                }
                RefreshOpenWithButtons();
            });
        }, TaskScheduler.Default);
    }

    /// <summary>
    /// The toolbar <c>{ }</c> + ▼ pair, in one panel so it docks as a unit. Hidden when no
    /// entry applies to this session's kind (or the session has nothing to open).
    /// </summary>
    private StackPanel BuildOpenWithButtons(SessionViewModel vm)
    {
        var btn = new WpfButton
        {
            Content = "{ }",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 11,
            FontFamily = new FontFamily("Consolas"),
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(5, 2, 2, 2),
        };
        btn.Click += (_, _) =>
        {
            var entry = OpenWithService.DefaultFor(_vm.Settings.OpenWith, vm.Session.Kind);
            if (entry != null) OpenSessionWith(vm, entry);
        };
        btn.MouseRightButtonUp += (_, e) => { OpenOpenWithEditor(); e.Handled = true; };
        // Computed when shown, from the live session — so it can't go stale after an edit,
        // and it spells out the exact command rather than trusting the entry's label.
        btn.ToolTip = "";
        btn.ToolTipOpening += (_, _) =>
        {
            var entry = OpenWithService.DefaultFor(_vm.Settings.OpenWith, vm.Session.Kind);
            btn.ToolTip = entry == null
                ? "Open with…"
                : $"Open folder in {entry.Label}\n{OpenWithService.Describe(entry, OpenWithContext.FromSession(vm.Session))}\n(right-click: edit list)";
        };

        var chevron = new WpfButton
        {
            Content = "▼",
            ToolTip = "Open with…",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 9,
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(2, 2, 4, 2),
        };
        chevron.Click += (_, _) => ShowOpenWithDropdown(vm, chevron);

        var host = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Margin = new Thickness(0, 0, 4, 0),
            VerticalAlignment = VerticalAlignment.Center,
        };
        host.Children.Add(btn);
        host.Children.Add(chevron);

        // A pane rebuild for the same session replaces the old controls.
        RemoveOpenWithControls(vm.Id);
        // Kind / folder / host can change under a live pane (Edit session, a WSL $HOME landing),
        // which is when the button may need to appear or disappear.
        System.ComponentModel.PropertyChangedEventHandler onChanged = (_, e) =>
        {
            if (string.IsNullOrEmpty(e.PropertyName)
                || e.PropertyName is nameof(SessionViewModel.WorkingFolder) or nameof(SessionViewModel.DisplayName))
                Dispatcher.BeginInvoke(() => ApplyOpenWithButtonState(vm, host, btn));
        };
        vm.PropertyChanged += onChanged;
        _openWithControls[vm.Id] = (host, btn, onChanged, vm);
        ApplyOpenWithButtonState(vm, host, btn);
        return host;
    }

    /// <summary>Drops a pane's Open-with controls — on close, sleep, restart and failed launch.</summary>
    private void RemoveOpenWithControls(string sessionId)
    {
        if (!_openWithControls.Remove(sessionId, out var c)) return;
        c.vm.PropertyChanged -= c.onChanged;
    }

    private void ApplyOpenWithButtonState(SessionViewModel vm, StackPanel host, WpfButton btn)
    {
        var entry = OpenWithContext.FromSession(vm.Session).HasTarget
            ? OpenWithService.DefaultFor(_vm.Settings.OpenWith, vm.Session.Kind)
            : null;
        var vis = entry == null ? Visibility.Collapsed : Visibility.Visible;
        if (host.Visibility != vis) host.Visibility = vis;
    }

    /// <summary>Repaints every pane's <c>{ }</c> after the list changed (seeded, edited, imported).</summary>
    private void RefreshOpenWithButtons()
    {
        foreach (var id in _openWithControls.Keys.ToList())
        {
            // No pruning here: a pane mid-launch is registered before its VM joins
            // _vm.Sessions. Entries are removed at the teardown sites (RemoveOpenWithControls).
            var c = _openWithControls[id];
            ApplyOpenWithButtonState(c.vm, c.host, c.btn);
        }
    }

    private void ShowOpenWithDropdown(SessionViewModel vm, WpfButton anchor)
    {
        var menu = new ContextMenu
        {
            PlacementTarget = anchor,
            Placement = System.Windows.Controls.Primitives.PlacementMode.Bottom,
        };
        foreach (var item in BuildOpenWithItems(vm)) menu.Items.Add(item);
        menu.IsOpen = true;
    }

    /// <summary>"Open with ▸" for the sidebar right-click menu. Null when the session has nothing to open.</summary>
    private MenuItem? BuildOpenWithSubmenu(SessionViewModel vm)
    {
        if (!OpenWithContext.FromSession(vm.Session).HasTarget) return null;
        var sub = new MenuItem { Header = "Open with" };
        foreach (var item in BuildOpenWithItems(vm)) sub.Items.Add(item);
        return sub;
    }

    /// <summary>
    /// One item per entry that has a template for this session's kind (the rest are hidden),
    /// then "Edit…". The tooltip carries the program path, so what a click runs is visible.
    /// </summary>
    private List<object> BuildOpenWithItems(SessionViewModel vm)
    {
        var items = new List<object>();
        var ctx = OpenWithContext.FromSession(vm.Session);
        foreach (var entry in OpenWithService.ApplicableTo(_vm.Settings.OpenWith, vm.Session.Kind))
        {
            // The program's file name is in the header and the full resolved command is the
            // tooltip, so an entry labelled "VS Code" can't quietly be something else.
            string exeName = System.IO.Path.GetFileName(entry.Executable);
            var mi = new MenuItem
            {
                Header = (entry.IsDefault ? $"{entry.Label} (default)" : entry.Label) + $"   — {exeName}",
                ToolTip = OpenWithService.Describe(entry, ctx),
            };
            mi.Click += (_, _) => OpenSessionWith(vm, entry);
            items.Add(mi);
        }
        if (items.Count == 0)
            items.Add(new MenuItem
            {
                Header = $"No editor is set up for {OpenWithService.KindName(vm.Session.Kind)} sessions",
                IsEnabled = false,
            });
        items.Add(new Separator());
        var edit = new MenuItem { Header = "Edit…" };
        edit.Click += (_, _) => OpenOpenWithEditor();
        items.Add(edit);
        return items;
    }

    /// <summary>
    /// Snapshots the session on the UI thread, then hands everything else — the folder check,
    /// a WSL $HOME probe, Process.Start — to <see cref="OpenWithService.LaunchAsync"/> on the pool.
    /// </summary>
    private void OpenSessionWith(SessionViewModel vm, OpenWithEntry entry)
    {
        var ctx = OpenWithContext.FromSession(vm.Session);
        var snapshot = entry.Clone();
        _ = OpenWithService.LaunchAsync(snapshot, ctx).ContinueWith(t =>
        {
            string? error = t.IsCompletedSuccessfully ? t.Result : t.Exception?.GetBaseException().Message;
            if (error == null) return;
            OpenWithService.Log($"'{snapshot.Label}' for session '{vm.DisplayName}': {error}");
            _ = Dispatcher.BeginInvoke(() => ToastHelper.Show($"Couldn't open in {snapshot.Label}", error));
        }, TaskScheduler.Default);
    }

    /// <summary>The list editor, reached from the ▼ menu, a right-click on <c>{ }</c>, and the sidebar submenu.</summary>
    private void OpenOpenWithEditor()
    {
        var dlg = new Views.OpenWithDialog(_vm.Settings.OpenWith) { Owner = this };
        if (dlg.ShowDialog() == true && dlg.Result != null)
        {
            _vm.Settings.OpenWith = dlg.Result;
            _vm.Settings.OpenWithSeeded = true;
            _ = _vm.SaveStateAsync();
            RefreshOpenWithButtons();
        }
    }
}

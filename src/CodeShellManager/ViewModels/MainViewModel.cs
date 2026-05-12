using System;
using System.Collections.ObjectModel;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CodeShellManager.Models;
using CodeShellManager.Services;

namespace CodeShellManager.ViewModels;

public enum LayoutMode { Single, TwoColumn, ThreeColumn, TwoByTwo, TwoRow, FourColumn, SixColumn, SixByTwo, SixByThree }

/// <summary>Sentinel <see cref="MainViewModel.ActiveGroupId"/> value meaning "show only sessions with no group".</summary>
public static class GroupFilter
{
    public const string Ungrouped = "__UNGROUPED__";
}

public partial class MainViewModel : ObservableObject
{
    private readonly SessionManager _sessionManager;
    private readonly StateService _stateService;
    private AppState _appState = new();

    public ObservableCollection<SessionViewModel> Sessions { get; } = [];

    [ObservableProperty] private SessionViewModel? _activeSession;
    [ObservableProperty] private LayoutMode _layout = LayoutMode.Single;
    [ObservableProperty] private bool _showSearch;
    [ObservableProperty] private bool _showCommandHelper;
    [ObservableProperty] private string _searchQuery = "";

    /// <summary>
    /// Null = show all sessions (no group filter active). <see cref="GroupFilter.Ungrouped"/>
    /// = only sessions with no GroupId. Any other value = a specific group's Id.
    /// </summary>
    [ObservableProperty] private string? _activeGroupId;

    /// <summary>IDs of sessions currently in the multi-select set (in addition to ActiveSession).</summary>
    public HashSet<string> SelectedSessionIds { get; } = new();

    public int AlertCount => Sessions.Count(s => s.NeedsAttention);

    public event Action<SessionViewModel>? SessionClosed;
    public event Action? GroupsChanged;
    public event Action? SelectionChanged;

    public SessionManager SessionManager => _sessionManager;

    public IReadOnlyList<Models.SessionGroup> Groups => _sessionManager.Groups;

    public MainViewModel(SessionManager sessionManager, StateService stateService)
    {
        _sessionManager = sessionManager;
        _stateService = stateService;
        _sessionManager.GroupsChanged += () =>
            App.Current.Dispatcher.Invoke(() => GroupsChanged?.Invoke());
    }

    public Models.SessionGroup CreateGroup(string name)
    {
        var g = _sessionManager.AddGroup(name);
        _ = SaveStateAsync();
        return g;
    }

    public void RenameGroup(string groupId, string newName)
    {
        _sessionManager.RenameGroup(groupId, newName);
        _ = SaveStateAsync();
    }

    public void RemoveGroup(string groupId)
    {
        _sessionManager.RemoveGroup(groupId);
        if (ActiveGroupId == groupId) ActiveGroupId = null;
        _ = SaveStateAsync();
    }

    /// <summary>Returns true when the session matches the current group filter.</summary>
    public bool SessionMatchesActiveGroup(SessionViewModel vm)
    {
        if (ActiveGroupId == null) return true;
        if (ActiveGroupId == GroupFilter.Ungrouped) return string.IsNullOrEmpty(vm.GroupId);
        return vm.GroupId == ActiveGroupId;
    }

    public bool IsSelected(string sessionId) => SelectedSessionIds.Contains(sessionId);

    public void ClearSelection()
    {
        if (SelectedSessionIds.Count == 0) return;
        SelectedSessionIds.Clear();
        SelectionChanged?.Invoke();
    }

    public void ToggleSelection(string sessionId)
    {
        if (!SelectedSessionIds.Add(sessionId))
            SelectedSessionIds.Remove(sessionId);
        SelectionChanged?.Invoke();
    }

    /// <summary>Selects every session in the visible list between anchor and target (inclusive).</summary>
    public void SetRangeSelection(IReadOnlyList<string> visibleIdsInOrder, string? anchorId, string targetId)
    {
        SelectedSessionIds.Clear();
        if (visibleIdsInOrder.Count == 0) return;
        int targetIdx = IndexOf(visibleIdsInOrder, targetId);
        if (targetIdx < 0) return;
        int anchorIdx = anchorId != null ? IndexOf(visibleIdsInOrder, anchorId) : -1;
        if (anchorIdx < 0) anchorIdx = targetIdx;
        int lo = Math.Min(anchorIdx, targetIdx);
        int hi = Math.Max(anchorIdx, targetIdx);
        for (int i = lo; i <= hi; i++)
            SelectedSessionIds.Add(visibleIdsInOrder[i]);
        SelectionChanged?.Invoke();
    }

    /// <summary>
    /// Returns the IDs of all sessions to act on for a multi-target action:
    /// the current selection if non-empty, otherwise just the explicit target.
    /// </summary>
    public IReadOnlyList<string> ResolveActionTargets(string targetSessionId)
    {
        if (SelectedSessionIds.Count > 0)
            return SelectedSessionIds.Contains(targetSessionId)
                ? SelectedSessionIds.ToArray()
                : new[] { targetSessionId };
        return new[] { targetSessionId };
    }

    public void AssignSessionsToGroup(IEnumerable<string> sessionIds, string? groupId)
    {
        foreach (var id in sessionIds)
            _sessionManager.SetSessionGroup(id, groupId);
        _ = SaveStateAsync();
    }

    private static int IndexOf(IReadOnlyList<string> list, string value)
    {
        for (int i = 0; i < list.Count; i++)
            if (list[i] == value) return i;
        return -1;
    }

    public async Task LoadStateAsync()
    {
        _appState = await _stateService.LoadAsync();
        _sessionManager.LoadFromState(_appState);
        Layout = Enum.TryParse<LayoutMode>(_appState.LastLayout, out var lm) ? lm : LayoutMode.Single;
    }

    public async Task SaveStateAsync()
    {
        // In --clean mode, never write state.json so the user's prior session list
        // survives the debug run untouched. Settings/window/layout changes from a
        // clean run are also discarded — that's the point of "clean".
        if (App.CleanStart) return;

        _sessionManager.PopulateState(_appState);
        _appState.LastLayout = Layout.ToString();
        await _stateService.SaveAsync(_appState);
    }

    public AppSettings Settings => _appState.Settings;

    /// <summary>Returns the current app state (after SaveStateAsync has been called to flush session data).</summary>
    public AppState CurrentState => _appState;

    /// <summary>Saves window position/size. Only updates NormalBounds when not maximized.</summary>
    public void UpdateWindowState(System.Windows.WindowState windowState, double left, double top, double width, double height)
    {
        _appState.WindowMaximized = windowState == System.Windows.WindowState.Maximized;
        if (windowState == System.Windows.WindowState.Normal)
        {
            _appState.LastNormalBounds = new Models.WindowBounds
            {
                Left = left,
                Top = top,
                Width = width,
                Height = height
            };
        }
    }

    public Models.WindowBounds? GetSavedWindowBounds() => _appState.LastNormalBounds;

    public bool IsWindowMaximized() => _appState.WindowMaximized;

    // Called by the View after it has set up the WebView2 + bridge for a session
    public void RegisterSession(SessionViewModel vm)
    {
        vm.CloseRequested += OnSessionCloseRequested;

        if (vm.Bridge != null)
        {
            vm.Bridge.UserInput += () =>
            {
                vm.AlertDetector?.NotifyUserInteracted();
                App.Current.Dispatcher.Invoke(() => OnPropertyChanged(nameof(AlertCount)));
            };
        }

        if (vm.AlertDetector != null)
        {
            vm.AlertDetector.AlertRaised += alert =>
            {
                App.Current.Dispatcher.Invoke(() =>
                {
                    vm.RaiseAlert(alert.Message, alert.Type);
                    OnPropertyChanged(nameof(AlertCount));
                    if (Settings.ShowToastNotifications)
                        ToastHelper.Show(vm.DisplayName, alert.Message, Settings.ShowNotificationSound);
                });
            };
            vm.AlertDetector.AlertCleared += _ =>
            {
                App.Current.Dispatcher.Invoke(() =>
                {
                    vm.ClearAlert();
                    OnPropertyChanged(nameof(AlertCount));
                });
            };
        }

        // Mirror SessionManager order so insert-after-parent (CreateSession with afterSessionId)
        // also lands the VM at the matching slot — otherwise the model order would be correct
        // but the sidebar would still show the new entry at the bottom.
        int idx = -1;
        for (int i = 0; i < _sessionManager.Sessions.Count; i++)
            if (_sessionManager.Sessions[i].Id == vm.Id) { idx = i; break; }
        if (idx >= 0 && idx <= Sessions.Count) Sessions.Insert(idx, vm);
        else Sessions.Add(vm);
        ActiveSession = vm;
        _ = SaveStateAsync();
    }

    private void OnSessionCloseRequested(SessionViewModel vm)
    {
        vm.CloseRequested -= OnSessionCloseRequested;
        Sessions.Remove(vm);
        if (SelectedSessionIds.Remove(vm.Id))
            SelectionChanged?.Invoke();

        if (ActiveSession == vm)
            ActiveSession = Sessions.LastOrDefault();

        SessionClosed?.Invoke(vm);
        vm.Dispose();
        OnPropertyChanged(nameof(AlertCount));
        _ = SaveStateAsync();
    }

    [RelayCommand]
    private void SetLayout(LayoutMode mode)
    {
        Layout = mode;
        _ = SaveStateAsync();
    }

    [RelayCommand]
    private void ToggleSearch() => ShowSearch = !ShowSearch;

    [RelayCommand]
    private void ToggleCommandHelper() => ShowCommandHelper = !ShowCommandHelper;

    [RelayCommand]
    private void FocusSession(SessionViewModel vm)
    {
        ActiveSession = vm;
        vm.ClearAlert();
        if (Settings.AutoFocusTerminalOnSelect)
            vm.Bridge?.FocusTerminal();
        vm.AlertDetector?.NotifyUserInteracted();
        OnPropertyChanged(nameof(AlertCount));
    }

    [RelayCommand]
    private void SendCommandToActive(string text)
    {
        ActiveSession?.Bridge?.SendToTerminal(text + "\r");
        ActiveSession?.AlertDetector?.NotifyUserInteracted();
    }

    /// <summary>Returns sessions assigned to layout slots 0..3</summary>
    public SessionViewModel? GetSlotSession(int slot) =>
        slot < Sessions.Count ? Sessions[slot] : null;

    public void MoveSession(string sessionId, int newIndex)
    {
        _sessionManager.MoveSession(sessionId, newIndex);
        var vm = Sessions.FirstOrDefault(s => s.Id == sessionId);
        if (vm == null) return;
        int cur = Sessions.IndexOf(vm);
        newIndex = Math.Clamp(newIndex, 0, Sessions.Count - 1);
        if (cur != newIndex) Sessions.Move(cur, newIndex);
        _ = SaveStateAsync();
    }
}

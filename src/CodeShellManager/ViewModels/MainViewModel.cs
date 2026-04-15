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

    public int AlertCount => Sessions.Count(s => s.NeedsAttention);

    public event Action<SessionViewModel>? SessionClosed;

    public MainViewModel(SessionManager sessionManager, StateService stateService)
    {
        _sessionManager = sessionManager;
        _stateService = stateService;
    }

    public async Task LoadStateAsync()
    {
        _appState = await _stateService.LoadAsync();
        _sessionManager.LoadFromState(_appState);
        Layout = Enum.TryParse<LayoutMode>(_appState.LastLayout, out var lm) ? lm : LayoutMode.Single;
    }

    public async Task SaveStateAsync()
    {
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

        if (vm.AlertDetector != null)
        {
            vm.AlertDetector.AlertRaised += alert =>
            {
                App.Current.Dispatcher.Invoke(() =>
                {
                    vm.RaiseAlert(alert.Message, alert.Type);
                    OnPropertyChanged(nameof(AlertCount));
                    if (Settings.ShowToastNotifications)
                        ToastHelper.Show(vm.DisplayName, alert.Message);
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

        Sessions.Add(vm);
        ActiveSession = vm;
        _ = SaveStateAsync();
    }

    private void OnSessionCloseRequested(SessionViewModel vm)
    {
        vm.CloseRequested -= OnSessionCloseRequested;
        Sessions.Remove(vm);

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

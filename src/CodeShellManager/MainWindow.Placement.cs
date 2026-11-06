using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using CodeShellManager.Models;
using CodeShellManager.Services;

namespace CodeShellManager;

/// <summary>
/// Saving and restoring the main window's bounds under per-monitor DPI v2 (#181).
///
/// Restore used to set Left/Top/Width/Height from <c>OnLoaded</c> — after the window was
/// already shown, maximized, on the primary monitor — and clamp them against
/// <c>SystemParameters.VirtualScreen*</c>, which is in the PRIMARY monitor's DIPs. On a
/// secondary monitor with a different scale, moving there triggered a DPI change and a resize
/// mid-move, so the size drifted from run to run and the clamp could misplace it.
///
/// Now: the bounds are read synchronously in the constructor, and applied in
/// <c>OnSourceInitialized</c> — the HWND exists but has not been shown — with
/// <c>SetWindowPlacement</c>, in physical pixels, against the real monitor work areas.
/// The decisions themselves live in the WPF-free <see cref="WindowPlacementMath"/>.
/// </summary>
public partial class MainWindow
{
    /// <summary>Read in the constructor; consumed once by <see cref="OnSourceInitialized"/>.</summary>
    private StateService.SavedWindowState? _savedWindowState;

    /// <summary>
    /// Called from the constructor, before WPF shows the window. A saved Normal window must
    /// not start out maximized (the XAML default, kept for a first run): WPF would create the
    /// HWND maximized on the primary monitor, and the placement below would then move a
    /// maximized window rather than set its normal rectangle.
    /// </summary>
    private void LoadSavedWindowState()
    {
        _savedWindowState = StateService.TryReadWindowState();
        if (_savedWindowState is { LastPlacement: not null } or { LastNormalBounds: not null })
            WindowState = WindowState.Normal;
    }

    protected override void OnSourceInitialized(EventArgs e)
    {
        base.OnSourceInitialized(e);
        try { ApplySavedPlacement(); }
        catch (Exception ex) { StateService.Log($"window placement restore failed: {ex.Message}"); }
    }

    private void ApplySavedPlacement()
    {
        var saved = _savedWindowState;
        _savedWindowState = null;
        if (saved is null) return; // first run: XAML default (maximized on the primary)

        var hwnd = new WindowInteropHelper(this).Handle;
        if (hwnd == IntPtr.Zero) return;

        var monitors = EnumerateMonitors();
        var resolved = WindowPlacementMath.Resolve(saved.LastPlacement, saved.LastNormalBounds, monitors, MinWidth, MinHeight);
        if (resolved is null)
        {
            if (saved.WindowMaximized) WindowState = WindowState.Maximized;
            return;
        }

        var (rect, monitor) = resolved.Value;
        var ws = WindowPlacementMath.ScreenToWorkspace(rect, monitor);
        var wp = new WINDOWPLACEMENT
        {
            length = Marshal.SizeOf<WINDOWPLACEMENT>(),
            flags = 0,
            // SetWindowPlacement shows the window, and WPF picks up the state from WM_SIZE;
            // its own ShowWindow afterwards is then a no-op. Never restore minimized.
            showCmd = saved.WindowMaximized ? SW_SHOWMAXIMIZED : SW_SHOWNORMAL,
            rcNormalPosition = new RECT { Left = ws.Left, Top = ws.Top, Right = ws.Right, Bottom = ws.Bottom },
        };

        uint dpiBefore = GetDpiForWindow(hwnd);
        SetWindowPlacement(hwnd, ref wp);
        // Landing on a monitor of a different scale raises WM_DPICHANGED, and WPF applies the
        // rect Windows suggests — the saved size scaled by the DPI ratio. Applying again, now
        // that the window is already on that monitor, sets the exact saved rectangle. This
        // second call is what stops the size drifting run to run in a mixed-DPI setup.
        if (GetDpiForWindow(hwnd) != dpiBefore)
            SetWindowPlacement(hwnd, ref wp);
    }

    /// <summary>
    /// Pushes the current bounds into the view model: the DIP fields for older builds, and
    /// the physical normal rectangle from GetWindowPlacement — which is right even while the
    /// window is maximized or minimized, where Left/Top/Width/Height are not.
    /// </summary>
    private void CaptureWindowState()
    {
        WindowPlacementState? placement = null;
        bool maximized = WindowState == WindowState.Maximized;
        try
        {
            var hwnd = new WindowInteropHelper(this).Handle;
            var wp = new WINDOWPLACEMENT { length = Marshal.SizeOf<WINDOWPLACEMENT>() };
            if (hwnd != IntPtr.Zero && GetWindowPlacement(hwnd, ref wp))
            {
                var ws = new PixelRect(wp.rcNormalPosition.Left, wp.rcNormalPosition.Top,
                    wp.rcNormalPosition.Right - wp.rcNormalPosition.Left,
                    wp.rcNormalPosition.Bottom - wp.rcNormalPosition.Top);
                var monitor = MonitorAreaFor(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST));
                if (monitor != null && ws.Width > 0 && ws.Height > 0)
                {
                    var screen = WindowPlacementMath.WorkspaceToScreen(ws, monitor);
                    placement = new WindowPlacementState
                    {
                        Left = screen.Left, Top = screen.Top, Width = screen.Width, Height = screen.Height,
                        // The DPI the rect's pixel size belongs to: the window's monitor, which
                        // is also where its normal rect lives (a window maximizes in place).
                        Dpi = monitor.Dpi,
                    };
                }
                if (WindowState == WindowState.Minimized)
                    maximized = (wp.flags & WPF_RESTORETOMAXIMIZED) != 0;
            }
        }
        catch { /* keep the DIP fields; a failed native read must not break saving */ }

        _vm.UpdateWindowState(WindowState, Left, Top, Width, Height, placement, maximized);
    }

    private static List<MonitorArea> EnumerateMonitors()
    {
        var list = new List<MonitorArea>();
        EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, (h, _, _, _) =>
        {
            var m = MonitorAreaFor(h);
            if (m != null) list.Add(m);
            return true;
        }, IntPtr.Zero);
        return list;
    }

    private static MonitorArea? MonitorAreaFor(IntPtr hMonitor)
    {
        if (hMonitor == IntPtr.Zero) return null;
        var mi = new MONITORINFO { cbSize = Marshal.SizeOf<MONITORINFO>() };
        if (!GetMonitorInfo(hMonitor, ref mi)) return null;
        int dpi = GetDpiForMonitor(hMonitor, MDT_EFFECTIVE_DPI, out uint dx, out _) == 0 && dx > 0 ? (int)dx : 96;
        return new MonitorArea(ToPixelRect(mi.rcMonitor), ToPixelRect(mi.rcWork), dpi,
            (mi.dwFlags & MONITORINFOF_PRIMARY) != 0);
    }

    private static PixelRect ToPixelRect(RECT r) => new(r.Left, r.Top, r.Right - r.Left, r.Bottom - r.Top);

    // ── Win32 ────────────────────────────────────────────────────────────────────────

    private const int SW_SHOWNORMAL = 1;
    private const int SW_SHOWMAXIMIZED = 3;
    private const int WPF_RESTORETOMAXIMIZED = 0x2;
    private const uint MONITOR_DEFAULTTONEAREST = 2;
    private const uint MONITORINFOF_PRIMARY = 1;
    private const int MDT_EFFECTIVE_DPI = 0;

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X, Y; }

    [StructLayout(LayoutKind.Sequential)]
    private struct WINDOWPLACEMENT
    {
        public int length;
        public int flags;
        public int showCmd;
        public POINT ptMinPosition;
        public POINT ptMaxPosition;
        public RECT rcNormalPosition;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MONITORINFO
    {
        public int cbSize;
        public RECT rcMonitor;
        public RECT rcWork;
        public uint dwFlags;
    }

    private delegate bool MonitorEnumProc(IntPtr hMonitor, IntPtr hdc, IntPtr lprcMonitor, IntPtr data);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowPlacement(IntPtr hWnd, ref WINDOWPLACEMENT lpwndpl);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPlacement(IntPtr hWnd, ref WINDOWPLACEMENT lpwndpl);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromWindow(IntPtr hWnd, uint dwFlags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EnumDisplayMonitors(IntPtr hdc, IntPtr lprcClip, MonitorEnumProc lpfnEnum, IntPtr dwData);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetMonitorInfo(IntPtr hMonitor, ref MONITORINFO lpmi);

    [DllImport("shcore.dll")]
    private static extern int GetDpiForMonitor(IntPtr hMonitor, int dpiType, out uint dpiX, out uint dpiY);
}

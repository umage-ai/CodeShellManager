using System;
using System.Runtime.InteropServices;
using System.Windows;

namespace CodeShellManager.Services;

/// <summary>
/// Shows a Windows balloon tooltip from the system tray, or falls back to a WPF MessageBox.
/// Full WinRT toast notifications require package identity; we use a simpler approach.
/// </summary>
public static class ToastHelper
{
    public static void Show(string title, string message)
    {
        try
        {
            // Use the taskbar notify icon if available (set up in App.xaml.cs)
            if (App.TrayIcon != null)
            {
                App.TrayIcon.ShowBalloonTip(4000, $"CodeShellManager — {title}", message,
                    System.Windows.Forms.ToolTipIcon.Info);
            }
        }
        catch { /* non-critical */ }
    }
}

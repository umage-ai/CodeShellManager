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
    public static void Show(string title, string message, bool playSound = false)
    {
        try
        {
            if (App.TrayIcon != null)
            {
                // ToolTipIcon.None avoids Windows auto-playing a sound with the balloon.
                // We play the sound ourselves so it's independently controllable.
                if (playSound)
                    System.Media.SystemSounds.Asterisk.Play();
                App.TrayIcon.ShowBalloonTip(4000, $"CodeShellManager — {title}", message,
                    System.Windows.Forms.ToolTipIcon.None);
            }
        }
        catch { /* non-critical */ }
    }
}

using CodeShellManager.Services;

namespace CodeShellManager.Services;

/// <summary>WPF <see cref="IToastNotifier"/> — delegates to the existing tray helper.</summary>
public sealed class WpfToastNotifier : IToastNotifier
{
    public void Show(string title, string message, bool playSound)
        => ToastHelper.Show(title, message, playSound);
}

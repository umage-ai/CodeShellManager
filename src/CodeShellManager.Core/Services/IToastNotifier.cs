namespace CodeShellManager.Services;

/// <summary>Shows a host notification (tray balloon on WPF). Implemented per-host.</summary>
public interface IToastNotifier
{
    void Show(string title, string message, bool playSound);
}

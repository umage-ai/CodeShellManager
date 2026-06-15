using CodeShellManager.Models;

namespace CodeShellManager.Terminal;

/// <summary>
/// The slice of the terminal bridge that platform-agnostic view-models depend on.
/// The WPF app's WebView2-backed <c>TerminalBridge</c> implements it; a future
/// Avalonia bridge will implement the same surface. Members here are exactly those
/// called through the typed <c>SessionViewModel.Bridge</c> property — nothing more.
/// </summary>
public interface ITerminalBridge : IDisposable
{
    event Action? UserInput;
    void SendToTerminal(string text);
    void FitTerminal();
    void FocusTerminal();
    void ApplyFontSettings(AppSettings settings);
}

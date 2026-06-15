namespace CodeShellManager.Services;

/// <summary>Marshals an action onto the UI thread. Implemented per-host (WPF, Avalonia).</summary>
public interface IDispatcher
{
    void Post(Action action);
}

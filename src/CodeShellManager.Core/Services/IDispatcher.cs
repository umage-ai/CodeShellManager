namespace CodeShellManager.Services;

/// <summary>
/// Marshals an action onto the UI thread. Implemented per-host (WPF, Avalonia).
/// </summary>
public interface IDispatcher
{
    /// <summary>
    /// Runs <paramref name="action"/> on the UI thread and BLOCKS until it completes.
    /// Despite the name, this is synchronous: callers rely on the action having finished
    /// (e.g. property-change notifications raised) before control returns. Implementations
    /// MUST use a blocking marshal — on WPF <c>Dispatcher.Invoke</c>, on Avalonia
    /// <c>Dispatcher.UIThread.Invoke</c> — NOT a fire-and-forget post.
    /// </summary>
    void Post(Action action);
}

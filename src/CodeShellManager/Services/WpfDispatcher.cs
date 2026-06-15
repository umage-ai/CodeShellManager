using System;
using CodeShellManager.Services;

namespace CodeShellManager.Services;

/// <summary>WPF <see cref="IDispatcher"/> — posts onto the application dispatcher.</summary>
public sealed class WpfDispatcher : IDispatcher
{
    public void Post(Action action) => System.Windows.Application.Current.Dispatcher.Invoke(action);
}

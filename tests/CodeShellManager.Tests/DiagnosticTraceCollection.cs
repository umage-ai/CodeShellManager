using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// DiagnosticTrace is process-wide static state (log path, Enabled, UiThreadId). Tests that
/// touch it must not run concurrently with each other or they clobber one another's setup.
/// </summary>
[CollectionDefinition("DiagnosticTrace", DisableParallelization = true)]
public class DiagnosticTraceCollection { }

/// <summary>
/// GitRepoWatcher keeps a process-wide shared map; the SharedCount assertions in its tests
/// would race with any parallel class that Acquires.
/// </summary>
[CollectionDefinition("GitRepoWatcher", DisableParallelization = true)]
public class GitRepoWatcherCollection { }

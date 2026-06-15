using System;

namespace CodeShellManager.Terminal;

/// <summary>
/// Minimum surface of <see cref="PseudoTerminal"/> consumed by
/// <see cref="Services.RunInstance"/>. Extracted so callers can be unit-tested
/// without spawning a real ConPTY child — production code still constructs the
/// concrete <see cref="PseudoTerminal"/>; tests substitute a fake.
/// </summary>
public interface IPseudoTerminal : IDisposable
{
    /// <summary>Fires from the PTY read loop's thread with a decoded UTF-8 chunk.</summary>
    event Action<string>? DataReceived;

    /// <summary>Fires once after the child process has exited and <see cref="ExitCode"/> is populated.</summary>
    event Action? Exited;

    /// <summary>Exit code of the child process. Null while running.</summary>
    int? ExitCode { get; }

    /// <summary>Spawns the child process. Mirrors <see cref="PseudoTerminal.Start"/>.</summary>
    void Start(string command, string args, string workingDirectory,
        int cols = 220, int rows = 50, bool useJobObject = false);
}

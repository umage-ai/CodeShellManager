using System;
using System.IO;
using System.Text;
using System.Text.RegularExpressions;
using CodeShellManager.Models;
using CodeShellManager.Terminal;
using CommunityToolkit.Mvvm.ComponentModel;

namespace CodeShellManager.Services;

public enum RunState
{
    Idle,           // never started, or output cleared
    Running,
    ExitedOk,       // exit code 0
    ExitedFailed,   // exit code != 0
}

/// <summary>
/// Runtime state for one invocation of a <see cref="RunCommandItem"/>.
/// Owns a headless <see cref="PseudoTerminal"/> and accumulates output into
/// a string buffer for display in the drawer / sending to the parent terminal.
/// NOT persisted to state.json.
/// </summary>
public partial class RunInstance : ObservableObject, IDisposable
{
    private const int MaxBufferChars = 1_000_000; // ~1MB ceiling; older content is dropped from the head

    public string ItemId { get; }
    public string Label { get; }
    public string CommandLine { get; }

    [ObservableProperty] private RunState _state = RunState.Idle;
    [ObservableProperty] private int? _exitCode;
    [ObservableProperty] private string _outputBuffer = "";
    [ObservableProperty] private DateTime? _startedAt;
    [ObservableProperty] private DateTime? _endedAt;

    public TimeSpan? Duration => StartedAt is { } s && EndedAt is { } e ? e - s : null;

    private PseudoTerminal? _pty;
    private readonly StringBuilder _ansiStripped = new();
    private readonly object _bufLock = new();
    private bool _disposed;
    private bool _stopRequested;

    public event Action? OutputChanged;
    public event Action? StateChanged;

    public RunInstance(RunCommandItem item)
    {
        ItemId = item.Id;
        Label = item.Label;
        CommandLine = item.CommandLine;
    }

    /// <summary>
    /// Spawns the child PTY. Builds the command line based on whether the parent
    /// is local or remote — see <see cref="BuildLocalCmd"/> / <see cref="BuildSshArgs"/>.
    /// </summary>
    public void Start(ShellSession parent)
    {
        if (_pty != null) throw new InvalidOperationException("Already started — call Dispose() first.");

        lock (_bufLock) { _ansiStripped.Clear(); }
        OutputBuffer = "";
        ExitCode = null;
        StartedAt = DateTime.Now;
        EndedAt = null;
        State = RunState.Running;
        StateChanged?.Invoke();

        _pty = new PseudoTerminal();
        _pty.DataReceived += OnPtyData;
        _pty.Exited += OnPtyExited;

        string command, args, workDir;
        if (parent.IsRemote)
        {
            command = "ssh";
            args = BuildSshArgs(parent, CommandLine);
            workDir = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }
        else
        {
            command = "cmd";
            args = BuildLocalCmd(CommandLine);
            workDir = Directory.Exists(parent.WorkingFolder)
                ? parent.WorkingFolder
                : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }

        _pty.Start(command, args, workDir, cols: 200, rows: 50);
    }

    public void Stop()
    {
        _stopRequested = true;
        Dispose();
    }

    private void OnPtyData(string text)
    {
        // Strip ANSI for the readonly drawer view + clipboard. Match the
        // OutputIndexer regex so any visible quirks stay consistent across the app.
        string stripped = AnsiPattern().Replace(text, "");
        lock (_bufLock)
        {
            _ansiStripped.Append(stripped);
            if (_ansiStripped.Length > MaxBufferChars)
                _ansiStripped.Remove(0, _ansiStripped.Length - MaxBufferChars);
        }
        // Marshal to UI thread is the consumer's responsibility — OutputChanged
        // fires from the PTY read loop's thread.
        OutputChanged?.Invoke();
    }

    private void OnPtyExited()
    {
        EndedAt = DateTime.Now;
        // PTY does not expose the exit code today — infer state from whether the
        // user explicitly stopped us. Natural exit = treat as success; explicit Stop
        // = failed/cancelled. ExitCode stays null in both cases.
        State = _stopRequested ? RunState.ExitedFailed : RunState.ExitedOk;
        StateChanged?.Invoke();
    }

    /// <summary>
    /// Snapshots the current ANSI-stripped buffer. Thread-safe.
    /// </summary>
    public string SnapshotOutput()
    {
        lock (_bufLock) return _ansiStripped.ToString();
    }

    /// <summary>
    /// Wraps a single-statement CommandLine for cmd.exe so &amp;&amp;, pipes,
    /// redirects, and quoted args all parse correctly. The outer cmd /c
    /// exits when the wrapped process exits — needed for clean Exited firing.
    /// </summary>
    internal static string BuildLocalCmd(string commandLine) => $"/c \"{commandLine}\"";

    /// <summary>
    /// Builds ssh args for a remote run. Pattern:
    ///   -p PORT -t user@host "cd '/folder' &amp;&amp; bash -c '<escaped>'"
    /// </summary>
    internal static string BuildSshArgs(ShellSession parent, string commandLine)
    {
        var sb = new StringBuilder();
        if (parent.SshPort != 22) sb.Append($"-p {parent.SshPort} ");
        sb.Append("-t ");
        sb.Append(string.IsNullOrWhiteSpace(parent.SshUser)
            ? parent.SshHost
            : $"{parent.SshUser}@{parent.SshHost}");
        sb.Append(" \"");
        if (!string.IsNullOrWhiteSpace(parent.SshRemoteFolder))
            sb.Append($"cd '{parent.SshRemoteFolder}' && ");
        sb.Append("bash -c ");
        sb.Append(SingleQuoteEscape(commandLine));
        sb.Append("\"");
        return sb.ToString();
    }

    /// <summary>
    /// POSIX single-quote escape: wraps in single quotes, replacing any inner
    /// single quote with '\'' so the shell still receives the literal char.
    /// E.g. <c>can't do</c> → <c>'can'\''t do'</c>.
    /// </summary>
    internal static string SingleQuoteEscape(string s) => "'" + s.Replace("'", "'\\''") + "'";

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        if (_pty != null)
        {
            _pty.DataReceived -= OnPtyData;
            _pty.Exited -= OnPtyExited;
            _pty.Dispose();
            _pty = null;
        }
        // If we were killed externally before the child exited naturally,
        // mark as failed with no exit code (it didn't get to report one).
        if (State == RunState.Running)
        {
            EndedAt = DateTime.Now;
            State = RunState.ExitedFailed;
            StateChanged?.Invoke();
        }
    }

    [GeneratedRegex(@"\x1B\[[0-9;]*[mGKHFJABCDsuhl]|\x1B\].*?\x07|\x1B[=>]|\r", RegexOptions.Compiled)]
    private static partial Regex AnsiPattern();
}

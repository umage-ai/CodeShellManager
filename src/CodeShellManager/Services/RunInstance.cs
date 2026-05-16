using System;
using System.Diagnostics;
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
    public RunMode Mode { get; }
    public string? PostRunUrl { get; }

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

    public event Action? OutputChanged;
    public event Action? StateChanged;

    public RunInstance(RunCommandItem item)
    {
        ItemId = item.Id;
        Label = item.Label;
        CommandLine = item.CommandLine;
        Mode = item.Mode;
        PostRunUrl = item.PostRunUrl;
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
            // SSH parents always go through bash — Mode is meaningless for remote runs.
            command = "ssh";
            args = BuildSshArgs(parent, CommandLine);
            workDir = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }
        else if (Mode == RunMode.PowerShell)
        {
            command = ResolvePwsh();
            args = BuildPwshArgs(CommandLine);
            workDir = Directory.Exists(parent.WorkingFolder)
                ? parent.WorkingFolder
                : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }
        else
        {
            command = "cmd";
            args = BuildLocalCmd(CommandLine);
            workDir = Directory.Exists(parent.WorkingFolder)
                ? parent.WorkingFolder
                : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        }

        _pty.Start(command, args, workDir, cols: 200, rows: 50, useJobObject: true);
    }

    public void Stop()
    {
        // Disposing the PTY closes the Job Object → kills the whole process tree.
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
        ExitCode = _pty?.ExitCode;
        State = ExitCode == 0 ? RunState.ExitedOk : RunState.ExitedFailed;
        StateChanged?.Invoke();

        // Open post-run URL on success only. ShellExecute hands the URL to the
        // OS default browser. We swallow exceptions because there's nothing
        // useful to surface from the PTY exit callback's thread.
        if (State == RunState.ExitedOk && !string.IsNullOrWhiteSpace(PostRunUrl))
        {
            try { Process.Start(new ProcessStartInfo(PostRunUrl) { UseShellExecute = true }); }
            catch { /* invalid URL or no associated handler — caller can't do anything */ }
        }
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
    /// Returns "pwsh.exe" if PowerShell 7+ is on PATH, otherwise falls back to
    /// the Windows-bundled "powershell.exe". ConPTY's CreateProcess resolves PATH
    /// for us — we just pick which name to ask for.
    /// </summary>
    internal static string ResolvePwsh()
    {
        // Cheap check: try to spawn pwsh -NoLogo -Command "exit". If it returns,
        // we trust pwsh is on PATH. Use a one-shot Process so we don't perturb
        // the user's environment. Skip the probe if we already know.
        if (_pwshResolved is { } cached) return cached;

        try
        {
            using var probe = Process.Start(new ProcessStartInfo
            {
                FileName = "pwsh.exe",
                Arguments = "-NoLogo -NoProfile -Command \"exit 0\"",
                UseShellExecute = false,
                CreateNoWindow = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            });
            if (probe != null)
            {
                probe.WaitForExit(2000);
                _pwshResolved = "pwsh.exe";
                return _pwshResolved;
            }
        }
        catch { /* not on PATH */ }

        _pwshResolved = "powershell.exe";
        return _pwshResolved;
    }
    private static string? _pwshResolved;

    /// <summary>
    /// Builds powershell args using -EncodedCommand so we don't have to worry
    /// about quoting in the user's command line. The payload is UTF-16 LE base64,
    /// which is what -EncodedCommand expects.
    /// </summary>
    internal static string BuildPwshArgs(string commandLine)
    {
        string b64 = Convert.ToBase64String(Encoding.Unicode.GetBytes(commandLine));
        return $"-NonInteractive -NoLogo -ExecutionPolicy Bypass -EncodedCommand {b64}";
    }

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

    // Mirrors the strip regex in OutputIndexer.AnsiPattern — keep them in sync.
    // The `?` inside [?0-9;]* covers CSI private-mode sequences like ESC[?9001h
    // (ConPTY's Win32 input-mode bootstrap), ESC[?1049h (alt screen), etc.
    [GeneratedRegex(@"\x1B\[[?0-9;]*[mGKHFJABCDsuhl]|\x1B\].*?(?:\x07|\x1B\\)|\x1B[=>]|\r", RegexOptions.Compiled)]
    private static partial Regex AnsiPattern();
}

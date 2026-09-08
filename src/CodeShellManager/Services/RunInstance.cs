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

    private IPseudoTerminal? _pty;
    private readonly Func<IPseudoTerminal> _ptyFactory;
    private readonly StringBuilder _ansiStripped = new();
    private readonly object _bufLock = new();
    private bool _disposed;

    public event Action? OutputChanged;
    public event Action? StateChanged;

    public RunInstance(RunCommandItem item)
        : this(item, static () => new PseudoTerminal())
    {
    }

    /// <summary>
    /// Test seam — accepts a factory that produces an <see cref="IPseudoTerminal"/>.
    /// Production code uses the parameterless ctor which delegates to <see cref="PseudoTerminal"/>.
    /// </summary>
    internal RunInstance(RunCommandItem item, Func<IPseudoTerminal> ptyFactory)
    {
        ItemId = item.Id;
        Label = item.Label;
        CommandLine = item.CommandLine;
        Mode = item.Mode;
        PostRunUrl = item.PostRunUrl;
        _ptyFactory = ptyFactory;
    }

    /// <summary>
    /// Spawns the child PTY. Builds the command line based on the parent's
    /// <see cref="ShellSession.Kind"/> — see <see cref="BuildLocalCmd"/>,
    /// <see cref="BuildSshArgs"/>, and <see cref="BuildWslArgs"/>.
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

        _pty = _ptyFactory();
        _pty.DataReceived += OnPtyData;
        _pty.Exited += OnPtyExited;

        try
        {
            string command, args, workDir;
            switch (parent.Kind)
            {
                case SessionKind.Ssh:
                    // SSH parents always go through bash — Mode is meaningless for remote runs.
                    command = "ssh";
                    args = BuildSshArgs(parent, CommandLine);
                    workDir = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                    break;
                case SessionKind.Wsl:
                    // WSL parents wrap the command in `wsl.exe … -- bash -lc` —
                    // running pwsh inside WSL is out of scope so Mode is ignored here too.
                    command = "wsl.exe";
                    args = BuildWslArgs(parent, CommandLine);
                    workDir = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                    break;
                default:
                    if (Mode == RunMode.PowerShell)
                    {
                        command = ResolvePwsh();
                        args = BuildPwshArgs(CommandLine);
                    }
                    else
                    {
                        command = "cmd";
                        args = BuildLocalCmd(CommandLine);
                    }
                    workDir = Directory.Exists(parent.WorkingFolder)
                        ? parent.WorkingFolder
                        : Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                    break;
            }

            _pty.Start(command, args, workDir, cols: 200, rows: 50, useJobObject: true);
        }
        catch (Exception ex)
        {
            // A run that can't even build its command line (e.g. blank WslDistro) must
            // show as a failed chip, not throw out of the toolbar click.
            AppendText($"Cannot start: {ex.Message}\r\n");
            ExitCode = -1;
            EndedAt = DateTime.Now;
            State = RunState.ExitedFailed;
            StateChanged?.Invoke();
            _pty.DataReceived -= OnPtyData;
            _pty.Exited -= OnPtyExited;
            _pty.Dispose();
            _pty = null;
        }
    }

    /// <summary>
    /// Appends text to the ANSI-stripped output buffer under <see cref="_bufLock"/>,
    /// refreshes <see cref="OutputBuffer"/>, and raises <see cref="OutputChanged"/>.
    /// </summary>
    private void AppendText(string text)
    {
        string snapshot;
        lock (_bufLock)
        {
            _ansiStripped.Append(text);
            if (_ansiStripped.Length > MaxBufferChars)
                _ansiStripped.Remove(0, _ansiStripped.Length - MaxBufferChars);
            snapshot = _ansiStripped.ToString();
        }
        OutputBuffer = snapshot;
        // Marshal to UI thread is the consumer's responsibility — OutputChanged
        // fires from the PTY read loop's thread (or, for the start-failure path,
        // synchronously from Start).
        OutputChanged?.Invoke();
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
        // Marshal to UI thread is the consumer's responsibility — OutputChanged
        // fires from the PTY read loop's thread.
        //
        // Deliberately NOT routed through AppendText: the PTY read loop hands us
        // 4KB chunks, and AppendText's OutputBuffer = ToString() snapshot would be
        // a full copy of the (up to 1MB) buffer per chunk — LOH churn plus a
        // PropertyChanged per chunk on a hot path. OutputBuffer has no consumers
        // in the app (only tests) — the app reads SnapshotOutput() on demand.
        string stripped = AnsiPattern().Replace(text, "");
        lock (_bufLock)
        {
            _ansiStripped.Append(stripped);
            if (_ansiStripped.Length > MaxBufferChars)
                _ansiStripped.Remove(0, _ansiStripped.Length - MaxBufferChars);
        }
        OutputChanged?.Invoke();
    }

    private void OnPtyExited()
    {
        EndedAt = DateTime.Now;
        ExitCode = _pty?.ExitCode;
        State = ExitCode == 0 ? RunState.ExitedOk : RunState.ExitedFailed;
        StateChanged?.Invoke();

        // Open post-run URL on success only. ShellExecute hands the URL to the
        // OS default browser. We can't pop UI from the PTY-exit callback thread,
        // so failures are logged to crash.log for diagnosability rather than silenced.
        if (State == RunState.ExitedOk && !string.IsNullOrWhiteSpace(PostRunUrl))
        {
            if (!TryGetLaunchableUrl(PostRunUrl, out string? safeUrl))
            {
                LogPostRunUrl(PostRunUrl, "rejected — only http and https URLs are opened");
                return;
            }
            // safeUrl, not PostRunUrl: launch the string the validator actually inspected.
            // Uri.TryCreate accepts and internally escapes characters that the raw string
            // still contains — a quote or space survives into ShellExecute, which expands it
            // into the handler's registered `shell\open\command` template. Modern browsers
            // pass --single-argument and are unaffected; other registered handlers may not
            // be. Validating one string and launching a different one is the bug class,
            // regardless of who is currently immune to it.
            try { Process.Start(new ProcessStartInfo(safeUrl!) { UseShellExecute = true }); }
            catch (Exception ex) { LogPostRunUrl(PostRunUrl, ex.Message); }
        }
    }

    /// <summary>
    /// True when <paramref name="url"/> is safe to hand to ShellExecute — an absolute
    /// http or https URL, and nothing else.
    ///
    /// This fires automatically when a run exits 0, with no confirmation step, and
    /// ShellExecute will happily launch a local executable, a .ps1, a UNC path or any
    /// registered protocol handler. A whole AppState — run commands included — can be
    /// imported from a JSON file the user didn't write (see ImportExportService), so the
    /// scheme is checked at launch time rather than trusting the stored value.
    ///
    /// Scheme-less input like "localhost:5173" is rejected too: Uri parses it as scheme
    /// "localhost", and guessing http:// on the user's behalf would defeat the check.
    /// </summary>
    internal static bool IsLaunchableUrl(string? url) => TryGetLaunchableUrl(url, out _);

    /// <summary>
    /// Validates and returns the exact string to launch — <see cref="Uri.AbsoluteUri"/>,
    /// the normalized form, so the value inspected and the value launched are identical.
    /// </summary>
    internal static bool TryGetLaunchableUrl(string? url, out string? safeUrl)
    {
        safeUrl = null;
        if (!Uri.TryCreate(url, UriKind.Absolute, out Uri? uri)) return false;
        if (uri.Scheme != Uri.UriSchemeHttp && uri.Scheme != Uri.UriSchemeHttps) return false;

        safeUrl = uri.AbsoluteUri;
        return true;
    }

    private static void LogPostRunUrl(string url, string detail)
    {
        try
        {
            string path = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "CodeShellManager", "crash.log");
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.AppendAllText(path,
                $"[{DateTime.Now:HH:mm:ss.fff}] PostRunUrl '{url}': {detail}\n");
        }
        catch { /* logger failure is not actionable */ }
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
    /// Returns "pwsh.exe" if PowerShell 7+ is on PATH, otherwise "powershell.exe".
    /// Delegates to <see cref="PwshLocator"/> so this and PseudoTerminal's session
    /// wrapper can never disagree about which shell the machine has.
    /// </summary>
    internal static string ResolvePwsh() => PwshLocator.Executable;

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
            // Escaped for the same reason the command below is — the folder was the one
            // value here that wasn't, and it comes from state.json. See ShellSession.BuildSshArgs.
            sb.Append($"cd {SingleQuoteEscape(parent.SshRemoteFolder)} && ");
        sb.Append("bash -c ");
        sb.Append(SingleQuoteEscape(commandLine));
        sb.Append("\"");
        return sb.ToString();
    }

    /// <summary>
    /// wsl.exe args for a run inside the parent's distro. One implementation with the
    /// session launcher — see <see cref="ShellSession.BuildWslArgs"/> — so the two can't
    /// disagree about quoting or about a blank distro.
    /// </summary>
    internal static string BuildWslArgs(ShellSession parent, string commandLine)
        => parent.BuildWslArgs(commandLine);

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

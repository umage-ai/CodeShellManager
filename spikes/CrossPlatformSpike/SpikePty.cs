using System.Runtime.InteropServices;
using System.Text;
using Porta.Pty;

namespace CrossPlatformSpike;

/// <summary>Thin wrapper over Porta.Pty, shaped like the main app's IPseudoTerminal.</summary>
public sealed class SpikePty : IDisposable
{
    private IPtyConnection? _pty;
    private CancellationTokenSource? _readCts;

    public event Action<byte[]>? DataReceived;
    public event Action<int>? Exited;

    public int Pid => _pty?.Pid ?? -1;
    public string ShellPath { get; private set; } = "";

    public static string GetDefaultShell()
    {
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
            return FindOnPath("pwsh.exe") ?? Path.Combine(Environment.SystemDirectory, "cmd.exe");
        var shell = Environment.GetEnvironmentVariable("SHELL");
        return string.IsNullOrEmpty(shell) ? "/bin/bash" : shell;
    }

    private static string? FindOnPath(string exe)
    {
        foreach (var dir in (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator))
        {
            try
            {
                var candidate = Path.Combine(dir.Trim(), exe);
                if (File.Exists(candidate)) return candidate;
            }
            catch { /* malformed PATH entry */ }
        }
        return null;
    }

    public async Task StartAsync(int cols, int rows)
    {
        ShellPath = GetDefaultShell();
        var env = new Dictionary<string, string>();
        // Unix TUIs read TERM for capabilities; xterm-256color matches xterm.js.
        if (!RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
            env["TERM"] = "xterm-256color";

        _pty = await PtyProvider.SpawnAsync(new PtyOptions
        {
            Name = "spike",
            App = ShellPath,
            Cols = cols,
            Rows = rows,
            Cwd = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            CommandLine = Array.Empty<string>(),
            Environment = env,
        }, CancellationToken.None);

        _pty.ProcessExited += (_, e) => Exited?.Invoke(e.ExitCode);

        _readCts = new CancellationTokenSource();
        _ = Task.Run(() => ReadLoopAsync(_pty, _readCts.Token));
    }

    private async Task ReadLoopAsync(IPtyConnection pty, CancellationToken ct)
    {
        var buffer = new byte[4096];
        try
        {
            while (!ct.IsCancellationRequested)
            {
                int n = await pty.ReaderStream.ReadAsync(buffer, ct);
                if (n == 0) break;
                DataReceived?.Invoke(buffer[..n]);
            }
        }
        catch (OperationCanceledException) { }
        catch (ObjectDisposedException) { }
    }

    public async Task WriteAsync(string data)
    {
        if (_pty is null) return;
        var bytes = Encoding.UTF8.GetBytes(data);
        await _pty.WriterStream.WriteAsync(bytes);
        await _pty.WriterStream.FlushAsync();
    }

    public void Resize(int cols, int rows) => _pty?.Resize(cols, rows);

    public void Dispose()
    {
        _readCts?.Cancel();
        try { _pty?.Kill(); } catch { /* already dead */ }
        _pty?.Dispose();
    }
}

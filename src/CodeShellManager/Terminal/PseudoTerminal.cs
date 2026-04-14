using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Win32.SafeHandles;

namespace CodeShellManager.Terminal;

/// <summary>
/// Wraps the Windows ConPTY (Pseudo Console) API to host an interactive terminal process.
/// </summary>
public sealed class PseudoTerminal : IDisposable
{
    // ── P/Invoke ──────────────────────────────────────────────────────────────

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CreatePipe(out SafeFileHandle hReadPipe, out SafeFileHandle hWritePipe,
        IntPtr lpPipeAttributes, int nSize);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern bool CreateProcess(string? lpApplicationName, string lpCommandLine,
        IntPtr lpProcessAttributes, IntPtr lpThreadAttributes, bool bInheritHandles,
        uint dwCreationFlags, IntPtr lpEnvironment, string? lpCurrentDirectory,
        ref STARTUPINFOEX lpStartupInfo, out PROCESS_INFORMATION lpProcessInformation);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern int CreatePseudoConsole(COORD size, SafeFileHandle hInput,
        SafeFileHandle hOutput, uint dwFlags, out IntPtr phPC);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern int ResizePseudoConsole(IntPtr hPC, COORD size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern void ClosePseudoConsole(IntPtr hPC);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool InitializeProcThreadAttributeList(IntPtr lpAttributeList,
        int dwAttributeCount, int dwFlags, ref IntPtr lpSize);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool UpdateProcThreadAttribute(IntPtr lpAttributeList, uint dwFlags,
        IntPtr Attribute, IntPtr lpValue, IntPtr cbSize, IntPtr lpPreviousValue,
        IntPtr lpReturnSize);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool DeleteProcThreadAttributeList(IntPtr lpAttributeList);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr hObject);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint WaitForSingleObject(IntPtr hHandle, uint dwMilliseconds);

    // ── Structs ───────────────────────────────────────────────────────────────

    [StructLayout(LayoutKind.Sequential)]
    private struct COORD { public short X; public short Y; }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct STARTUPINFOEX
    {
        public STARTUPINFO StartupInfo;
        public IntPtr lpAttributeList;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct STARTUPINFO
    {
        public int cb;
        public string? lpReserved, lpDesktop, lpTitle;
        public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
        public short wShowWindow, cbReserved2;
        public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PROCESS_INFORMATION
    {
        public IntPtr hProcess, hThread;
        public int dwProcessId, dwThreadId;
    }

    private const uint EXTENDED_STARTUPINFO_PRESENT = 0x00080000;
    private const IntPtr PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = (IntPtr)0x00020016;

    // ── Fields ────────────────────────────────────────────────────────────────

    private IntPtr _hPC = IntPtr.Zero;
    private IntPtr _hProcess = IntPtr.Zero;
    private SafeFileHandle? _inputRead, _inputWrite, _outputRead, _outputWrite;
    private FileStream? _stdin, _stdout;
    private CancellationTokenSource _cts = new();
    private bool _disposed;
    // Stateful UTF-8 decoder — preserves state across reads so multi-byte sequences
    // (box-drawing chars, emoji, etc.) split at buffer boundaries decode correctly.
    private readonly Decoder _utf8 = Encoding.UTF8.GetDecoder();

    public event Action<string>? DataReceived;
    public event Action? Exited;

    public bool IsRunning => _hProcess != IntPtr.Zero;

    // ── Public API ────────────────────────────────────────────────────────────

    private static string BuildCmdLine(string command, string fullUserCmd)
    {
        // Shells are passed through as-is (they initialize the console themselves).
        string exe = System.IO.Path.GetFileNameWithoutExtension(command).ToLowerInvariant();
        if (exe is "cmd" or "powershell" or "pwsh" or "wsl" or "bash" or "zsh" or "sh")
            return fullUserCmd;

        // Wrap in PowerShell so the shell sets up the Win32 console environment
        // before launching the target process (Electron/Node SEA apps like claude.exe
        // crash with STATUS_DLL_INIT_FAILED when launched directly inside a ConPTY).
        return $"powershell.exe -NoExit -Command {fullUserCmd}";
    }

    public void Start(string command, string args, string workingDirectory, int cols = 220, int rows = 50)
    {
        // Create pipe pairs: input to PTY, output from PTY
        CreatePipe(out _inputRead!, out _inputWrite!, IntPtr.Zero, 0);
        CreatePipe(out _outputRead!, out _outputWrite!, IntPtr.Zero, 0);

        var size = new COORD { X = (short)cols, Y = (short)rows };
        int hr = CreatePseudoConsole(size, _inputRead, _outputWrite, 0, out _hPC);
        if (hr != 0) throw new InvalidOperationException($"CreatePseudoConsole failed: 0x{hr:X}");

        // Build attribute list with PTY handle
        IntPtr attrListSize = IntPtr.Zero;
        InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref attrListSize);
        IntPtr attrList = Marshal.AllocHGlobal(attrListSize);
        try
        {
            if (!InitializeProcThreadAttributeList(attrList, 1, 0, ref attrListSize))
                throw new InvalidOperationException("InitializeProcThreadAttributeList failed");

            // Pass the HPCON handle value directly as lpValue.
            // UpdateProcThreadAttribute for PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE
            // stores the HPCON itself (an opaque handle/pointer), not a pointer to it.
            if (!UpdateProcThreadAttribute(attrList, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                    _hPC, (IntPtr)IntPtr.Size, IntPtr.Zero, IntPtr.Zero))
                throw new InvalidOperationException("UpdateProcThreadAttribute failed");

            var si = new STARTUPINFOEX
            {
                StartupInfo = new STARTUPINFO { cb = Marshal.SizeOf<STARTUPINFOEX>() },
                lpAttributeList = attrList
            };

            // Build command line. Route through cmd.exe unless the user explicitly asked for
            // cmd or powershell — some large executables (Electron/Node SEA like claude.exe)
            // crash with STATUS_DLL_INIT_FAILED when launched directly inside a ConPTY without
            // a shell wrapper to set up the console environment first.
            string userCmd = string.IsNullOrWhiteSpace(args) ? command : $"{command} {args}";
            string cmdLine = BuildCmdLine(command, userCmd);
            string? workDir = string.IsNullOrWhiteSpace(workingDirectory) ? null : workingDirectory;

            Log($"CreateProcess cmdLine='{cmdLine}' workDir='{workDir}'");
            if (!CreateProcess(null, cmdLine, IntPtr.Zero, IntPtr.Zero, false,
                    EXTENDED_STARTUPINFO_PRESENT, IntPtr.Zero, workDir, ref si, out var pi))
                throw new InvalidOperationException($"CreateProcess failed: {Marshal.GetLastWin32Error()}");

            _hProcess = pi.hProcess;
            CloseHandle(pi.hThread);
        }
        finally
        {
            DeleteProcThreadAttributeList(attrList);
            Marshal.FreeHGlobal(attrList);
        }

        // ConPTY now owns _inputRead and _outputWrite.
        // Close our copies so the pipe sees EOF when the process exits and
        // ConPTY drops its end — otherwise Read() blocks forever.
        _inputRead!.Close();
        _outputWrite!.Close();

        // Wrap pipes in streams
        _stdin = new FileStream(_inputWrite!, FileAccess.Write, 4096, false);
        _stdout = new FileStream(_outputRead!, FileAccess.Read, 4096, false);

        _ = Task.Run(ReadLoopAsync);
        _ = Task.Run(MonitorExitAsync);
    }

    public void Write(string text)
    {
        if (_stdin == null || !IsRunning) return;
        var bytes = Encoding.UTF8.GetBytes(text);
        _stdin.Write(bytes, 0, bytes.Length);
        _stdin.Flush();
    }

    public void Resize(int cols, int rows)
    {
        if (_hPC == IntPtr.Zero) return;
        ResizePseudoConsole(_hPC, new COORD { X = (short)cols, Y = (short)rows });
    }

    // ── Private ───────────────────────────────────────────────────────────────

    private static void Log(string msg)
    {
        try
        {
            string path = System.IO.Path.Combine(
                System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData),
                "CodeShellManager", "crash.log");
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
            System.IO.File.AppendAllText(path, $"[{DateTime.Now:HH:mm:ss.fff}] PTY {msg}\n");
        }
        catch { }
    }

    private async Task ReadLoopAsync()
    {
        var buffer = new byte[4096];
        try
        {
            while (!_cts.IsCancellationRequested)
            {
                // Anonymous pipes are synchronous — ReadAsync stalls on Windows.
                // Use blocking Read() on a thread-pool thread instead.
                int read = await Task.Run(() =>
                {
                    try { return _stdout!.Read(buffer, 0, buffer.Length); }
                    catch { return 0; }
                }, _cts.Token);

                if (read == 0) break;
                // Send raw bytes as Latin-1 so xterm.js can interpret VT sequences correctly
                // Decoder.GetString handles multi-byte sequences split across reads
                int charCount = _utf8.GetCharCount(buffer, 0, read);
                char[] chars = new char[charCount];
                _utf8.GetChars(buffer, 0, read, chars, 0);
                string text = new string(chars);
                DataReceived?.Invoke(text);
            }
        }
        catch (OperationCanceledException) { }
        catch (IOException) { } // pipe closed when process exits
        catch (Exception ex) { Log($"ReadLoop error: {ex.Message}"); }
    }

    private async Task MonitorExitAsync()
    {
        await Task.Run(() => WaitForSingleObject(_hProcess, 0xFFFFFFFF));
        Exited?.Invoke();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _cts.Cancel();
        _stdin?.Dispose();
        _stdout?.Dispose();
        _inputRead?.Dispose();
        _inputWrite?.Dispose();
        _outputRead?.Dispose();
        _outputWrite?.Dispose();
        if (_hPC != IntPtr.Zero) { ClosePseudoConsole(_hPC); _hPC = IntPtr.Zero; }
        if (_hProcess != IntPtr.Zero) { CloseHandle(_hProcess); _hProcess = IntPtr.Zero; }
    }
}

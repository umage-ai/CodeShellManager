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
public sealed class PseudoTerminal : IPseudoTerminal
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

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr CreateJobObject(IntPtr lpJobAttributes, string? lpName);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetInformationJobObject(IntPtr hJob, int JobObjectInfoClass,
        ref JOBOBJECT_EXTENDED_LIMIT_INFORMATION lpJobObjectInfo, int cbJobObjectInfoLength);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool AssignProcessToJobObject(IntPtr hJob, IntPtr hProcess);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern uint ResumeThread(IntPtr hThread);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetExitCodeProcess(IntPtr hProcess, out uint lpExitCode);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool DuplicateHandle(IntPtr hSourceProcessHandle, IntPtr hSourceHandle,
        IntPtr hTargetProcessHandle, out IntPtr lpTargetHandle, uint dwDesiredAccess,
        bool bInheritHandle, uint dwOptions);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetCurrentProcess();

    private const uint DUPLICATE_SAME_ACCESS = 0x00000002;

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

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_BASIC_LIMIT_INFORMATION
    {
        public long PerProcessUserTimeLimit;
        public long PerJobUserTimeLimit;
        public uint LimitFlags;
        public IntPtr MinimumWorkingSetSize;
        public IntPtr MaximumWorkingSetSize;
        public uint ActiveProcessLimit;
        public IntPtr Affinity;
        public uint PriorityClass;
        public uint SchedulingClass;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct IO_COUNTERS
    {
        public ulong ReadOperationCount, WriteOperationCount, OtherOperationCount;
        public ulong ReadTransferCount, WriteTransferCount, OtherTransferCount;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION
    {
        public JOBOBJECT_BASIC_LIMIT_INFORMATION BasicLimitInformation;
        public IO_COUNTERS IoInfo;
        public IntPtr ProcessMemoryLimit;
        public IntPtr JobMemoryLimit;
        public IntPtr PeakProcessMemoryUsed;
        public IntPtr PeakJobMemoryUsed;
    }

    private const uint EXTENDED_STARTUPINFO_PRESENT = 0x00080000;
    private const IntPtr PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE = (IntPtr)0x00020016;
    private const uint CREATE_SUSPENDED = 0x00000004;
    private const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000;
    private const int JobObjectExtendedLimitInformation = 9;

    // ── Fields ────────────────────────────────────────────────────────────────

    private IntPtr _hPC = IntPtr.Zero;
    private IntPtr _hProcess = IntPtr.Zero;
    private IntPtr _hJob = IntPtr.Zero;

    /// <summary>
    /// Process exit code, populated once <see cref="Exited"/> fires.
    /// Null while the process is still running. Uint cast to int (Windows exit codes can be negative).
    /// </summary>
    public int? ExitCode { get; private set; }
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

    // Resolved once per process. pwsh (PowerShell 7+) is preferred because that's
    // where modern users keep their profile functions — wrapping in legacy
    // powershell.exe (5.1) loads a different profile and won't see them.
    // Shared with RunInstance so both PowerShell-wrapping paths agree — see PwshLocator.
    internal static string BuildCmdLine(string command, string fullUserCmd)
        => BuildCmdLine(command, fullUserCmd, Services.PwshLocator.Executable);

    internal static string BuildCmdLine(string command, string fullUserCmd, string wrapperShell)
    {
        // Shells are passed through as-is (they initialize the console themselves).
        string exe = System.IO.Path.GetFileNameWithoutExtension(command).ToLowerInvariant();
        if (exe is "cmd" or "powershell" or "pwsh" or "wsl" or "bash" or "zsh" or "sh" or "ssh" or "nu" or "fish")
            return fullUserCmd;

        // Wrap in PowerShell so the shell sets up the Win32 console environment
        // before launching the target process (Electron/Node SEA apps like claude.exe
        // crash with STATUS_DLL_INIT_FAILED when launched directly inside a ConPTY).
        return $"{wrapperShell} -NoExit -Command {fullUserCmd}";
    }

    public void Start(string command, string args, string workingDirectory,
        int cols = 220, int rows = 50, bool useJobObject = false)
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

            // Build CreateProcess flags. When useJobObject=true we add CREATE_SUSPENDED so
            // we can attach the new process to the Job Object before it starts spawning children.
            uint creationFlags = EXTENDED_STARTUPINFO_PRESENT;
            if (useJobObject) creationFlags |= CREATE_SUSPENDED;

            Log($"CreateProcess cmdLine='{cmdLine}' workDir='{workDir}'");
            if (!CreateProcess(null, cmdLine, IntPtr.Zero, IntPtr.Zero, false,
                    creationFlags, IntPtr.Zero, workDir, ref si, out var pi))
                throw new InvalidOperationException($"CreateProcess failed: {Marshal.GetLastWin32Error()}");

            _hProcess = pi.hProcess;

            // Inner try/finally guarantees pi.hThread is closed even if any job-object
            // P/Invoke throws — otherwise we leak the thread handle on error paths.
            try
            {
                if (useJobObject)
                {
                    _hJob = CreateJobObject(IntPtr.Zero, null);
                    if (_hJob == IntPtr.Zero)
                        throw new InvalidOperationException(
                            $"CreateJobObject failed: {Marshal.GetLastWin32Error()}");

                    var limits = new JOBOBJECT_EXTENDED_LIMIT_INFORMATION
                    {
                        BasicLimitInformation = new JOBOBJECT_BASIC_LIMIT_INFORMATION
                        {
                            LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                        }
                    };
                    if (!SetInformationJobObject(_hJob, JobObjectExtendedLimitInformation,
                            ref limits, Marshal.SizeOf<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()))
                        throw new InvalidOperationException(
                            $"SetInformationJobObject failed: {Marshal.GetLastWin32Error()}");

                    if (!AssignProcessToJobObject(_hJob, _hProcess))
                        throw new InvalidOperationException(
                            $"AssignProcessToJobObject failed: {Marshal.GetLastWin32Error()}");

                    // Process was started suspended — resume it now that it's in the job.
                    ResumeThread(pi.hThread);
                }
            }
            finally
            {
                CloseHandle(pi.hThread);
            }
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

    /// <summary>
    /// Awaits a Win32 handle becoming signalled without occupying a thread while it waits.
    ///
    /// <see cref="ThreadPool.RegisterWaitForSingleObject"/> registers the handle with the
    /// OS wait infrastructure; the callback runs on a pool thread only once it signals.
    /// The registration is unregistered from inside the callback, which is the documented
    /// way to release it exactly once for a one-shot wait.
    /// </summary>
    private static Task WaitForHandleAsync(IntPtr handle)
    {
        var tcs = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var safe = new SafeWaitHandle(handle, ownsHandle: false);   // caller closes the handle
        var waitHandle = new ManualResetEvent(false) { SafeWaitHandle = safe };

        RegisteredWaitHandle? registration = null;
        registration = ThreadPool.RegisterWaitForSingleObject(
            waitHandle,
            (_, _) =>
            {
                // Unregister first so the entry is released even if a continuation throws.
                registration?.Unregister(null);
                waitHandle.Dispose();
                tcs.TrySetResult();
            },
            state: null,
            millisecondsTimeOutInterval: Timeout.Infinite,
            executeOnlyOnce: true);

        return tcs.Task;
    }

    private async Task MonitorExitAsync()
    {
        // Exited must fire on EVERY path, exactly once (issue #91).
        //
        // MainWindow.DisposeAndWaitForExitAsync waits on this event with a 10s timeout,
        // and the shutdown loop is sequential. A path that returns without firing costs
        // the full 10s for that session, every time, on top of every other session's.
        // The early return below used to do exactly that.
        try
        {
            // Duplicate _hProcess so Dispose() can close the original without racing the
            // wait. Closing a handle another thread is waiting on is Win32 UB — the wait
            // may return prematurely and we'd fire Exited before the child actually exits.
            if (!DuplicateHandle(GetCurrentProcess(), _hProcess, GetCurrentProcess(),
                    out IntPtr waitHandle, 0, false, DUPLICATE_SAME_ACCESS))
            {
                // Can't observe the real exit, so ExitCode stays null and the caller
                // treats it as unknown — but it must not be left waiting on an event
                // that will never arrive.
                Log($"DuplicateHandle failed: {Marshal.GetLastWin32Error()} — " +
                    "firing Exited without an exit code so shutdown doesn't stall");
                return;
            }

            try
            {
                // Wait WITHOUT holding a thread.
                //
                // This used to be `await Task.Run(() => WaitForSingleObject(h, INFINITE))`,
                // which parks one thread-pool thread per live PTY for the whole lifetime of
                // the session — plus one per run-command PTY. With ~25 sessions restoring,
                // that is ~25 permanently blocked pool threads, and the pool only injects
                // replacements at roughly one per second. Everything else queued behind it.
                //
                // Measured consequence: the Claude launch gate, itself moved onto the pool
                // in #107, waited up to 23998ms against a 2000ms cap — not because the gate
                // was slow but because it could not get a thread. 81s of a 135s restore.
                //
                // RegisterWaitForSingleObject hands the wait to the OS and calls back on a
                // pool thread only once the handle signals, so an idle PTY costs nothing.
                await WaitForHandleAsync(waitHandle).ConfigureAwait(false);
                if (GetExitCodeProcess(waitHandle, out uint code))
                    ExitCode = unchecked((int)code);
            }
            finally
            {
                CloseHandle(waitHandle);
            }
        }
        finally
        {
            Exited?.Invoke();
        }
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
        if (_hJob != IntPtr.Zero) { CloseHandle(_hJob); _hJob = IntPtr.Zero; }
        if (_hPC != IntPtr.Zero) { ClosePseudoConsole(_hPC); _hPC = IntPtr.Zero; }
        if (_hProcess != IntPtr.Zero) { CloseHandle(_hProcess); _hProcess = IntPtr.Zero; }
    }
}

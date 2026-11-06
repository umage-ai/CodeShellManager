using System;
using System.IO;
using System.Runtime.InteropServices;

namespace CodeShellManager.Terminal;

/// <summary>Which ConPTY implementation a pseudo console was created with.</summary>
public enum ConPtyKind
{
    /// <summary>The Windows inbox ConPTY (kernel32 → system conhost).</summary>
    Inbox,
    /// <summary>The bundled <c>conpty.dll</c> + <c>OpenConsole.exe</c> beside the exe.</summary>
    Bundled,
}

/// <summary>COORD, as the ConPTY functions take it (by value).</summary>
[StructLayout(LayoutKind.Sequential)]
internal struct ConPtyCoord
{
    public short X;
    public short Y;
}

/// <summary>
/// When repeated bundled CreatePseudoConsole failures switch the process to inbox: after
/// <see cref="FailuresBeforeDemotion"/> in a row; a success resets the count. One transient
/// failure (resource pressure, an antivirus scan racing OpenConsole.exe) should not cost the
/// rest of the run the bundled ConPTY. Not thread-safe — the caller holds its lock.
/// </summary>
internal sealed class BundledFailurePolicy
{
    public const int FailuresBeforeDemotion = 3;
    private int _consecutive;

    /// <summary>Records a failure; true when this one reaches the demotion threshold.</summary>
    public bool RecordFailure(out int consecutive)
    {
        consecutive = ++_consecutive;
        return consecutive >= FailuresBeforeDemotion;
    }

    public void RecordSuccess() => _consecutive = 0;
}

/// <summary>
/// WPF-free decision: should the bundled ConPTY be tried at all? Pure, so the rules are
/// unit-tested (<c>ConPtyApiTests</c>). Whether it then actually LOADS is a separate step.
/// </summary>
internal static class ConPtyResolver
{
    public const string DllName = "conpty.dll";
    public const string HostName = "OpenConsole.exe";

    public readonly record struct Decision(bool TryBundled, string Reason);

    public static Decision Decide(bool preferBundled, string baseDirectory, Func<string, bool> fileExists)
    {
        if (!preferBundled)
            return new(false, "disabled by setting UseBundledConPty");
        if (string.IsNullOrEmpty(baseDirectory))
            return new(false, "no application directory");
        // OpenConsole.exe is checked too: conpty.dll looks for it next to itself, and a
        // conpty.dll without it is not the implementation we measured.
        foreach (var name in new[] { DllName, HostName })
        {
            if (!fileExists(Path.Combine(baseDirectory, name)))
                return new(false, $"{name} not found in {baseDirectory}");
        }
        return new(true, $"found {DllName} + {HostName} in {baseDirectory}");
    }
}

/// <summary>
/// The three ConPTY entry points, bound either to the bundled <c>conpty.dll</c> or to the
/// inbox kernel32. Both are bound BY FULL PATH — never a bare <c>"conpty.dll"</c>, which
/// would let any same-named DLL earlier on the search path (the current directory, PATH)
/// be loaded into the process instead.
/// </summary>
internal sealed unsafe class ConPtyApi
{
    private readonly delegate* unmanaged<ConPtyCoord, IntPtr, IntPtr, uint, IntPtr*, int> _create;
    private readonly delegate* unmanaged<IntPtr, ConPtyCoord, int> _resize;
    private readonly delegate* unmanaged<IntPtr, void> _close;

    public ConPtyKind Kind { get; }

    private ConPtyApi(ConPtyKind kind, IntPtr create, IntPtr resize, IntPtr close)
    {
        Kind = kind;
        _create = (delegate* unmanaged<ConPtyCoord, IntPtr, IntPtr, uint, IntPtr*, int>)create;
        _resize = (delegate* unmanaged<IntPtr, ConPtyCoord, int>)resize;
        _close = (delegate* unmanaged<IntPtr, void>)close;
    }

    public int Create(ConPtyCoord size, SafeHandle input, SafeHandle output, uint flags, out IntPtr hpc)
    {
        IntPtr result = IntPtr.Zero;
        bool addIn = false, addOut = false;
        try
        {
            input.DangerousAddRef(ref addIn);
            output.DangerousAddRef(ref addOut);
            int hr = _create(size, input.DangerousGetHandle(), output.DangerousGetHandle(), flags, &result);
            hpc = result;
            return hr;
        }
        finally
        {
            if (addIn) input.DangerousRelease();
            if (addOut) output.DangerousRelease();
        }
    }

    public int Resize(IntPtr hpc, ConPtyCoord size) => _resize(hpc, size);

    public void Close(IntPtr hpc) => _close(hpc);

    // ── Binding ───────────────────────────────────────────────────────────────

    private static ConPtyApi? s_inbox;

    /// <summary>The inbox implementation, from System32's kernel32 by full path.</summary>
    public static ConPtyApi Inbox => s_inbox ??= BindInbox();

    private static ConPtyApi BindInbox()
    {
        IntPtr k32 = NativeLibrary.Load(Path.Combine(Environment.SystemDirectory, "kernel32.dll"));
        return new ConPtyApi(ConPtyKind.Inbox,
            NativeLibrary.GetExport(k32, "CreatePseudoConsole"),
            NativeLibrary.GetExport(k32, "ResizePseudoConsole"),
            NativeLibrary.GetExport(k32, "ClosePseudoConsole"));
    }

    /// <summary>
    /// Loads the bundled <c>conpty.dll</c> from <paramref name="fullPath"/> and binds its
    /// exports. Returns null, with a reason, on any failure — wrong architecture, a corrupt
    /// file, a missing export.
    /// </summary>
    public static ConPtyApi? TryBindBundled(string fullPath, out string reason)
    {
        if (!Path.IsPathFullyQualified(fullPath))
        {
            reason = $"refusing to load a relative path: {fullPath}";
            return null;
        }
        if (!NativeLibrary.TryLoad(fullPath, out IntPtr lib))
        {
            reason = $"could not load {fullPath}";
            return null;
        }
        if (!NativeLibrary.TryGetExport(lib, "CreatePseudoConsole", out var create)
            || !NativeLibrary.TryGetExport(lib, "ResizePseudoConsole", out var resize)
            || !NativeLibrary.TryGetExport(lib, "ClosePseudoConsole", out var close))
        {
            reason = $"{fullPath} lacks a ConPTY export";
            return null;
        }
        reason = $"loaded {fullPath}";
        return new ConPtyApi(ConPtyKind.Bundled, create, resize, close);
    }
}

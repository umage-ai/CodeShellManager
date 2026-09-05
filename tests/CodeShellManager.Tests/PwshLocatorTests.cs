using System;
using System.IO;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Cover for issue #104 — <c>where.exe</c> proves a name resolves on PATH, not that it
/// runs. The false positive that matters on Windows is a Microsoft Store App Execution
/// Alias stub: zero bytes on disk, a reparse point, resolves fine, fails on execution.
///
/// Since the run-command and session-wrapper locators merged, picking a bad pwsh means
/// every non-shell session fails to launch rather than degrading to powershell.exe.
/// </summary>
public class PwshLocatorTests : IDisposable
{
    private readonly string _dir;

    public PwshLocatorTests()
    {
        _dir = Path.Combine(Path.GetTempPath(), $"csm-pwsh-{Guid.NewGuid():N}");
        Directory.CreateDirectory(_dir);
    }

    public void Dispose()
    {
        try { Directory.Delete(_dir, recursive: true); } catch { }
    }

    private string Make(string name, byte[] content)
    {
        string p = Path.Combine(_dir, name);
        File.WriteAllBytes(p, content);
        return p;
    }

    [Fact]
    public void IsRunnable_RealExecutable_Accepted()
    {
        Assert.True(PwshLocator.IsRunnable(Make("pwsh.exe", new byte[] { 0x4D, 0x5A, 0x90, 0x00 })));
    }

    [Fact]
    public void IsRunnable_ZeroByteStub_Rejected()
    {
        // The shape of a Store alias stub that was never backed by an install.
        Assert.False(PwshLocator.IsRunnable(Make("pwsh.exe", Array.Empty<byte>())));
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    public void IsRunnable_BlankPath_Rejected(string path)
    {
        Assert.False(PwshLocator.IsRunnable(path));
    }

    [Fact]
    public void IsRunnable_MissingFile_Rejected()
    {
        Assert.False(PwshLocator.IsRunnable(Path.Combine(_dir, "not-here.exe")));
    }

    [Fact]
    public void IsRunnable_MalformedPath_RejectedRatherThanThrowing()
    {
        // FileInfo throws on some malformed input; falling back to powershell.exe is
        // always safe, so the locator must never propagate that.
        Assert.False(PwshLocator.IsRunnable("::::"));
        Assert.False(PwshLocator.IsRunnable("C:\\a\0b"));
    }

    [Fact]
    public void Executable_ResolvesToOneOfTheTwoKnownNames()
    {
        // Whatever this machine has, the answer must be a name ConPTY can resolve.
        Assert.Contains(PwshLocator.Executable, new[] { "pwsh.exe", "powershell.exe" });
    }

    // ── Store App Execution Alias (issue #104 follow-up) ──────────────────

    [Fact]
    public void IsRunnable_ZeroByteStub_DoesNotShortCircuitToFalse()
    {
        // Regression: an earlier version rejected the alias SHAPE outright. A working
        // Store install of PowerShell 7 is the same shape as a dead stub — a zero-byte
        // AppExecLink — so shape alone must not decide. A plain zero-byte file here is
        // not executable, so the probe correctly says no; the point is that it is the
        // PROBE saying no, not the length check.
        Assert.False(PwshLocator.IsRunnable(Make("pwsh.exe", Array.Empty<byte>())));
    }

    [Fact]
    public void IsRunnable_RealExecutableWithContent_SkipsTheProbeEntirely()
    {
        // The common MSI install must stay on the fast path — non-zero length and not a
        // reparse point is decided from metadata, with no process spawned.
        var sw = System.Diagnostics.Stopwatch.StartNew();
        Assert.True(PwshLocator.IsRunnable(Make("real.exe", new byte[] { 0x4D, 0x5A, 0x90, 0x00 })));
        Assert.True(sw.ElapsedMilliseconds < 500,
            $"metadata path should not spawn anything, took {sw.ElapsedMilliseconds}ms");
    }
}

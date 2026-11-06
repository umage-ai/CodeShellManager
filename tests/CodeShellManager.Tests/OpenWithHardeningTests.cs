using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading.Tasks;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// The review round on #178: what .NET really sends, name-check bypasses (8.3, renamed copy,
/// %SystemRoot%, network drive), probe ordering, ambiguous shims and import review text.
/// </summary>
public class OpenWithHardeningTests : IDisposable
{
    private readonly string _tmp = Path.Combine(Path.GetTempPath(), $"csm-owh-{Guid.NewGuid():N}");

    public OpenWithHardeningTests() => Directory.CreateDirectory(_tmp);

    public void Dispose()
    {
        try { Directory.Delete(_tmp, recursive: true); } catch { }
    }

    private string TempExe(string name = "Code.exe")
    {
        string p = Path.Combine(_tmp, name);
        File.WriteAllText(p, "");
        return p;
    }

    // ── What .NET actually puts on the command line ──────────────────────────────────────

    /// <summary>
    /// The launch uses <see cref="ProcessStartInfo.ArgumentList"/>, whose encoding is .NET's
    /// own (PasteArguments), not <see cref="ShellSession.QuoteForCmd"/>. So spawn a real child
    /// with exactly that ArgumentList and have it print its raw <c>GetCommandLineW</c>, then
    /// split that with the real <c>CommandLineToArgvW</c>. Windows PowerShell 5.1 is the echo
    /// helper (always present on Windows); with <c>-File</c> it never interprets our arguments
    /// as code, and the raw command line is read regardless of what it does with them.
    /// </summary>
    [Fact]
    public void ArgumentList_RealProcess_EachHostileFolderArrivesAsOneArgument()
    {
        string ps = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System),
            "WindowsPowerShell", "v1.0", "powershell.exe");
        if (!File.Exists(ps)) return;   // not Windows desktop — nothing to test against

        string script = Path.Combine(_tmp, "echo-cmdline.ps1");
        File.WriteAllText(script,
            "[Console]::OutputEncoding = [Text.Encoding]::UTF8\r\n[Console]::Out.Write([Environment]::CommandLine)\r\n",
            new UTF8Encoding(true));

        string[] folders =
        [
            @"C:\repos\plain",
            @"C:\repos\has space",
            @"C:\repos\a&calc",
            @"C:\repos\100%PATH%",
            @"C:\repos\x"" --new-window ""y",
            @"C:\repos\back\""slash",
            @"C:\repos\trailing\",
            @"C:\repos\trailing space\ ",
            @"C:\repos\$(id)`whoami`",
            @"C:\repos\caret^pipe|lt<gt>",
            @"C:\repos\uniçødé ✓",
        ];
        var entry = OpenWithDetector.VsCodeEntry("VS Code", @"C:\Program Files\Microsoft VS Code\Code.exe");
        var expected = new List<string>();
        foreach (var f in folders)
        {
            var (cmd, error) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Local, Folder: f));
            Assert.Null(error);
            expected.AddRange(cmd!.Arguments);
        }
        // And the WSL URI form, which carries a quote and spaces inside one token.
        var (wsl, _) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Wsl, Distro: "Ubuntu", LinuxPath: "/a \"b\" c"));
        expected.AddRange(wsl!.Arguments);

        var psi = new ProcessStartInfo(ps)
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true,
            StandardOutputEncoding = Encoding.UTF8,
        };
        foreach (var a in new[] { "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script })
            psi.ArgumentList.Add(a);
        foreach (var a in expected) psi.ArgumentList.Add(a);   // exactly how OpenWithCommand.ToStartInfo adds them

        using var p = Process.Start(psi)!;
        var stdout = p.StandardOutput.ReadToEndAsync();
        _ = p.StandardError.ReadToEndAsync();
        Assert.True(p.WaitForExit(30000), "echo helper timed out");
        string raw = stdout.Result;

        string[] argv = Win32CommandLineTests.Split(raw);   // argv[0] here = powershell.exe
        int scriptAt = Array.FindIndex(argv, a => string.Equals(a, script, StringComparison.OrdinalIgnoreCase));
        Assert.True(scriptAt > 0, "script path not found in: " + raw);
        Assert.Equal(expected, argv[(scriptAt + 1)..]);
    }

    // ── Executable: bypasses of a name-based check ───────────────────────────────────────

    [Theory]
    [InlineData(@"C:\Windows\System32\OpenSSH\ssh.exe")]
    [InlineData(@"C:\Windows\System32\wbem\WMIC.exe")]
    [InlineData(@"C:\Windows\System32\pcalua.exe")]
    [InlineData(@"C:\Windows\System32\mmc.exe")]
    [InlineData(@"C:\Windows\hh.exe")]
    [InlineData(@"C:\Windows\explorer.exe")]
    [InlineData(@"C:\Windows\Microsoft.NET\Framework64\v4.0.30319\MSBuild.exe")]
    [InlineData(@"C:\Windows\Microsoft.NET\Framework64\v4.0.30319\InstallUtil.exe")]
    [InlineData(@"C:\Windows\System32\odbcconf.exe")]
    [InlineData(@"c:\WINDOWS\system32\notepad.exe")]
    [InlineData(@"C:\Tools\..\Windows\System32\anything.exe")]
    public void SystemRootPrograms_AreRefused(string path)
    {
        string? saved = OpenWithService.SystemRoot;
        try
        {
            OpenWithService.SystemRoot = @"C:\Windows";
            Assert.NotNull(OpenWithService.ValidateExecutable(path, allowBatch: true));
        }
        finally { OpenWithService.SystemRoot = saved!; }
    }

    [Theory]
    [InlineData(@"C:\Tools\ssh.exe")]
    [InlineData(@"C:\Tools\wmic.exe")]
    [InlineData(@"C:\Tools\explorer.exe")]
    [InlineData(@"C:\Tools\MSBuild.exe")]
    [InlineData(@"C:\Tools\node.exe")]
    public void CommandRunningToolsOutsideSystemRoot_AreRefusedByName(string path) =>
        Assert.NotNull(OpenWithService.ValidateExecutable(path, allowBatch: true));

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetShortPathNameW(string longPath, StringBuilder shortPath, uint size);

    [Fact]
    public void ShortName_OfAShell_IsCaughtOnDisk()
    {
        string dir = Directory.CreateDirectory(Path.Combine(_tmp, "A Long Folder Name")).FullName;
        string longPath = Path.Combine(dir, "powershell.exe");
        File.WriteAllText(longPath, "");
        var sb = new StringBuilder(1024);
        uint n = GetShortPathNameW(longPath, sb, (uint)sb.Capacity);
        string shortPath = sb.ToString(0, (int)n);
        if (n == 0 || Path.GetFileName(shortPath).Equals("powershell.exe", StringComparison.OrdinalIgnoreCase))
            return;   // 8.3 name generation is off on this volume — the bypass can't exist here

        // The pure, name-only check is fooled by POWERS~1.EXE — that is the bypass…
        Assert.Null(OpenWithService.ValidateExecutable(shortPath, allowBatch: false));
        // …and the on-disk check resolves the real name first.
        Assert.NotNull(OpenWithService.ValidateExecutableOnDisk(shortPath, allowBatch: false));
    }

    [Fact]
    public void RenamedCopyOfAShell_IsCaughtByItsVersionInfo()
    {
        string ps = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System),
            "WindowsPowerShell", "v1.0", "powershell.exe");
        if (!File.Exists(ps)) return;
        string copy = Path.Combine(_tmp, "editor.exe");
        File.Copy(ps, copy);
        Assert.Null(OpenWithService.ValidateExecutable(copy, allowBatch: false));
        Assert.Contains("copy of", OpenWithService.ValidateExecutableOnDisk(copy, allowBatch: false));
    }

    [Fact]
    public void NetworkDrive_IsRefused()
    {
        Assert.Contains("network", OpenWithService.ValidateExecutableOnDisk(
            @"Z:\tools\editor.exe", allowBatch: false, driveTypeOf: _ => DriveType.Network));
    }

    [Fact]
    public void LocalProgram_PassesOnDisk()
    {
        Assert.Null(OpenWithService.ValidateExecutableOnDisk(TempExe(), allowBatch: false));
    }

    [Fact]
    public async Task Prepare_RefusesNetworkDriveProgram_BeforeAnythingElse()
    {
        // The launch path runs the on-disk check too, not only import.
        var entry = OpenWithDetector.VsCodeEntry("VS Code", @"\\server\share\Code.exe");
        var (cmd, error) = await OpenWithService.PrepareAsync(entry, new OpenWithContext(SessionKind.Local, Folder: _tmp));
        Assert.Null(cmd);
        Assert.Contains("Network", error);
    }

    // ── PrepareAsync: validate before probing WSL ────────────────────────────────────────

    [Theory]
    [InlineData(@"C:\Windows\System32\cmd.exe", "Ubuntu")]   // refused program
    [InlineData(null, "-x")]                                  // option-shaped distro
    [InlineData(null, "bad distro")]                          // whitespace in distro
    public async Task Prepare_InvalidCommand_NeverProbesWsl(string? exe, string distro)
    {
        int probes = 0;
        var entry = OpenWithDetector.VsCodeEntry("VS Code", exe ?? TempExe());
        var (cmd, _) = await OpenWithService.PrepareAsync(entry,
            new OpenWithContext(SessionKind.Wsl, Distro: distro, LinuxPath: ""),
            (_, _) => { probes++; return Task.FromResult<string?>("/home/u"); });
        Assert.Null(cmd);
        Assert.Equal(0, probes);
    }

    [Fact]
    public async Task Prepare_MissingProgram_NeverProbesWsl()
    {
        int probes = 0;
        var entry = OpenWithDetector.VsCodeEntry("VS Code", Path.Combine(_tmp, "absent", "Code.exe"));
        await OpenWithService.PrepareAsync(entry, new OpenWithContext(SessionKind.Wsl, Distro: "Ubuntu"),
            (_, _) => { probes++; return Task.FromResult<string?>("/home/u"); });
        Assert.Equal(0, probes);
    }

    [Theory]
    [InlineData("", "vscode-remote://wsl+Ubuntu/home/u")]
    [InlineData("~", "vscode-remote://wsl+Ubuntu/home/u")]
    [InlineData("~/my proj", "vscode-remote://wsl+Ubuntu/home/u/my%20proj")]
    public async Task Prepare_ValidCommand_ProbesHomeOnce(string linuxPath, string uri)
    {
        int probes = 0;
        var entry = OpenWithDetector.VsCodeEntry("VS Code", TempExe());
        var (cmd, error) = await OpenWithService.PrepareAsync(entry,
            new OpenWithContext(SessionKind.Wsl, Distro: "Ubuntu", LinuxPath: linuxPath),
            (_, _) => { probes++; return Task.FromResult<string?>("/home/u"); });
        Assert.Null(error);
        Assert.Equal(1, probes);
        Assert.Equal(new[] { "--folder-uri", uri }, cmd!.Arguments);
    }

    [Fact]
    public async Task Prepare_HomeProbeFails_IsAClearError()
    {
        var entry = OpenWithDetector.VsCodeEntry("VS Code", TempExe());
        var (cmd, error) = await OpenWithService.PrepareAsync(entry,
            new OpenWithContext(SessionKind.Wsl, Distro: "Ubuntu"),
            (_, _) => Task.FromResult<string?>(null));
        Assert.Null(cmd);
        Assert.Contains("Linux folder", error);
    }

    // ── Shims: one unambiguous candidate or nothing ──────────────────────────────────────

    [Fact]
    public void ResolveShim_TwoCandidates_IsAmbiguous()
    {
        string a = TempExe("rider64.exe"), b = TempExe("other.exe");
        string script = Path.Combine(_tmp, "rider.cmd");
        File.WriteAllText(script, $"@echo off\r\nif exist \"{b}\" start \"\" \"{b}\" %*\r\nstart \"\" \"{a}\" %*\r\n");
        Assert.Null(OpenWithService.ResolveShim(script));
    }

    [Fact]
    public void ResolveShim_SameCandidateTwice_IsStillOne()
    {
        string a = TempExe("rider64.exe");
        string script = Path.Combine(_tmp, "rider.cmd");
        File.WriteAllText(script, $"@echo off\r\nif exist \"{a}\" start \"\" \"{a}\" %*\r\n");
        Assert.Equal(a, OpenWithService.ResolveShim(script));
    }

    // ── Import review ────────────────────────────────────────────────────────────────────

    [Fact]
    public void DescribeForReview_ShowsProgramAndEveryTemplate()
    {
        var list = new List<OpenWithEntry>
        {
            new() { Label = "VS Code", Executable = @"C:\Tools\evil.exe", LocalArgs = "--run {folder}", SshArgs = "x {host}", IsDefault = true },
        };
        string text = OpenWithService.DescribeForReview(list);
        Assert.Contains("★ VS Code", text);
        Assert.Contains(@"local: C:\Tools\evil.exe --run {folder}", text);
        Assert.Contains(@"SSH: C:\Tools\evil.exe x {host}", text);
        Assert.DoesNotContain("WSL", text);
    }

    [Fact]
    public void SameCommands_DetectsAnyLaunchRelevantChange()
    {
        var a = new List<OpenWithEntry> { OpenWithDetector.VsCodeEntry("VS Code", @"C:\a\Code.exe") };
        var b = new List<OpenWithEntry> { OpenWithDetector.VsCodeEntry("VS Code", @"C:\a\Code.exe") };
        Assert.True(OpenWithService.SameCommands(a, b));
        b[0].LocalArgs = "--x {folder}";
        Assert.False(OpenWithService.SameCommands(a, b));
    }

    [Fact]
    public void Describe_ShowsResolvedCommandForTheSession()
    {
        var e = new OpenWithEntry { Label = "VS Code", Executable = @"C:\Tools\evil.exe", LocalArgs = "--run {folder}" };
        Assert.Equal(@"C:\Tools\evil.exe --run C:\repo",
            OpenWithService.Describe(e, new OpenWithContext(SessionKind.Local, Folder: @"C:\repo")));
    }
}

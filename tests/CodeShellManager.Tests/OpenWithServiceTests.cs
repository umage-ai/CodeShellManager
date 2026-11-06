using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// "Open with…" (issue #178): template parsing, substitution, the never-a-shell rules and
/// import trust. The injection cases round-trip the resolved argv through the real
/// <c>CommandLineToArgvW</c>, like <see cref="GitServiceInjectionTests"/>, to prove each
/// hostile value arrives as exactly one argument.
/// </summary>
public class OpenWithServiceTests : IDisposable
{
    private const string Exe = @"C:\Program Files\Microsoft VS Code\Code.exe";
    private readonly string _tmp = Path.Combine(Path.GetTempPath(), $"csm-openwith-{Guid.NewGuid():N}");

    public OpenWithServiceTests() => Directory.CreateDirectory(_tmp);

    public void Dispose()
    {
        try { Directory.Delete(_tmp, recursive: true); } catch { }
    }

    private static OpenWithEntry VsCode(string exe = Exe) => OpenWithDetector.VsCodeEntry("VS Code", exe);

    private static string[] RoundTrip(OpenWithCommand cmd) =>
        Win32CommandLineTests.Split(string.Join(" ", cmd.Arguments.Select(a => ShellSession.QuoteForCmd(a))));

    // ── Substitution: one placeholder = one argv element ─────────────────────────────────

    [Theory]
    [InlineData(@"C:\repos\plain")]
    [InlineData(@"C:\repos\has space")]
    [InlineData(@"C:\repos\a&calc")]
    [InlineData(@"C:\repos\100%PATH%")]
    [InlineData(@"C:\repos\x"" --new-window ""y")]
    [InlineData(@"C:\repos\semi;colon,comma")]
    [InlineData(@"C:\repos\back\""slash")]
    [InlineData(@"C:\repos\trailing\")]
    [InlineData(@"C:\repos\$(id)`whoami`")]
    [InlineData(@"C:\repos\caret^pipe|lt<gt>")]
    [InlineData(@"C:\repos\uniçødé ✓")]
    public void LocalFolder_IsExactlyOneArgument(string folder)
    {
        var (cmd, error) = OpenWithService.Build(VsCode(), new OpenWithContext(SessionKind.Local, Folder: folder));
        Assert.Null(error);
        Assert.Equal(new[] { folder }, cmd!.Arguments);
        Assert.Equal(new[] { folder }, RoundTrip(cmd));
    }

    [Fact]
    public void Placeholder_InsideAToken_StaysInsideThatToken()
    {
        var entry = new OpenWithEntry { Executable = Exe, LocalArgs = "--folder-uri=file:///{folder} --reuse-window" };
        var (cmd, _) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Local, Folder: @"C:\a b"));
        Assert.Equal(new[] { @"--folder-uri=file:///C:\a b", "--reuse-window" }, cmd!.Arguments);
        Assert.Equal(cmd.Arguments, RoundTrip(cmd));
    }

    [Theory]
    [InlineData("/home/alice/proj", "vscode-remote://wsl+Ubuntu/home/alice/proj")]
    [InlineData("/home/alice/my proj", "vscode-remote://wsl+Ubuntu/home/alice/my%20proj")]
    [InlineData("/home/alice/$(id)", "vscode-remote://wsl+Ubuntu/home/alice/%24%28id%29")]
    [InlineData("/home/a/x\" --disable-extensions \"y", "vscode-remote://wsl+Ubuntu/home/a/x%22%20--disable-extensions%20%22y")]
    [InlineData("/home/a/#tag?q=1&100%", "vscode-remote://wsl+Ubuntu/home/a/%23tag%3Fq%3D1%26100%25")]
    [InlineData("/home/a/ünï", "vscode-remote://wsl+Ubuntu/home/a/%C3%BCn%C3%AF")]
    public void Wsl_VsCodeFolderUri_IsOneEncodedArgument(string linuxPath, string uri)
    {
        var ctx = new OpenWithContext(SessionKind.Wsl, Folder: @"\\wsl$\Ubuntu\home", Distro: "Ubuntu", LinuxPath: linuxPath);
        var (cmd, error) = OpenWithService.Build(VsCode(), ctx);
        Assert.Null(error);
        var expected = new[] { "--folder-uri", uri };
        Assert.Equal(expected, cmd!.Arguments);
        Assert.Equal(expected, RoundTrip(cmd));
        // What VS Code's URI.parse would recover: the original path, intact.
        Assert.Equal(linuxPath, Uri.UnescapeDataString(uri["vscode-remote://wsl+Ubuntu".Length..]));
    }

    [Fact]
    public void Wsl_DistroInTheAuthority()
    {
        var ctx = new OpenWithContext(SessionKind.Wsl, Distro: "Ubuntu-22.04", LinuxPath: "/srv");
        var (cmd, _) = OpenWithService.Build(VsCode(), ctx);
        Assert.Equal("vscode-remote://wsl+Ubuntu-22.04/srv", cmd!.Arguments[1]);
    }

    [Fact]
    public void Ssh_UserAtHost_IsPercentEncodedInTheAuthority()
    {
        // '@' would otherwise be URI user-info; VS Code decodes %40 back to "alice@box".
        var ctx = new OpenWithContext(SessionKind.Ssh, User: "alice", Host: "box", RemoteFolder: "/srv/my app");
        var (cmd, _) = OpenWithService.Build(VsCode(), ctx);
        Assert.Equal(new[] { "--folder-uri", "vscode-remote://ssh-remote+alice%40box/srv/my%20app" }, cmd!.Arguments);
    }

    [Fact]
    public void Ssh_HostOnly()
    {
        var ctx = new OpenWithContext(SessionKind.Ssh, Host: "box", RemoteFolder: "/srv");
        var (cmd, _) = OpenWithService.Build(VsCode(), ctx);
        Assert.Equal(new[] { "--folder-uri", "vscode-remote://ssh-remote+box/srv" }, cmd!.Arguments);
    }

    [Fact]
    public void Ssh_NoRemoteFolder_IsAClearErrorNotADifferentTarget()
    {
        var (cmd, error) = OpenWithService.Build(VsCode(), new OpenWithContext(SessionKind.Ssh, Host: "box"));
        Assert.Null(cmd);
        Assert.Contains("remote folder", error);
    }

    [Fact]
    public void EmptyStandalonePlaceholderToken_IsDropped()
    {
        var entry = new OpenWithEntry { Executable = Exe, SshArgs = "--host {host} {remoteFolder}" };
        var (cmd, _) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Ssh, Host: "box"));
        Assert.Equal(new[] { "--host", "box" }, cmd!.Arguments);
    }

    // ── '/' and '-' at the start of an argument (Chromium reads /name=value as a switch) ──

    [Theory]
    [InlineData("{linuxPath}")]
    [InlineData("--remote wsl+{distro} {linuxPath}")]          // the pre-review seeded form
    [InlineData("{linuxPath:uri}")]
    public void SlashLeadingValue_StartingAnArgument_IsRefused(string template)
    {
        var entry = new OpenWithEntry { Executable = Exe, WslArgs = template };
        var (cmd, error) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Wsl, Distro: "Ubuntu", LinuxPath: "/js-flags=--x"));
        Assert.Null(cmd);
        Assert.Contains("'/'", error);
    }

    [Fact]
    public void SlashLeadingValue_InsideAnArgument_IsFine()
    {
        var entry = new OpenWithEntry { Executable = Exe, SshArgs = "--path={remoteFolder}" };
        var (cmd, _) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Ssh, Host: "h", RemoteFolder: "/srv"));
        Assert.Equal(new[] { "--path=/srv" }, cmd!.Arguments);
    }

    [Fact]
    public void Ssh_PortPlaceholder()
    {
        var entry = new OpenWithEntry { Executable = Exe, SshArgs = "--host {host} --port {port}" };
        var (cmd, _) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Ssh, Host: "h", Port: 2222));
        Assert.Equal(new[] { "--host", "h", "--port", "2222" }, cmd!.Arguments);
    }

    [Fact]
    public void QuotedTemplateToken_KeepsSpaces()
    {
        var entry = new OpenWithEntry { Executable = Exe, LocalArgs = "\"--title=My Editor\" {folder}" };
        var (cmd, _) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Local, Folder: @"C:\x"));
        Assert.Equal(new[] { "--title=My Editor", @"C:\x" }, cmd!.Arguments);
    }

    // ── Value rules ──────────────────────────────────────────────────────────────────────

    [Theory]
    [InlineData(SessionKind.Ssh, "-oProxyCommand=calc", "")]
    [InlineData(SessionKind.Ssh, "box", "--help")]
    [InlineData(SessionKind.Wsl, "-x", "")]
    public void OptionShapedValues_AreRefused(SessionKind kind, string first, string second)
    {
        var ctx = kind == SessionKind.Ssh
            ? new OpenWithContext(kind, Host: first, RemoteFolder: second)
            : new OpenWithContext(kind, Distro: first, LinuxPath: "/home");
        var (cmd, error) = OpenWithService.Build(VsCode(), ctx);
        Assert.Null(cmd);
        Assert.Contains("'-'", error);
    }

    [Theory]
    [InlineData("bad host")]
    [InlineData("host\nx")]
    [InlineData("host\0")]
    public void HostWithWhitespaceOrControlChars_IsRefused(string host)
    {
        var (cmd, error) = OpenWithService.Build(VsCode(), new OpenWithContext(SessionKind.Ssh, Host: host));
        Assert.Null(cmd);
        Assert.NotNull(error);
    }

    [Theory]
    [InlineData(@"relative\path")]
    [InlineData(@"C:\a\r\nb" + "\n")]
    public void LocalFolder_MustBeFullPathWithoutControlChars(string folder)
    {
        var (cmd, error) = OpenWithService.Build(VsCode(), new OpenWithContext(SessionKind.Local, Folder: folder));
        Assert.Null(cmd);
        Assert.NotNull(error);
    }

    [Theory]
    [InlineData(0)]
    [InlineData(70000)]
    public void PortOutOfRange_IsRefused(int port)
    {
        var entry = new OpenWithEntry { Executable = Exe, SshArgs = "{host}:{port}" };
        var (cmd, _) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Ssh, Host: "h", Port: port));
        Assert.Null(cmd);
    }

    // ── Templates ────────────────────────────────────────────────────────────────────────

    [Theory]
    [InlineData("{folder}", new[] { "{folder}" })]
    [InlineData("  a   b  ", new[] { "a", "b" })]
    [InlineData("\"a b\" c", new[] { "a b", "c" })]
    [InlineData("x\"y z\"w", new[] { "xy zw" })]
    [InlineData("\"\"", new[] { "" })]
    [InlineData("", new string[0])]
    public void Tokenize(string template, string[] expected) =>
        Assert.Equal(expected, OpenWithService.Tokenize(template));

    [Fact]
    public void Tokenize_UnbalancedQuote_IsNull() => Assert.Null(OpenWithService.Tokenize("a \"b"));

    [Theory]
    [InlineData("{host}", SessionKind.Local)]
    [InlineData("{linuxPath}", SessionKind.Local)]
    [InlineData("{folder}", SessionKind.Ssh)]
    [InlineData("{bogus}", SessionKind.Wsl)]
    [InlineData("\"{folder}", SessionKind.Local)]
    [InlineData("--literal-only", SessionKind.Local)]         // must open the session's location
    [InlineData("/c calc", SessionKind.Local)]
    public void InvalidTemplates_AreReported(string template, SessionKind kind) =>
        Assert.NotNull(OpenWithService.ValidateTemplate(template, kind));

    [Theory]
    [InlineData("{folder}", SessionKind.Local)]
    [InlineData("--remote wsl+{distro} {linuxPath}", SessionKind.Wsl)]
    [InlineData("{folder}", SessionKind.Wsl)]
    [InlineData("--remote ssh-remote+{userAtHost} {remoteFolder}", SessionKind.Ssh)]
    [InlineData("", SessionKind.Ssh)]
    [InlineData("--folder-uri vscode-remote://wsl+{distro:uri}{linuxPath:uri}", SessionKind.Wsl)]
    public void ValidTemplates_Pass(string template, SessionKind kind) =>
        Assert.Null(OpenWithService.ValidateTemplate(template, kind));

    [Fact]
    public void Build_KindWithoutTemplate_IsAnError()
    {
        var entry = new OpenWithEntry { Label = "VS", Executable = Exe, LocalArgs = "{folder}" };
        var (cmd, error) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Ssh, Host: "h"));
        Assert.Null(cmd);
        Assert.Contains("SSH", error);
    }

    // ── Executable ───────────────────────────────────────────────────────────────────────

    [Theory]
    [InlineData(@"C:\Tools\editor.exe", false, true)]
    [InlineData(@"C:\Tools\EDITOR.EXE", false, true)]
    [InlineData(@"editor.exe", false, false)]                     // relative — would resolve via PATH/cwd
    [InlineData(@"C:editor.exe", false, false)]                   // drive-relative
    [InlineData(@"\\attacker\share\editor.exe", false, false)]    // network
    [InlineData(@"\\?\C:\Tools\editor.exe", false, false)]        // device path
    [InlineData(@"C:\Tools\script.ps1", false, false)]
    [InlineData(@"C:\Tools\thing.com", false, false)]
    [InlineData(@"C:\Tools\launch.cmd", false, false)]
    [InlineData(@"C:\Tools\launch.cmd", true, true)]
    [InlineData(@"C:\Tools\launch.bat", true, true)]
    [InlineData(@"C:\Windows\System32\cmd.exe", true, false)]
    [InlineData(@"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", true, false)]
    [InlineData(@"C:\Program Files\PowerShell\7\pwsh.exe", true, false)]
    [InlineData(@"C:\Windows\System32\wsl.exe", true, false)]
    [InlineData(@"C:\Windows\System32\mshta.exe", true, false)]
    [InlineData(@"C:\Windows\System32\rundll32.exe", true, false)]
    [InlineData(@"C:\Windows\System32\cmd.cmd", true, false)]
    [InlineData("", true, false)]
    public void ValidateExecutable(string path, bool allowBatch, bool ok) =>
        Assert.Equal(ok, OpenWithService.ValidateExecutable(path, allowBatch) == null);

    // ── BatBadBut ────────────────────────────────────────────────────────────────────────

    [Theory]
    [InlineData(@"C:\repos\a&calc")]
    [InlineData(@"C:\repos\100%PATH%")]
    [InlineData(@"C:\repos\bang!")]
    [InlineData(@"C:\repos\x""y")]
    [InlineData(@"C:\repos\a|b")]
    [InlineData(@"C:\repos\a^b")]
    [InlineData(@"C:\repos\(a)")]
    [InlineData(@"C:\repos\a,b")]
    [InlineData(@"C:\repos\a;b")]
    [InlineData(@"C:\repos\a=b")]
    [InlineData(@"C:\repos\a<b>")]
    public void BatchTarget_RefusesCmdMetacharacters(string folder)
    {
        var entry = new OpenWithEntry { Label = "shim", Executable = @"C:\Tools\code.cmd", LocalArgs = "{folder}" };
        var (cmd, error) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Local, Folder: folder));
        Assert.Null(cmd);
        Assert.Contains("cmd.exe", error);
    }

    [Fact]
    public void BatchTarget_PlainFolderIsAllowed()
    {
        var entry = new OpenWithEntry { Label = "shim", Executable = @"C:\Tools\code.cmd", LocalArgs = "{folder}" };
        var (cmd, error) = OpenWithService.Build(entry, new OpenWithContext(SessionKind.Local, Folder: @"C:\repos\my project"));
        Assert.Null(error);
        Assert.Equal(new[] { @"C:\repos\my project" }, cmd!.Arguments);
    }

    [Fact]
    public void ExeTarget_DoesNotApplyBatchRestriction()
    {
        var (cmd, _) = OpenWithService.Build(VsCode(), new OpenWithContext(SessionKind.Local, Folder: @"C:\repos\a&b%c%"));
        Assert.NotNull(cmd);
    }

    [Fact]
    public void StartInfo_NeverUsesTheShell_AndPassesArgvVerbatim()
    {
        var (cmd, _) = OpenWithService.Build(VsCode(), new OpenWithContext(SessionKind.Local, Folder: @"C:\a b&c"));
        var psi = cmd!.ToStartInfo();
        Assert.False(psi.UseShellExecute);
        Assert.Equal(Exe, psi.FileName);
        Assert.Equal("", psi.Arguments);
        Assert.Equal(new[] { @"C:\a b&c" }, psi.ArgumentList);
    }

    // ── Selection ────────────────────────────────────────────────────────────────────────

    [Fact]
    public void DefaultFor_FallsBackToFirstApplicableEntry()
    {
        var vs = new OpenWithEntry { Label = "VS", Executable = Exe, LocalArgs = "{folder}", IsDefault = true };
        var code = VsCode();
        var list = new List<OpenWithEntry> { vs, code };
        Assert.Same(vs, OpenWithService.DefaultFor(list, SessionKind.Local));
        Assert.Same(code, OpenWithService.DefaultFor(list, SessionKind.Wsl));
        Assert.Equal(new[] { code }, OpenWithService.ApplicableTo(list, SessionKind.Ssh));
        Assert.Null(OpenWithService.DefaultFor([], SessionKind.Local));
    }

    [Fact]
    public void Context_FromSession_Wsl()
    {
        var s = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Ubuntu", WslWorkingFolder = "/home/a", WorkingFolder = @"\\wsl$\Ubuntu\home\a" };
        var ctx = OpenWithContext.FromSession(s);
        Assert.True(ctx.HasTarget);
        Assert.Equal("Ubuntu", ctx.Distro);
        Assert.Equal("/home/a", ctx.LinuxPath);
        Assert.False(OpenWithContext.FromSession(new ShellSession { Kind = SessionKind.Ssh }).HasTarget);
    }

    // ── Sanitize / import trust ──────────────────────────────────────────────────────────

    [Fact]
    public void Sanitize_DropsShellsRelativeNetworkAndOnImportBatch()
    {
        var list = new List<OpenWithEntry?>
        {
            null,
            new() { Label = "ok", Executable = @"C:\Tools\ed.exe", LocalArgs = "{folder}" },
            new() { Label = "shell", Executable = @"C:\Windows\System32\cmd.exe", LocalArgs = "/c calc" },
            new() { Label = "rel", Executable = "calc.exe", LocalArgs = "{folder}" },
            new() { Label = "net", Executable = @"\\evil\x\ed.exe", LocalArgs = "{folder}" },
            new() { Label = "bat", Executable = @"C:\Tools\ed.cmd", LocalArgs = "{folder}" },
        };
        Assert.Equal(new[] { "ok" }, OpenWithService.Sanitize(list, allowBatch: false).Select(e => e.Label));
        Assert.Equal(new[] { "ok", "bat" }, OpenWithService.Sanitize(list, allowBatch: true).Select(e => e.Label));
    }

    [Fact]
    public void Sanitize_ClearsBadTemplates_DropsEntriesLeftWithNone_FixesIdsAndDefault()
    {
        var list = new List<OpenWithEntry?>
        {
            new() { Id = "x", Label = " A ", Executable = @"C:\a.exe", LocalArgs = "{folder}", SshArgs = "{folder}" },
            new() { Id = "x", Label = "", Executable = @"C:\b.exe", WslArgs = "{distro}", IsDefault = true },
            new() { Id = "y", Label = "C", Executable = @"C:\c.exe", LocalArgs = "{nope}" },
            new() { Id = "z", Label = "D", Executable = @"C:\d.exe", IsDefault = true, LocalArgs = "{folder}" },
        };
        var result = OpenWithService.Sanitize(list, allowBatch: false);
        Assert.Equal(new[] { "A", "b", "D" }, result.Select(e => e.Label));
        Assert.Null(result[0].SshArgs);
        Assert.Equal("{folder}", result[0].LocalArgs);
        Assert.Equal(3, result.Select(e => e.Id).Distinct().Count());
        Assert.Single(result, e => e.IsDefault);
        Assert.True(result[1].IsDefault);
    }

    [Fact]
    public void Sanitize_CapsEntries()
    {
        var list = Enumerable.Range(0, 80).Select(i => (OpenWithEntry?)new OpenWithEntry
        { Label = $"e{i}", Executable = $@"C:\e{i}.exe", LocalArgs = "{folder}" });
        Assert.Equal(OpenWithService.MaxEntries, OpenWithService.Sanitize(list, true).Count);
    }

    [Fact]
    public async Task Import_DropsBatchAndShellEntries()
    {
        var state = new AppState();
        state.Settings.OpenWithSeeded = true;
        state.Settings.OpenWith =
        [
            VsCode(),
            new OpenWithEntry { Label = "shim", Executable = @"C:\Tools\code.cmd", LocalArgs = "{folder}" },
            new OpenWithEntry { Label = "evil", Executable = @"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe", LocalArgs = "-c calc" },
        ];
        string path = Path.Combine(_tmp, "backup.csm");
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(state));

        var imported = await ImportExportService.ImportAsync(path);

        Assert.NotNull(imported);
        Assert.Equal(new[] { "VS Code" }, imported!.Settings.OpenWith.Select(e => e.Label));
        Assert.True(imported.Settings.OpenWithSeeded);
    }

    [Fact]
    public async Task Import_ExplicitNullList_BecomesEmpty()
    {
        string path = Path.Combine(_tmp, "null.csm");
        await File.WriteAllTextAsync(path, "{\"Settings\":{\"OpenWith\":null,\"OpenWithSeeded\":true}}");
        var imported = await ImportExportService.ImportAsync(path);
        Assert.NotNull(imported!.Settings.OpenWith);
        Assert.Empty(imported.Settings.OpenWith);
    }

    [Fact]
    public void StateNormalize_KeepsUserChosenBatch_ButDropsShells()
    {
        var state = new AppState();
        state.Settings.OpenWith =
        [
            new OpenWithEntry { Label = "shim", Executable = @"C:\Tools\code.cmd", LocalArgs = "{folder}" },
            new OpenWithEntry { Label = "evil", Executable = @"C:\Windows\System32\cmd.exe", LocalArgs = "/c {folder}" },
        ];
        StateService.Normalize(state);
        Assert.Equal(new[] { "shim" }, state.Settings.OpenWith.Select(e => e.Label));
    }

    [Fact]
    public void OldState_HasNoListAndIsNotSeeded()
    {
        var state = JsonSerializer.Deserialize<AppState>("{\"Settings\":{\"DefaultCommand\":\"claude\"}}")!;
        StateService.Normalize(state);
        Assert.Empty(state.Settings.OpenWith);
        Assert.False(state.Settings.OpenWithSeeded);
    }

    // ── Prepare (disk checks) ────────────────────────────────────────────────────────────

    [Fact]
    public async Task Prepare_MissingFolder_IsRefused()
    {
        string exe = Path.Combine(_tmp, "Code.exe");
        File.WriteAllText(exe, "");
        var ctx = new OpenWithContext(SessionKind.Local, Folder: Path.Combine(_tmp, "nope"));
        var (cmd, error) = await OpenWithService.PrepareAsync(VsCode(exe), ctx);
        Assert.Null(cmd);
        Assert.Contains("doesn't exist", error);
    }

    [Fact]
    public async Task Prepare_FolderThatIsAFile_IsRefused()
    {
        string file = Path.Combine(_tmp, "evil.exe");
        File.WriteAllText(file, "");
        var (cmd, _) = await OpenWithService.PrepareAsync(VsCode(), new OpenWithContext(SessionKind.Local, Folder: file));
        Assert.Null(cmd);
    }

    [Fact]
    public async Task Prepare_MissingProgram_IsReported()
    {
        var entry = VsCode(Path.Combine(_tmp, "missing", "Code.exe"));
        var (cmd, error) = await OpenWithService.PrepareAsync(entry, new OpenWithContext(SessionKind.Local, Folder: _tmp));
        Assert.Null(cmd);
        Assert.Contains("not found", error);
    }

    [Fact]
    public async Task Prepare_NormalizesForwardSlashFolder()
    {
        string exe = Path.Combine(_tmp, "Code.exe");
        File.WriteAllText(exe, "");
        string sub = Directory.CreateDirectory(Path.Combine(_tmp, "wt feat")).FullName;
        var (cmd, error) = await OpenWithService.PrepareAsync(VsCode(exe),
            new OpenWithContext(SessionKind.Local, Folder: sub.Replace('\\', '/') + "/"));
        Assert.Null(error);
        Assert.Equal(new[] { sub }, cmd!.Arguments);
    }

    // ── Shim resolution ──────────────────────────────────────────────────────────────────

    [Fact]
    public void ResolveShim_VsCodeLayout()
    {
        string install = Directory.CreateDirectory(Path.Combine(_tmp, "Microsoft VS Code")).FullName;
        string bin = Directory.CreateDirectory(Path.Combine(install, "bin")).FullName;
        File.WriteAllText(Path.Combine(bin, "code.cmd"), "@echo off");
        File.WriteAllText(Path.Combine(install, "Code.exe"), "");
        Assert.Equal(Path.Combine(install, "Code.exe"), OpenWithService.ResolveShim(Path.Combine(bin, "code.cmd")));
    }

    [Fact]
    public void ResolveShim_CursorLayout_ThreeLevelsUp()
    {
        string install = Directory.CreateDirectory(Path.Combine(_tmp, "cursor")).FullName;
        string bin = Directory.CreateDirectory(Path.Combine(install, "resources", "app", "bin")).FullName;
        File.WriteAllText(Path.Combine(bin, "cursor.cmd"), "@echo off");
        File.WriteAllText(Path.Combine(install, "Cursor.exe"), "");
        Assert.Equal(Path.Combine(install, "Cursor.exe"), OpenWithService.ResolveShim(Path.Combine(bin, "cursor.cmd")));
    }

    [Fact]
    public void ResolveShim_GenericScriptWithQuotedExe()
    {
        string exe = Path.Combine(_tmp, "rider64.exe");
        File.WriteAllText(exe, "");
        string script = Path.Combine(_tmp, "rider.cmd");
        File.WriteAllText(script, $"@echo off\r\nstart \"\" \"{exe}\" %*\r\n");
        Assert.Equal(exe, OpenWithService.ResolveShim(script));
    }

    [Fact]
    public void ResolveShim_GenericScript_IgnoresShellsAndMissingExes()
    {
        string script = Path.Combine(_tmp, "x.cmd");
        File.WriteAllText(script, "\"C:\\Windows\\System32\\cmd.exe\" /c \"C:\\does\\not\\exist.exe\"");
        Assert.Null(OpenWithService.ResolveShim(script));
    }

    [Fact]
    public void ResolveShim_NonBatchIsUnchanged() =>
        Assert.Equal(Exe, OpenWithService.ResolveShim(Exe));
}

using System;
using System.IO;
using System.Linq;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Auto-detection for "Open with…" (issue #178), against a fake install tree. The rule that
/// matters most: whatever is found, the seeded list never contains a batch file.
/// </summary>
public class OpenWithDetectorTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), $"csm-detect-{Guid.NewGuid():N}");

    public OpenWithDetectorTests() => Directory.CreateDirectory(_root);

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    private string Dir(params string[] parts) => Directory.CreateDirectory(Path.Combine(new[] { _root }.Concat(parts).ToArray())).FullName;

    private static string Touch(string path)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, "");
        return path;
    }

    private OpenWithDetector.DetectorEnvironment Env(string path = "") => new()
    {
        Path = path,
        LocalAppData = Dir("local"),
        ProgramFiles = Dir("pf"),
        ProgramFilesX86 = Dir("pf86"),
        UserProfile = Dir("home"),
    };

    [Fact]
    public void Empty_FindsNothing() => Assert.Empty(OpenWithDetector.Detect(Env()));

    [Fact]
    public void VsCodeOnPath_ResolvesShimToExe_WithRemoteForms_AndIsDefault()
    {
        // Today's zero-config lookup: code.cmd on PATH → ..\Code.exe.
        string install = Dir("portable", "vscode");
        string exe = Touch(Path.Combine(install, "Code.exe"));
        Touch(Path.Combine(install, "bin", "code.cmd"));
        // Somewhere the standard-folder probe doesn't look, so PATH is what finds it. The
        // relative and network PATH entries ahead of it must be skipped, not trusted.
        var env = Env(path: @"relative\dir;\\server\share;" + Path.Combine(install, "bin"));

        var found = OpenWithDetector.Detect(env);

        var vs = Assert.Single(found);
        Assert.Equal("VS Code", vs.Label);
        Assert.Equal(exe, vs.Executable);
        Assert.Equal("{folder}", vs.LocalArgs);
        Assert.Equal("--folder-uri vscode-remote://wsl+{distro:uri}{linuxPath:uri}", vs.WslArgs);
        Assert.Equal("--folder-uri vscode-remote://ssh-remote+{userAtHost:uri}{remoteFolder:uri}", vs.SshArgs);
        Assert.True(vs.IsDefault);
    }

    [Fact]
    public void StandardInstallFolders_VsCodeInsidersCursorWindsurfZed()
    {
        var env = Env();
        Touch(Path.Combine(env.LocalAppData, "Programs", "Microsoft VS Code Insiders", "Code - Insiders.exe"));
        Touch(Path.Combine(env.LocalAppData, "Programs", "cursor", "Cursor.exe"));
        Touch(Path.Combine(env.LocalAppData, "Programs", "Windsurf", "Windsurf.exe"));
        Touch(Path.Combine(env.LocalAppData, "Programs", "Zed", "Zed.exe"));

        var labels = OpenWithDetector.Detect(env).Select(e => e.Label).ToList();

        Assert.Equal(new[] { "VS Code Insiders", "Cursor", "Windsurf", "Zed" }, labels);
    }

    [Fact]
    public void Scoop_VsCode()
    {
        var env = Env();
        string exe = Touch(Path.Combine(env.UserProfile, "scoop", "apps", "vscode", "current", "Code.exe"));
        Assert.Equal(exe, Assert.Single(OpenWithDetector.Detect(env)).Executable);
    }

    [Fact]
    public void AppPathsRegistry_IsConsulted()
    {
        string exe = Touch(Path.Combine(_root, "custom", "Code.exe"));
        var baseEnv = Env();
        var env = new OpenWithDetector.DetectorEnvironment
        {
            LocalAppData = baseEnv.LocalAppData,
            ProgramFiles = baseEnv.ProgramFiles,
            ProgramFilesX86 = baseEnv.ProgramFilesX86,
            UserProfile = baseEnv.UserProfile,
            AppPath = name => name == "Code.exe" ? exe : null,
        };
        Assert.Equal(exe, Assert.Single(OpenWithDetector.Detect(env)).Executable);
    }

    [Fact]
    public void JetBrains_StandaloneAndToolbox_NewestWins_ExeNotScript()
    {
        var env = Env();
        Touch(Path.Combine(env.ProgramFiles, "JetBrains", "JetBrains Rider 2024.1", "bin", "rider64.exe"));
        string newer = Touch(Path.Combine(env.LocalAppData, "JetBrains", "Toolbox", "apps", "Rider", "ch-0", "252.1", "bin", "rider64.exe"));
        File.SetLastWriteTimeUtc(newer, DateTime.UtcNow.AddMinutes(5));
        Touch(Path.Combine(env.LocalAppData, "Programs", "PyCharm", "bin", "pycharm64.exe"));
        Touch(Path.Combine(env.LocalAppData, "JetBrains", "Toolbox", "scripts", "rider.cmd"));

        var found = OpenWithDetector.Detect(env);

        var rider = Assert.Single(found, e => e.Label == "Rider");
        Assert.Equal(newer, rider.Executable);
        Assert.Contains(found, e => e.Label == "PyCharm");
        Assert.All(found, e => Assert.False(OpenWithService.IsBatch(e.Executable)));
    }

    [Fact]
    public void NotepadPlusPlus_AndSublime()
    {
        var env = Env();
        Touch(Path.Combine(env.ProgramFiles, "Notepad++", "notepad++.exe"));
        Touch(Path.Combine(env.ProgramFiles, "Sublime Text", "subl.exe"));
        var found = OpenWithDetector.Detect(env);
        Assert.Equal("-openFoldersAsWorkspace {folder}", Assert.Single(found, e => e.Label == "Notepad++").LocalArgs);
        Assert.Contains(found, e => e.Label == "Sublime Text");
        Assert.Null(found.Single(e => e.Label == "Notepad++").SshArgs);
    }

    [Fact]
    public void VisualStudio_FromVsWhereJson()
    {
        const string json = """
        [
          { "displayName": "Visual Studio Enterprise 2026", "productPath": "C:\\VS\\Common7\\IDE\\devenv.exe" },
          { "displayName": "Build Tools", "productPath": "C:\\BT\\Common7\\Tools\\LaunchDevCmd.bat" },
          { "displayName": "Weird" }
        ]
        """;
        var vs = Assert.Single(OpenWithDetector.ParseVsWhere(json));
        Assert.Equal("Visual Studio Enterprise 2026", vs.Label);
        Assert.Equal(@"C:\VS\Common7\IDE\devenv.exe", vs.Executable);
        Assert.Equal("{folder}", vs.LocalArgs);
        Assert.Null(vs.WslArgs);
    }

    [Fact]
    public void VisualStudio_BrokenVsWhere_DoesNotStopOtherProbes()
    {
        var baseEnv = Env();
        Touch(Path.Combine(baseEnv.LocalAppData, "Programs", "Zed", "Zed.exe"));
        var env = new OpenWithDetector.DetectorEnvironment
        {
            LocalAppData = baseEnv.LocalAppData,
            ProgramFiles = baseEnv.ProgramFiles,
            ProgramFilesX86 = baseEnv.ProgramFilesX86,
            UserProfile = baseEnv.UserProfile,
            VsWhereJson = () => "{ not json",
        };
        Assert.Equal("Zed", Assert.Single(OpenWithDetector.Detect(env)).Label);
    }

    [Fact]
    public void SameExeFoundTwice_IsListedOnce()
    {
        var env = Env();
        string install = Path.Combine(env.ProgramFiles, "Microsoft VS Code");
        Touch(Path.Combine(install, "Code.exe"));
        Touch(Path.Combine(install, "bin", "code.cmd"));
        var env2 = new OpenWithDetector.DetectorEnvironment
        {
            Path = Path.Combine(install, "bin"),
            LocalAppData = env.LocalAppData,
            ProgramFiles = env.ProgramFiles,
            ProgramFilesX86 = env.ProgramFilesX86,
            UserProfile = env.UserProfile,
        };
        Assert.Single(OpenWithDetector.Detect(env2));
    }
}

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// Finds installed editors / IDEs to seed <see cref="AppSettings.OpenWith"/> (issue #178).
/// Runs once per profile — after that the list belongs to the user, like run-command
/// templates — and again only when the user clicks "Detect" in the editor.
/// </summary>
/// <remarks>
/// <b>All of this is disk and registry I/O, and Visual Studio needs a vswhere spawn: call
/// <see cref="Detect()"/> from <c>Task.Run</c>, never on the UI thread.</b> See "Never spawn a
/// process on the UI thread" in CLAUDE.md. Every probe is wrapped so one broken install can't
/// stop the rest being found.
/// </remarks>
public static class OpenWithDetector
{
    /// <summary>Where to look. Injectable so the probes can be tested against a temp tree.</summary>
    public sealed class DetectorEnvironment
    {
        public string Path { get; init; } = "";
        public string LocalAppData { get; init; } = "";
        public string ProgramFiles { get; init; } = "";
        public string ProgramFilesX86 { get; init; } = "";
        public string UserProfile { get; init; } = "";
        /// <summary>App Paths lookup: exe file name → full path (HKCU then HKLM). Null = none.</summary>
        public Func<string, string?> AppPath { get; init; } = _ => null;
        /// <summary>Runs vswhere and returns its JSON, or null. Null = skip Visual Studio.</summary>
        public Func<string?> VsWhereJson { get; init; } = () => null;

        public static DetectorEnvironment Current() => new()
        {
            Path = System.Environment.GetEnvironmentVariable("PATH") ?? "",
            LocalAppData = System.Environment.GetFolderPath(System.Environment.SpecialFolder.LocalApplicationData),
            ProgramFiles = System.Environment.GetFolderPath(System.Environment.SpecialFolder.ProgramFiles),
            ProgramFilesX86 = System.Environment.GetFolderPath(System.Environment.SpecialFolder.ProgramFilesX86),
            UserProfile = System.Environment.GetFolderPath(System.Environment.SpecialFolder.UserProfile),
            AppPath = RegistryAppPath,
            VsWhereJson = RunVsWhere,
        };
    }

    // VS Code and its forks share the CLI flags, including the remote ones. The remote forms
    // are ONE argument (`--folder-uri vscode-remote://…`) rather than `--remote wsl+X /path`:
    // a bare Linux path starts with '/', and Chromium reads `/name=value` as a switch on
    // Windows (verified against Code.exe — see OpenWithService's remarks). The `:uri` forms
    // percent-encode the values; VS Code's URI.parse decodes authority and path.
    public const string VsCodeLocalArgs = "{folder}";
    public const string VsCodeWslArgs = "--folder-uri vscode-remote://wsl+{distro:uri}{linuxPath:uri}";
    public const string VsCodeSshArgs = "--folder-uri vscode-remote://ssh-remote+{userAtHost:uri}{remoteFolder:uri}";

    private sealed record VsCodeFamily(string Label, string Shim, string Exe, string[] InstallDirs, string[] ScoopApps);

    private static readonly VsCodeFamily[] VsCodeFamilies =
    [
        new("VS Code", "code.cmd", "Code.exe", ["Microsoft VS Code"], ["vscode"]),
        new("VS Code Insiders", "code-insiders.cmd", "Code - Insiders.exe", ["Microsoft VS Code Insiders"], ["vscode-insiders"]),
        new("Cursor", "cursor.cmd", "Cursor.exe", ["cursor", "Cursor"], ["cursor"]),
        new("Windsurf", "windsurf.cmd", "Windsurf.exe", ["Windsurf", "windsurf"], ["windsurf"]),
        new("VSCodium", "codium.cmd", "VSCodium.exe", ["VSCodium"], ["vscodium"]),
    ];

    /// <summary>JetBrains launcher exe → product name. The 64-bit launchers, which every current IDE ships.</summary>
    private static readonly (string Exe, string Label)[] JetBrainsProducts =
    [
        ("rider64.exe", "Rider"),
        ("idea64.exe", "IntelliJ IDEA"),
        ("pycharm64.exe", "PyCharm"),
        ("webstorm64.exe", "WebStorm"),
        ("goland64.exe", "GoLand"),
        ("clion64.exe", "CLion"),
        ("phpstorm64.exe", "PhpStorm"),
        ("rubymine64.exe", "RubyMine"),
        ("rustrover64.exe", "RustRover"),
        ("datagrip64.exe", "DataGrip"),
        ("dataspell64.exe", "DataSpell"),
    ];

    public static List<OpenWithEntry> Detect() => Detect(DetectorEnvironment.Current());

    public static List<OpenWithEntry> Detect(DetectorEnvironment env)
    {
        var found = new List<OpenWithEntry>();
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

        void Add(OpenWithEntry e)
        {
            string key;
            try { key = System.IO.Path.GetFullPath(e.Executable); } catch { return; }
            if (OpenWithService.ValidateExecutable(e.Executable, allowBatch: false) != null) return;
            if (seen.Add(key)) found.Add(e);
        }

        foreach (var fam in VsCodeFamilies)
            Try(() => { if (FindVsCodeFamily(fam, env) is { } exe) Add(VsCodeEntry(fam.Label, exe)); });

        Try(() => { foreach (var e in FindVisualStudio(env)) Add(e); });
        Try(() => { foreach (var e in FindJetBrains(env)) Add(e); });
        Try(() => { foreach (var e in FindSimpleEditors(env)) Add(e); });

        OpenWithEntry.EnsureSingleDefault(found);   // VS Code first when present — today's behaviour
        return found;
    }

    public static OpenWithEntry VsCodeEntry(string label, string exe) => new()
    {
        Label = label,
        Executable = exe,
        LocalArgs = VsCodeLocalArgs,
        WslArgs = VsCodeWslArgs,
        SshArgs = VsCodeSshArgs,
    };

    private static void Try(Action probe)
    {
        try { probe(); } catch { /* one broken install must not stop the rest */ }
    }

    // ── VS Code family ────────────────────────────────────────────────────────────────────

    /// <summary>
    /// PATH shim first — today's zero-config lookup, and the install the user's shell
    /// actually runs — then App Paths, then the standard per-user / per-machine folders and
    /// Scoop. The shim is resolved to the exe; the batch file itself is never used.
    /// </summary>
    internal static string? FindVsCodeFamily(string label, DetectorEnvironment env) =>
        FindVsCodeFamily(VsCodeFamilies.First(f => f.Label == label), env);

    private static string? FindVsCodeFamily(VsCodeFamily fam, DetectorEnvironment env)
    {
        foreach (string dir in PathDirs(env.Path))
        {
            string shim = System.IO.Path.Combine(dir, fam.Shim);
            if (!File.Exists(shim)) continue;
            string? exe = OpenWithService.ResolveShim(shim);
            if (exe != null && File.Exists(exe)) return exe;
        }
        if (env.AppPath(fam.Exe) is { } appPath && File.Exists(appPath)) return appPath;
        foreach (string dirName in fam.InstallDirs)
        {
            foreach (string root in new[] { System.IO.Path.Combine(env.LocalAppData, "Programs"), env.ProgramFiles, env.ProgramFilesX86 })
            {
                if (string.IsNullOrEmpty(root)) continue;
                string exe = System.IO.Path.Combine(root, dirName, fam.Exe);
                if (File.Exists(exe)) return exe;
            }
        }
        foreach (string app in fam.ScoopApps)
        {
            if (string.IsNullOrEmpty(env.UserProfile)) break;
            string exe = System.IO.Path.Combine(env.UserProfile, "scoop", "apps", app, "current", fam.Exe);
            if (File.Exists(exe)) return exe;
        }
        return null;
    }

    private static IEnumerable<string> PathDirs(string path)
    {
        foreach (string raw in (path ?? "").Split(';', StringSplitOptions.RemoveEmptyEntries))
        {
            string d = raw.Trim().Trim('"');
            // Skip relative entries and network dirs — a PATH entry on a share is not a place
            // to pick up a program to launch on a click.
            if (d.Length == 0 || d.StartsWith(@"\\", StringComparison.Ordinal)) continue;
            bool qualified;
            try { qualified = System.IO.Path.IsPathFullyQualified(d); } catch { qualified = false; }
            if (qualified) yield return d;
        }
    }

    // ── Visual Studio ─────────────────────────────────────────────────────────────────────

    private static IEnumerable<OpenWithEntry> FindVisualStudio(DetectorEnvironment env)
    {
        string? json = env.VsWhereJson();
        return json == null ? [] : ParseVsWhere(json);
    }

    /// <summary>
    /// vswhere <c>-format json</c> → one entry per instance with a <c>devenv.exe</c>. Visual
    /// Studio opens a folder with <c>devenv.exe &lt;folder&gt;</c> (Open Folder mode). Local
    /// only — it has no WSL/SSH remote form worth seeding.
    /// </summary>
    internal static List<OpenWithEntry> ParseVsWhere(string json)
    {
        var list = new List<OpenWithEntry>();
        using var doc = JsonDocument.Parse(json);
        if (doc.RootElement.ValueKind != JsonValueKind.Array) return list;
        foreach (var inst in doc.RootElement.EnumerateArray())
        {
            string? productPath = inst.TryGetProperty("productPath", out var pp) ? pp.GetString() : null;
            if (string.IsNullOrEmpty(productPath)
                || !System.IO.Path.GetFileName(productPath).Equals("devenv.exe", StringComparison.OrdinalIgnoreCase))
                continue;
            string label = inst.TryGetProperty("displayName", out var dn) && dn.GetString() is { Length: > 0 } name
                ? name
                : "Visual Studio";
            list.Add(new OpenWithEntry { Label = label, Executable = productPath, LocalArgs = "{folder}" });
        }
        return list;
    }

    private static string? RunVsWhere()
    {
        string pf86 = System.Environment.GetFolderPath(System.Environment.SpecialFolder.ProgramFilesX86);
        string vswhere = System.IO.Path.Combine(pf86, "Microsoft Visual Studio", "Installer", "vswhere.exe");
        if (!File.Exists(vswhere)) return null;
        var psi = new ProcessStartInfo(vswhere)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = Encoding.UTF8,
        };
        foreach (var a in new[] { "-all", "-prerelease", "-format", "json", "-utf8", "-nologo" })
            psi.ArgumentList.Add(a);
        using var p = Process.Start(psi);
        if (p == null) return null;
        var stdout = p.StandardOutput.ReadToEndAsync();
        _ = p.StandardError.ReadToEndAsync();
        if (!p.WaitForExit(5000))
        {
            try { p.Kill(entireProcessTree: true); } catch { }
            return null;
        }
        return p.ExitCode == 0 ? stdout.Result : null;
    }

    // ── JetBrains ─────────────────────────────────────────────────────────────────────────

    /// <summary>
    /// Toolbox 2.x installs to <c>%LOCALAPPDATA%\Programs\&lt;IDE&gt;\bin</c>, Toolbox 1.x to
    /// <c>%LOCALAPPDATA%\JetBrains\Toolbox\apps\&lt;product&gt;\ch-N\&lt;build&gt;\bin</c>, and a
    /// standalone install to <c>%ProgramFiles%\JetBrains\&lt;IDE&gt;\bin</c>. The exe is launched
    /// directly — never the Toolbox <c>.cmd</c> scripts. Newest file wins per product.
    /// </summary>
    private static IEnumerable<OpenWithEntry> FindJetBrains(DetectorEnvironment env)
    {
        var binDirs = new List<string>();
        void AddChildBins(string root)
        {
            if (string.IsNullOrEmpty(root) || !Directory.Exists(root)) return;
            foreach (var d in Directory.EnumerateDirectories(root)) binDirs.Add(System.IO.Path.Combine(d, "bin"));
        }
        AddChildBins(System.IO.Path.Combine(env.LocalAppData, "Programs"));
        AddChildBins(System.IO.Path.Combine(env.ProgramFiles, "JetBrains"));
        string toolboxApps = System.IO.Path.Combine(env.LocalAppData, "JetBrains", "Toolbox", "apps");
        if (Directory.Exists(toolboxApps))
        {
            foreach (var product in Directory.EnumerateDirectories(toolboxApps))
                foreach (var channel in Directory.EnumerateDirectories(product))
                    foreach (var build in Directory.EnumerateDirectories(channel))
                        binDirs.Add(System.IO.Path.Combine(build, "bin"));
        }

        var best = new Dictionary<string, (string Path, DateTime When)>(StringComparer.OrdinalIgnoreCase);
        foreach (var bin in binDirs)
        {
            if (!Directory.Exists(bin)) continue;
            foreach (var (exeName, _) in JetBrainsProducts)
            {
                string exe = System.IO.Path.Combine(bin, exeName);
                if (!File.Exists(exe)) continue;
                var when = File.GetLastWriteTimeUtc(exe);
                if (!best.TryGetValue(exeName, out var cur) || when > cur.When) best[exeName] = (exe, when);
            }
        }
        foreach (var (exeName, label) in JetBrainsProducts)
        {
            if (best.TryGetValue(exeName, out var hit))
                // JetBrains IDEs open a \\wsl$ folder natively, so the WSL form is the UNC mirror.
                yield return new OpenWithEntry { Label = label, Executable = hit.Path, LocalArgs = "{folder}", WslArgs = "{folder}" };
        }
    }

    // ── Plain editors ─────────────────────────────────────────────────────────────────────

    private static IEnumerable<OpenWithEntry> FindSimpleEditors(DetectorEnvironment env)
    {
        // (label, App Paths name, candidate paths relative to roots, args)
        var zed = FirstExisting(env.AppPath("Zed.exe"),
            System.IO.Path.Combine(env.LocalAppData, "Programs", "Zed", "Zed.exe"));
        if (zed != null) yield return new OpenWithEntry { Label = "Zed", Executable = zed, LocalArgs = "{folder}", WslArgs = "{folder}" };

        var subl = FirstExisting(
            System.IO.Path.Combine(env.ProgramFiles, "Sublime Text", "subl.exe"),
            System.IO.Path.Combine(env.ProgramFiles, "Sublime Text 3", "subl.exe"),
            env.AppPath("sublime_text.exe"));
        if (subl != null) yield return new OpenWithEntry { Label = "Sublime Text", Executable = subl, LocalArgs = "{folder}", WslArgs = "{folder}" };

        var npp = FirstExisting(env.AppPath("notepad++.exe"),
            System.IO.Path.Combine(env.ProgramFiles, "Notepad++", "notepad++.exe"),
            System.IO.Path.Combine(env.ProgramFilesX86, "Notepad++", "notepad++.exe"));
        if (npp != null)
            yield return new OpenWithEntry
            {
                Label = "Notepad++",
                Executable = npp,
                LocalArgs = "-openFoldersAsWorkspace {folder}",
                WslArgs = "-openFoldersAsWorkspace {folder}",
            };
    }

    private static string? FirstExisting(params string?[] candidates)
    {
        foreach (var c in candidates)
        {
            if (string.IsNullOrEmpty(c)) continue;
            try { if (File.Exists(c)) return c; } catch { }
        }
        return null;
    }

    // ── Registry ──────────────────────────────────────────────────────────────────────────

    /// <summary><c>…\CurrentVersion\App Paths\&lt;exe&gt;</c> default value, HKCU then HKLM.</summary>
    private static string? RegistryAppPath(string exeName)
    {
        foreach (var hive in new[] { Microsoft.Win32.Registry.CurrentUser, Microsoft.Win32.Registry.LocalMachine })
        {
            try
            {
                using var key = hive.OpenSubKey($@"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exeName}");
                if (key?.GetValue(null) is string v && v.Length > 0)
                {
                    string expanded = System.Environment.ExpandEnvironmentVariables(v.Trim().Trim('"'));
                    if (File.Exists(expanded)) return expanded;
                }
            }
            catch { }
        }
        return null;
    }
}

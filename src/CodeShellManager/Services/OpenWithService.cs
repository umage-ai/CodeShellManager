using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using CodeShellManager.Models;
using Microsoft.Win32.SafeHandles;

namespace CodeShellManager.Services;

/// <summary>
/// The values a session offers to an "Open with…" argument template. Built on the UI thread
/// from the live <see cref="ShellSession"/> (a snapshot — the session is mutated there), then
/// handed to <see cref="OpenWithService"/> off it.
/// </summary>
public sealed record OpenWithContext(
    SessionKind Kind,
    string Folder = "",
    string Distro = "",
    string LinuxPath = "",
    string WslUser = "",
    string User = "",
    string Host = "",
    int Port = 22,
    string RemoteFolder = "")
{
    public static OpenWithContext FromSession(ShellSession s) => new(
        s.Kind,
        Folder: s.WorkingFolder ?? "",
        Distro: s.WslDistro ?? "",
        LinuxPath: s.WslWorkingFolder ?? "",
        WslUser: s.WslUser ?? "",
        User: s.SshUser ?? "",
        Host: s.SshHost ?? "",
        Port: s.SshPort,
        RemoteFolder: s.SshRemoteFolder ?? "");

    /// <summary>
    /// Whether a session of this shape has anything to open: a local folder, a distro, or a
    /// host. Pure — no disk access.
    /// </summary>
    public bool HasTarget => Kind switch
    {
        SessionKind.Local => !string.IsNullOrWhiteSpace(Folder),
        SessionKind.Wsl => !string.IsNullOrWhiteSpace(Distro),
        SessionKind.Ssh => !string.IsNullOrWhiteSpace(Host),
        _ => false,
    };
}

/// <summary>A resolved launch: the program and its argv, each element passed as-is via <see cref="ProcessStartInfo.ArgumentList"/>.</summary>
public sealed record OpenWithCommand(string Executable, IReadOnlyList<string> Arguments)
{
    /// <summary>
    /// The command line, for the editor preview, menu tooltips and error messages. Never
    /// executed. Quoted with <see cref="ShellSession.QuoteForCmd"/>, which encodes the same
    /// argv as .NET's own <c>ArgumentList</c> encoding (both are MSVCRT rules — see
    /// <c>OpenWithServiceTests.ArgumentList_RealProcess_*</c>), though the spelling can differ.
    /// </summary>
    public string Display =>
        string.Join(" ", new[] { ShellSession.QuoteForCmd(Executable) }
            .Concat(Arguments.Select(a => ShellSession.QuoteForCmd(a))));

    public ProcessStartInfo ToStartInfo()
    {
        // UseShellExecute=false: CreateProcess on the program itself, no shell and no file
        // association lookup. ArgumentList: each element is quoted on its own (MSVCRT rules),
        // so a placeholder value cannot spill into a second argument.
        var psi = new ProcessStartInfo(Executable) { UseShellExecute = false };
        foreach (var a in Arguments) psi.ArgumentList.Add(a);
        return psi;
    }
}

/// <summary>
/// "Open with…" (issue #178): template parsing, placeholder substitution and every
/// validation rule — WPF-free so it is unit-tested. See "Open with…" in CLAUDE.md.
/// </summary>
/// <remarks>
/// <para><b>Never a command line built by interpolation.</b> A template is split into tokens
/// first; only then are placeholders substituted, each inside its own token. A folder named
/// <c>a" --x "b</c> therefore stays one argv element, and the process is started with
/// <c>UseShellExecute=false</c> + <c>ArgumentList</c>, so no shell ever parses it.</para>
/// <para><b>Batch files are the exception that has to be fenced.</b> CreateProcess runs a
/// <c>.cmd</c>/<c>.bat</c> through <c>cmd.exe</c>, which re-parses the command line with its
/// own rules — <c>%</c> expands even inside quotes, <c>&amp;</c> chains a second command
/// (the BatBadBut class, CVE-2024-24576). Detection resolves the known shims
/// (<c>code.cmd</c> → <c>Code.exe</c>) so the seeded list holds none; when the user insists on
/// a batch file anyway, every argument containing a cmd metacharacter is refused — never
/// escaped. An import never accepts one at all.</para>
/// <para><b>An argument must not be readable as a switch.</b> A value that starts an argv
/// element may not begin with <c>-</c> or <c>/</c>: Chromium (so every Electron editor)
/// accepts <c>/name=value</c> as a switch on Windows — verified against Code.exe 1.143.1,
/// where <c>/js-flags=…</c> reached the renderer as <c>--js-flags=…</c>. That is why the seeded
/// VS Code remote forms are the single-token <c>--folder-uri vscode-remote://…</c> rather than
/// a bare Linux path argument.</para>
/// </remarks>
public static class OpenWithService
{
    /// <summary>Placeholders each session kind provides. Anything else in a template is an error, not literal text.</summary>
    public static readonly IReadOnlyDictionary<SessionKind, string[]> PlaceholdersByKind =
        new Dictionary<SessionKind, string[]>
        {
            [SessionKind.Local] = ["folder"],
            [SessionKind.Wsl] = ["folder", "distro", "linuxPath"],
            [SessionKind.Ssh] = ["user", "host", "userAtHost", "port", "remoteFolder"],
        };

    /// <summary>Placeholders whose <c>:uri</c> form keeps <c>/</c> (a path), rather than encoding it.</summary>
    private static readonly HashSet<string> PathPlaceholders = new(StringComparer.Ordinal) { "linuxPath", "remoteFolder" };

    /// <summary>
    /// Characters cmd.exe interprets in a batch file's command line, quoted or not. <c>%</c>
    /// and <c>!</c> expand variables, <c>&amp; | &lt; &gt;</c> chain and redirect, <c>^</c>
    /// escapes, <c>"</c> toggles cmd's quote state, <c>( )</c> group, and <c>, ; =</c> are
    /// argument delimiters to a batch script.
    /// </summary>
    public const string BatchMetaChars = "%!&|<>^\"(),;=";

    /// <summary>
    /// Programs that run text (or an argument) as code. Checked against the file name, the
    /// long-path name and the PE version-info original name. This is a speed bump, not the
    /// boundary: a renamed copy of an arbitrary program can't be recognised by name, which is
    /// why imported lists are confirmed by the user and why everything under %SystemRoot% is
    /// refused outright.
    /// </summary>
    private static readonly HashSet<string> ShellLikeNames = new(StringComparer.OrdinalIgnoreCase)
    {
        "cmd", "powershell", "powershell_ise", "pwsh", "wsl", "bash", "sh", "wscript", "cscript",
        "mshta", "rundll32", "regsvr32", "msiexec", "conhost", "wt", "schtasks", "forfiles",
        "runas", "start", "reg", "certutil", "bitsadmin", "ssh", "scp", "sftp", "wmic", "pcalua",
        "mmc", "hh", "explorer", "msbuild", "installutil", "odbcconf", "regasm", "regsvcs", "cmstp",
        "msxsl", "ieexec", "dotnet", "node", "python", "pythonw", "py", "pyw", "perl", "ruby",
        "java", "javaw", "csi", "fsi", "winrm", "psexec", "at",
    };

    private static readonly Regex PlaceholderRegex = new(@"\{([A-Za-z]+)(:uri)?\}", RegexOptions.CultureInvariant);

    public const int MaxEntries = 50;

    // ── Executable ────────────────────────────────────────────────────────────────────────

    public static bool IsBatch(string? path)
    {
        string ext = Path.GetExtension(path ?? "");
        return ext.Equals(".cmd", StringComparison.OrdinalIgnoreCase)
            || ext.Equals(".bat", StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>%SystemRoot%, e.g. <c>C:\Windows</c>. Overridable for tests.</summary>
    internal static string SystemRoot { get; set; } =
        Environment.GetFolderPath(Environment.SpecialFolder.Windows) is { Length: > 0 } w ? w : @"C:\Windows";

    /// <summary>
    /// Null when <paramref name="path"/> is acceptable as an entry's program, else the reason.
    /// Pure — no disk access; <see cref="ValidateExecutableOnDisk"/> repeats the checks against
    /// the real path at import and launch time. Rules: an absolute local path (no relative path,
    /// so nothing is resolved against PATH or the cwd; no UNC/device path, so nothing runs from
    /// a share), a <c>.exe</c> — or a <c>.cmd</c>/<c>.bat</c> only when <paramref name="allowBatch"/>
    /// — not under %SystemRoot%, and not a known shell or script host.
    /// </summary>
    public static string? ValidateExecutable(string? path, bool allowBatch)
    {
        if (string.IsNullOrWhiteSpace(path)) return "No program is set.";
        if (path.Any(char.IsControl)) return "The program path contains control characters.";
        if (path.StartsWith(@"\\", StringComparison.Ordinal) || path.StartsWith("//", StringComparison.Ordinal))
            return "Network and device paths are not accepted — use a program on this machine.";
        bool qualified;
        try { qualified = Path.IsPathFullyQualified(path); } catch { qualified = false; }
        if (!qualified) return "The program must be a full path, e.g. C:\\Program Files\\Editor\\editor.exe.";

        if (IsBatch(path))
        {
            if (!allowBatch)
                return "Batch files (.cmd/.bat) run through cmd.exe and are not accepted here — point at the program's .exe.";
        }
        else if (!Path.GetExtension(path).Equals(".exe", StringComparison.OrdinalIgnoreCase))
        {
            return "The program must be an .exe.";
        }

        if (IsUnder(path, SystemRoot))
            return $"Programs under {SystemRoot} are not accepted — no editor lives there, and many tools there run commands from their arguments.";
        if (ShellLikeNames.Contains(Path.GetFileNameWithoutExtension(path)))
            return $"{Path.GetFileName(path)} is a shell or script host, not an editor — it is not accepted.";
        return null;
    }

    /// <summary>
    /// <see cref="ValidateExecutable"/> against the program's REAL path: 8.3 short names
    /// expanded (<c>POWERS~1.EXE</c>), junctions, symlinks and subst drives followed, then a
    /// network drive refused (mapped or subst'd onto a share — the final path is UNC, or the
    /// drive type is Network), and the PE original file name checked too, so a renamed copy of
    /// a known shell is caught. A program that doesn't exist yet passes (it fails at launch).
    /// Touches the disk — call it off the UI thread.
    /// </summary>
    public static string? ValidateExecutableOnDisk(string? path, bool allowBatch, Func<string, DriveType>? driveTypeOf = null)
    {
        string? shapeError = ValidateExecutable(path, allowBatch);
        if (shapeError != null) return shapeError;
        driveTypeOf ??= DriveTypeOf;
        try
        {
            if (driveTypeOf(Path.GetPathRoot(path!)!) == DriveType.Network)
                return "Programs on a network drive are not accepted — use a program on this machine.";
            if (!File.Exists(path)) return null;

            string? real = FinalPath(path!);
            if (real != null && !string.Equals(real, path, StringComparison.OrdinalIgnoreCase))
            {
                if (real.StartsWith(@"\\", StringComparison.Ordinal))
                    return "That program resolves to a network location — it is not accepted.";
                if (driveTypeOf(Path.GetPathRoot(real)!) == DriveType.Network)
                    return "Programs on a network drive are not accepted — use a program on this machine.";
                string? realError = ValidateExecutable(real, allowBatch);
                if (realError != null) return realError;
            }

            var info = FileVersionInfo.GetVersionInfo(path!);
            foreach (var name in new[] { info.OriginalFilename, info.InternalName })
            {
                if (string.IsNullOrWhiteSpace(name)) continue;
                string bare = Path.GetFileNameWithoutExtension(name.Trim().Replace(".mui", "", StringComparison.OrdinalIgnoreCase));
                if (ShellLikeNames.Contains(bare))
                    return $"{Path.GetFileName(path)} is a copy of {name.Trim()}, a shell or script host — it is not accepted.";
            }
        }
        catch (Exception ex)
        {
            return $"Couldn't check the program: {ex.Message}";
        }
        return null;
    }

    private static DriveType DriveTypeOf(string root)
    {
        try { return new DriveInfo(root).DriveType; } catch { return DriveType.Unknown; }
    }

    private static bool IsUnder(string path, string root)
    {
        if (string.IsNullOrEmpty(root)) return false;
        try
        {
            string p = Path.TrimEndingDirectorySeparator(Path.GetFullPath(path));
            string r = Path.TrimEndingDirectorySeparator(Path.GetFullPath(root));
            return p.StartsWith(r + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
                || p.Equals(r, StringComparison.OrdinalIgnoreCase);
        }
        catch { return false; }
    }

    /// <summary>The file's final path (GetFinalPathNameByHandle): long names, links and subst resolved. Null on failure.</summary>
    internal static string? FinalPath(string path)
    {
        try
        {
            using var h = CreateFileW(Path.GetFullPath(path), 0, 0x7, IntPtr.Zero, 3, 0x02000000, IntPtr.Zero);
            if (h.IsInvalid) return null;
            var buf = new char[2048];
            uint n = GetFinalPathNameByHandleW(h, buf, (uint)buf.Length, 0);
            if (n == 0 || n >= buf.Length) return null;
            string final = new string(buf, 0, (int)n);
            if (final.StartsWith(@"\\?\UNC\", StringComparison.Ordinal)) return @"\\" + final[8..];
            if (final.StartsWith(@"\\?\", StringComparison.Ordinal)) return final[4..];
            return final;
        }
        catch { return null; }
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint GetFinalPathNameByHandleW(SafeFileHandle h, [Out] char[] buf, uint len, uint flags);

    // ── Templates ─────────────────────────────────────────────────────────────────────────

    /// <summary>
    /// Splits a template into tokens on whitespace; a <c>"…"</c> span groups text containing
    /// spaces into one token (the quotes are removed — there is no escape syntax, and a
    /// literal <c>"</c> cannot be written). Returns null on an unbalanced quote.
    /// </summary>
    public static List<string>? Tokenize(string? template)
    {
        var tokens = new List<string>();
        if (string.IsNullOrWhiteSpace(template)) return tokens;
        var sb = new StringBuilder();
        bool inQuotes = false, inToken = false;
        foreach (char c in template)
        {
            if (c == '"') { inQuotes = !inQuotes; inToken = true; continue; }
            if (!inQuotes && char.IsWhiteSpace(c))
            {
                if (inToken) { tokens.Add(sb.ToString()); sb.Clear(); inToken = false; }
                continue;
            }
            sb.Append(c);
            inToken = true;
        }
        if (inQuotes) return null;
        if (inToken) tokens.Add(sb.ToString());
        return tokens;
    }

    /// <summary>
    /// Null when the template is usable for <paramref name="kind"/>, else the reason. Blank is
    /// valid (= not offered for that kind). A non-blank template must use at least one
    /// placeholder: an Open-with entry opens the session's location, and a template of only
    /// literal arguments would just be a fixed command line behind a friendly label.
    /// </summary>
    public static string? ValidateTemplate(string? template, SessionKind kind)
    {
        if (string.IsNullOrWhiteSpace(template)) return null;
        if (template.Any(char.IsControl)) return "The arguments contain control characters.";
        var tokens = Tokenize(template);
        if (tokens == null) return "Unbalanced quote in the arguments.";
        var allowed = PlaceholdersByKind[kind];
        int count = 0;
        foreach (var token in tokens)
        {
            foreach (Match m in PlaceholderRegex.Matches(token))
            {
                count++;
                string name = m.Groups[1].Value;
                if (!allowed.Contains(name, StringComparer.Ordinal))
                    return $"{{{name}}} is not available for {KindName(kind)} sessions (use {string.Join(", ", allowed.Select(a => "{" + a + "}"))}).";
            }
        }
        if (count == 0)
            return $"The arguments must use a placeholder for the session's location (e.g. {string.Join(", ", allowed.Select(a => "{" + a + "}"))}).";
        return null;
    }

    public static string KindName(SessionKind kind) => kind switch
    {
        SessionKind.Local => "local",
        SessionKind.Wsl => "WSL",
        SessionKind.Ssh => "SSH",
        _ => kind.ToString(),
    };

    // ── Build ─────────────────────────────────────────────────────────────────────────────

    /// <summary>
    /// Resolves <paramref name="entry"/> for <paramref name="ctx"/> into a program + argv, or
    /// returns the reason it can't. Pure: no disk access (folder existence and the program
    /// file are checked by <see cref="PrepareAsync"/>).
    /// </summary>
    public static (OpenWithCommand? Command, string? Error) Build(OpenWithEntry entry, OpenWithContext ctx)
    {
        string? exeError = ValidateExecutable(entry.Executable, allowBatch: true);
        if (exeError != null) return (null, exeError);

        string? template = entry.ArgsFor(ctx.Kind);
        if (string.IsNullOrWhiteSpace(template))
            return (null, $"\"{entry.Label}\" has no arguments for {KindName(ctx.Kind)} sessions.");
        string? templateError = ValidateTemplate(template, ctx.Kind);
        if (templateError != null) return (null, templateError);

        var values = ValuesFor(ctx);
        var args = new List<string>();
        foreach (var token in Tokenize(template)!)
        {
            var matches = PlaceholderRegex.Matches(token);
            if (matches.Count == 0) { args.Add(token); continue; }

            // A token that is nothing but one placeholder, whose value is empty, is dropped
            // rather than passed as "".
            if (matches.Count == 1 && matches[0].Value == token && values[matches[0].Groups[1].Value].Length == 0)
                continue;

            foreach (Match m in matches)
            {
                string name = m.Groups[1].Value;
                string value = values[name];
                string? valueError = ValidateValue(name, value);
                if (valueError != null) return (null, valueError);
                // Empty inside a larger token (`vscode-remote://ssh-remote+box{remoteFolder:uri}`)
                // would silently produce a different target. Only {user} may be empty.
                if (value.Length == 0 && name != "user")
                    return (null, MissingValueMessage(name, ctx.Kind));
            }

            string arg = PlaceholderRegex.Replace(token, m => Encode(m.Groups[1].Value, values[m.Groups[1].Value], m.Groups[2].Success));
            // A placeholder that STARTS an argv element must not make it switch-shaped. '-' is
            // refused for every value already; '/' is refused here because Chromium/Electron
            // parse /name=value as a switch on Windows (see the class remarks).
            if (matches[0].Index == 0 && (arg.StartsWith('-') || arg.StartsWith('/')))
                return (null, $"The session's {matches[0].Groups[1].Value} would start an argument with '{arg[0]}', " +
                              "which the program could read as a switch. Put it inside a larger argument " +
                              "(e.g. --folder-uri vscode-remote://…{linuxPath:uri}).");
            args.Add(arg);
        }

        if (IsBatch(entry.Executable))
        {
            // BatBadBut: cmd.exe re-parses a batch file's arguments. Refuse, never escape —
            // there is no escaping that is correct for every batch script.
            foreach (var a in args)
            {
                int bad = a.IndexOfAny(BatchMetaChars.ToCharArray());
                if (bad >= 0)
                    return (null, $"{Path.GetFileName(entry.Executable)} is a batch file, which runs through cmd.exe, " +
                                  $"and the argument \"{a}\" contains '{a[bad]}', which cmd.exe would interpret. " +
                                  "Point the entry at the program's .exe instead.");
            }
        }

        return (new OpenWithCommand(entry.Executable, args), null);
    }

    private static string MissingValueMessage(string name, SessionKind kind) => name switch
    {
        "remoteFolder" => "This SSH session has no remote folder. Set one with Edit session…, or use an editor entry that can open a host without a folder.",
        "linuxPath" => "Couldn't work out this WSL session's Linux folder. Set one with Edit session….",
        _ => $"This {KindName(kind)} session has no {name}.",
    };

    /// <summary>
    /// The <c>:uri</c> form, for a value spliced into a URI such as
    /// <c>vscode-remote://wsl+{distro:uri}{linuxPath:uri}</c>. Path placeholders encode each
    /// segment and keep <c>/</c>; anything else is encoded whole (so <c>@</c> in
    /// <c>{userAtHost:uri}</c> becomes <c>%40</c> and cannot read as URI user-info). VS Code's
    /// <c>URI.parse</c> percent-decodes both the authority and the path.
    /// </summary>
    internal static string Encode(string name, string value, bool uri)
    {
        if (!uri) return value;
        if (PathPlaceholders.Contains(name))
            return string.Join("/", value.Split('/').Select(Uri.EscapeDataString));
        return Uri.EscapeDataString(value);
    }

    private static Dictionary<string, string> ValuesFor(OpenWithContext ctx)
    {
        string user = ctx.User?.Trim() ?? "";
        string host = ctx.Host?.Trim() ?? "";
        return new Dictionary<string, string>(StringComparer.Ordinal)
        {
            ["folder"] = ctx.Folder ?? "",
            ["distro"] = ctx.Distro?.Trim() ?? "",
            ["linuxPath"] = ctx.LinuxPath ?? "",
            ["user"] = user,
            ["host"] = host,
            ["userAtHost"] = user.Length == 0 ? host : $"{user}@{host}",
            ["port"] = ctx.Port.ToString(System.Globalization.CultureInfo.InvariantCulture),
            ["remoteFolder"] = ctx.RemoteFolder ?? "",
        };
    }

    /// <summary>
    /// Rules for a substituted value. Every value comes from state.json / an import / the
    /// user's own session — untrusted. Argv quoting already keeps it one element; these stop
    /// it being read as something else by the program that receives it.
    /// </summary>
    private static string? ValidateValue(string name, string value)
    {
        if (value.Any(char.IsControl))
            return $"The session's {name} contains control characters.";
        // An option-shaped value (-x, --foo) could be taken by the editor as a flag. Refused
        // wherever it sits — stricter than needed, never wrong for a path, host or distro.
        if (value.StartsWith('-'))
            return $"The session's {name} starts with '-', which the program could read as an option.";
        switch (name)
        {
            case "folder":
                if (value.Length == 0) break;
                bool qualified;
                try { qualified = Path.IsPathFullyQualified(value); } catch { qualified = false; }
                if (!qualified) return "The session's folder is not a full path.";
                break;
            case "host":
            case "user":
            case "userAtHost":
            case "distro":
                if (value.Any(char.IsWhiteSpace)) return $"The session's {name} contains whitespace.";
                break;
            case "port":
                if (!int.TryParse(value, out int p) || p < 1 || p > 65535) return "The session's port is out of range.";
                break;
        }
        return null;
    }

    // ── Selection / display ───────────────────────────────────────────────────────────────

    public static IEnumerable<OpenWithEntry> ApplicableTo(IEnumerable<OpenWithEntry>? list, SessionKind kind) =>
        (list ?? []).Where(e => e != null && e.AppliesTo(kind));

    /// <summary>The default entry when it applies to this kind; otherwise the first entry that does. Null when none does.</summary>
    public static OpenWithEntry? DefaultFor(IEnumerable<OpenWithEntry>? list, SessionKind kind)
    {
        var applicable = ApplicableTo(list, kind).ToList();
        return applicable.FirstOrDefault(e => e.IsDefault) ?? applicable.FirstOrDefault();
    }

    /// <summary>
    /// What a click on <paramref name="entry"/> for this session would run, or why it can't —
    /// for menu tooltips, so a friendly label can't hide the program or its arguments.
    /// </summary>
    public static string Describe(OpenWithEntry entry, OpenWithContext ctx)
    {
        var (cmd, error) = Build(entry, ctx);
        return cmd != null ? cmd.Display : $"{ShellSession.QuoteForCmd(entry.Executable)} — {error}";
    }

    /// <summary>
    /// The whole list, spelled out for the import confirmation: each entry's program and the
    /// literal argument template for every kind it applies to.
    /// </summary>
    public static string DescribeForReview(IEnumerable<OpenWithEntry> list)
    {
        var sb = new StringBuilder();
        foreach (var e in list)
        {
            sb.Append(e.IsDefault ? "★ " : "• ").Append(e.Label).Append('\n');
            foreach (var kind in new[] { SessionKind.Local, SessionKind.Wsl, SessionKind.Ssh })
            {
                string? t = e.ArgsFor(kind);
                if (string.IsNullOrWhiteSpace(t)) continue;
                sb.Append("    ").Append(KindName(kind)).Append(": ")
                  .Append(ShellSession.QuoteForCmd(e.Executable)).Append(' ').Append(t).Append('\n');
            }
        }
        return sb.ToString();
    }

    /// <summary>True when two lists would launch the same things (labels, programs, templates, default, order).</summary>
    public static bool SameCommands(IReadOnlyList<OpenWithEntry> a, IReadOnlyList<OpenWithEntry> b) =>
        a.Count == b.Count && a.Zip(b).All(p =>
            p.First.Label == p.Second.Label
            && string.Equals(p.First.Executable, p.Second.Executable, StringComparison.OrdinalIgnoreCase)
            && p.First.LocalArgs == p.Second.LocalArgs && p.First.WslArgs == p.Second.WslArgs
            && p.First.SshArgs == p.Second.SshArgs && p.First.IsDefault == p.Second.IsDefault);

    // ── Sanitize (state.json / import) ────────────────────────────────────────────────────

    /// <summary>
    /// Returns a cleaned copy of a list that came from disk: entries with an unacceptable
    /// program are dropped; templates that don't validate are cleared (and an entry left with
    /// none is dropped); labels and ids are filled in; exactly one default. state.json passes
    /// <paramref name="allowBatch"/> = true (the user may have chosen a batch file in the
    /// editor); an import passes false — see "Open with…" in CLAUDE.md. Pure.
    /// </summary>
    public static List<OpenWithEntry> Sanitize(IEnumerable<OpenWithEntry?>? list, bool allowBatch, Action<string>? log = null) =>
        SanitizeCore(list, e => ValidateExecutable(e, allowBatch), log);

    /// <summary><see cref="Sanitize"/> plus <see cref="ValidateExecutableOnDisk"/> per program (no batch files). Touches the disk.</summary>
    public static List<OpenWithEntry> SanitizeImported(IEnumerable<OpenWithEntry?>? list, Action<string>? log = null) =>
        SanitizeCore(list, e => ValidateExecutableOnDisk(e, allowBatch: false), log);

    private static List<OpenWithEntry> SanitizeCore(IEnumerable<OpenWithEntry?>? list, Func<string, string?> validateExe, Action<string>? log)
    {
        var result = new List<OpenWithEntry>();
        var ids = new HashSet<string>(StringComparer.Ordinal);
        foreach (var raw in list ?? [])
        {
            if (raw == null) continue;
            if (result.Count >= MaxEntries) { log?.Invoke($"dropped entries beyond {MaxEntries}"); break; }
            var e = raw.Clone();
            e.Executable = e.Executable?.Trim() ?? "";
            string? exeError = validateExe(e.Executable);
            if (exeError != null)
            {
                log?.Invoke($"dropped '{e.Label}' ({e.Executable}): {exeError}");
                continue;
            }
            e.LocalArgs = CleanTemplate(e.LocalArgs, SessionKind.Local, e, log);
            e.WslArgs = CleanTemplate(e.WslArgs, SessionKind.Wsl, e, log);
            e.SshArgs = CleanTemplate(e.SshArgs, SessionKind.Ssh, e, log);
            if (e.LocalArgs == null && e.WslArgs == null && e.SshArgs == null)
            {
                log?.Invoke($"dropped '{e.Label}' ({e.Executable}): no usable arguments for any session kind");
                continue;
            }
            e.Label = string.IsNullOrWhiteSpace(e.Label)
                ? Path.GetFileNameWithoutExtension(e.Executable)
                : new string(e.Label.Trim().Where(c => !char.IsControl(c)).Take(80).ToArray());
            if (string.IsNullOrWhiteSpace(e.Id) || !ids.Add(e.Id))
            {
                e.Id = Guid.NewGuid().ToString();
                ids.Add(e.Id);
            }
            result.Add(e);
        }
        OpenWithEntry.EnsureSingleDefault(result);
        return result;
    }

    private static string? CleanTemplate(string? template, SessionKind kind, OpenWithEntry e, Action<string>? log)
    {
        if (string.IsNullOrWhiteSpace(template)) return null;
        string? err = ValidateTemplate(template, kind);
        if (err == null) return template.Trim();
        log?.Invoke($"'{e.Label}': cleared {KindName(kind)} arguments: {err}");
        return null;
    }

    // ── Shims ─────────────────────────────────────────────────────────────────────────────

    /// <summary>
    /// The program behind a launcher script. Known VS Code-family shims (<c>code.cmd</c>,
    /// <c>code-insiders.cmd</c>, <c>cursor.cmd</c>, <c>windsurf.cmd</c>, <c>codium.cmd</c>) map
    /// to their Electron exe a few folders up. Any other batch file is scanned for quoted
    /// absolute paths to an existing, acceptable <c>.exe</c> — and resolves only when there is
    /// exactly ONE distinct candidate (JetBrains Toolbox scripts have that shape); a script that
    /// names several programs is ambiguous and returns null rather than a guess. A non-batch
    /// path is returned unchanged. Touches the disk.
    /// </summary>
    public static string? ResolveShim(string path)
    {
        if (!IsBatch(path)) return path;
        try
        {
            string name = Path.GetFileNameWithoutExtension(path);
            string? known = name.ToLowerInvariant() switch
            {
                "code" => "Code.exe",
                "code-insiders" => "Code - Insiders.exe",
                "cursor" => "Cursor.exe",
                "windsurf" => "Windsurf.exe",
                "codium" => "VSCodium.exe",
                _ => null,
            };
            if (known != null)
            {
                string? dir = Path.GetDirectoryName(Path.GetFullPath(path));
                for (int i = 0; i < 4 && dir != null; i++)
                {
                    string candidate = Path.Combine(dir, known);
                    if (File.Exists(candidate)) return candidate;
                    dir = Path.GetDirectoryName(dir);
                }
            }

            var info = new FileInfo(path);
            if (!info.Exists || info.Length > 64 * 1024) return null;
            var candidates = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (Match m in Regex.Matches(File.ReadAllText(path), "\"([A-Za-z]:\\\\[^\"\\r\\n]+?\\.exe)\"", RegexOptions.IgnoreCase))
            {
                string exe = m.Groups[1].Value;
                if (ValidateExecutable(exe, allowBatch: false) == null && File.Exists(exe))
                    candidates.Add(Path.GetFullPath(exe));
            }
            return candidates.Count == 1 ? candidates.First() : null;
        }
        catch { }
        return null;
    }

    // ── Launch ────────────────────────────────────────────────────────────────────────────

    /// <summary>
    /// Opens the session's folder in <paramref name="entry"/>. Runs entirely on the thread
    /// pool (disk checks, a possible WSL <c>$HOME</c> probe, <c>Process.Start</c>) — never
    /// call the inner work on the UI thread. Returns null on success, else a message for the
    /// user. Never throws.
    /// </summary>
    public static Task<string?> LaunchAsync(OpenWithEntry entry, OpenWithContext ctx) => Task.Run(async () =>
    {
        try
        {
            var prepared = await PrepareAsync(entry, ctx).ConfigureAwait(false);
            if (prepared.Error != null) return prepared.Error;
            Process.Start(prepared.Command!.ToStartInfo())?.Dispose();
            return (string?)null;
        }
        catch (Exception ex)
        {
            return $"Couldn't start {entry.Label}: {ex.Message}";
        }
    });

    /// <summary>
    /// Everything <see cref="LaunchAsync"/> does short of starting the process. Order matters:
    /// everything that can be decided without side effects (program shape, template, values)
    /// is validated FIRST, then the program on disk, and only then is a WSL <c>$HOME</c> probed
    /// — never spawn wsl.exe for a command that was going to be refused anyway. Touches the disk.
    /// </summary>
    internal static async Task<(OpenWithCommand? Command, string? Error)> PrepareAsync(
        OpenWithEntry entry, OpenWithContext ctx, Func<string, string?, Task<string?>>? homeProbe = null)
    {
        string? template = entry.ArgsFor(ctx.Kind);
        bool usesFolder = template?.Contains("{folder", StringComparison.Ordinal) == true;
        bool needsHome = false;
        string lp = ctx.LinuxPath?.Trim() ?? "";
        if (ctx.Kind == SessionKind.Wsl && template?.Contains("{linuxPath", StringComparison.Ordinal) == true)
            needsHome = lp.Length == 0 || lp == "~" || lp.StartsWith("~/", StringComparison.Ordinal);

        // 1. Pure validation, with a stand-in for the not-yet-resolved home folder.
        var probeCtx = needsHome ? ctx with { LinuxPath = "/home/stand-in" + (lp.Length > 1 ? lp[1..] : "") } : ctx;
        var dry = Build(entry, probeCtx);
        if (dry.Error != null) return dry;

        // 2. The program on disk (long path, links, network drive, renamed shell).
        string? diskError = ValidateExecutableOnDisk(entry.Executable, allowBatch: true);
        if (diskError != null) return (null, diskError);
        if (!File.Exists(entry.Executable))
            return (null, $"{entry.Label}: program not found at {entry.Executable}. Edit the entry under Open with → Edit…");

        // 3. Only now the side-effecting probe: a blank Linux folder means $HOME.
        if (needsHome)
        {
            homeProbe ??= WslDiscoveryService.GetDistroHomeAsync;
            string? home = await homeProbe(ctx.Distro, ctx.WslUser).ConfigureAwait(false);
            ctx = ctx with { LinuxPath = home == null ? "" : lp.Length <= 1 ? home : home.TrimEnd('/') + lp[1..] };
        }

        // 4. The folder must be an existing directory (same check as Open in Explorer).
        if (usesFolder)
        {
            string? target = FolderOpener.Target(ctx.Folder);
            if (target == null) return (null, $"The session's folder doesn't exist: {ctx.Folder}");
            ctx = ctx with { Folder = target };
        }

        return Build(entry, ctx);
    }

    /// <summary>Appends a line to crash.log. Never throws.</summary>
    public static void Log(string message)
    {
        try
        {
            string path = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "CodeShellManager", "crash.log");
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            File.AppendAllText(path, $"[{DateTime.Now:HH:mm:ss.fff}] OpenWith: {message}\n");
        }
        catch { /* logger failure is not actionable */ }
    }
}

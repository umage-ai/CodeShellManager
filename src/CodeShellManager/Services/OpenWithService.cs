using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.RegularExpressions;
using System.Threading.Tasks;
using CodeShellManager.Models;

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
    /// <summary>The command line as CreateProcess will see it — for the editor preview and error messages. Never executed.</summary>
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

    /// <summary>
    /// Characters cmd.exe interprets in a batch file's command line, quoted or not. <c>%</c>
    /// and <c>!</c> expand variables, <c>&amp; | &lt; &gt;</c> chain and redirect, <c>^</c>
    /// escapes, <c>"</c> toggles cmd's quote state, <c>( )</c> group, and <c>, ; =</c> are
    /// argument delimiters to a batch script.
    /// </summary>
    public const string BatchMetaChars = "%!&|<>^\"(),;=";

    /// <summary>
    /// Programs that run text as code. An "editor" that is really a shell turns a template
    /// into a command-execution primitive, and the list arrives from imports. Checked by file
    /// name, case-insensitively.
    /// </summary>
    private static readonly HashSet<string> ShellLikeNames = new(StringComparer.OrdinalIgnoreCase)
    {
        "cmd", "powershell", "powershell_ise", "pwsh", "wsl", "bash", "sh", "wscript", "cscript",
        "mshta", "rundll32", "regsvr32", "msiexec", "conhost", "wt", "schtasks", "forfiles",
        "runas", "start", "reg", "certutil", "bitsadmin",
    };

    private static readonly Regex PlaceholderRegex = new(@"\{([A-Za-z]+)\}", RegexOptions.CultureInvariant);

    public const int MaxEntries = 50;

    // ── Executable ────────────────────────────────────────────────────────────────────────

    public static bool IsBatch(string? path)
    {
        string ext = Path.GetExtension(path ?? "");
        return ext.Equals(".cmd", StringComparison.OrdinalIgnoreCase)
            || ext.Equals(".bat", StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>
    /// Null when <paramref name="path"/> is acceptable as an entry's program, else the reason.
    /// Pure — does not check that the file exists (that happens at launch). Rules: an absolute
    /// local path (no relative path, so nothing is resolved against PATH or the cwd; no UNC, so
    /// nothing runs from a network share), a <c>.exe</c> — or a <c>.cmd</c>/<c>.bat</c> only when
    /// <paramref name="allowBatch"/> — and not a shell or script host.
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

        string ext = Path.GetExtension(path);
        if (IsBatch(path))
        {
            if (!allowBatch)
                return "Batch files (.cmd/.bat) run through cmd.exe and are not accepted here — point at the program's .exe.";
        }
        else if (!ext.Equals(".exe", StringComparison.OrdinalIgnoreCase))
        {
            return "The program must be an .exe.";
        }

        if (ShellLikeNames.Contains(Path.GetFileNameWithoutExtension(path)))
            return $"{Path.GetFileName(path)} is a shell or script host, not an editor — it is not accepted.";
        return null;
    }

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

    /// <summary>Null when the template is usable for <paramref name="kind"/>, else the reason. Blank is valid (= not offered for that kind).</summary>
    public static string? ValidateTemplate(string? template, SessionKind kind)
    {
        if (string.IsNullOrWhiteSpace(template)) return null;
        if (template.Any(char.IsControl)) return "The arguments contain control characters.";
        var tokens = Tokenize(template);
        if (tokens == null) return "Unbalanced quote in the arguments.";
        var allowed = PlaceholdersByKind[kind];
        foreach (var token in tokens)
        {
            foreach (Match m in PlaceholderRegex.Matches(token))
            {
                string name = m.Groups[1].Value;
                if (!allowed.Contains(name, StringComparer.Ordinal))
                    return $"{{{name}}} is not available for {KindName(kind)} sessions (use {string.Join(", ", allowed.Select(a => "{" + a + "}"))}).";
            }
        }
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
    /// file are checked by <see cref="LaunchAsync"/>).
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
            // rather than passed as "": an SSH session with no remote folder opens the host,
            // not a folder called "".
            if (matches.Count == 1 && matches[0].Value == token && values[matches[0].Groups[1].Value].Length == 0)
                continue;

            foreach (Match m in matches)
            {
                string name = m.Groups[1].Value;
                string? valueError = ValidateValue(name, values[name]);
                if (valueError != null) return (null, valueError);
            }
            args.Add(PlaceholderRegex.Replace(token, m => values[m.Groups[1].Value]));
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
        // An option-shaped value (-x, --foo) could be taken by the editor as a flag. Applies
        // anywhere in a token, which is stricter than needed but never wrong for a path,
        // host or distro.
        if (value.StartsWith('-'))
            return $"The session's {name} starts with '-', which the program could read as an option.";
        switch (name)
        {
            case "folder":
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

    // ── Selection ─────────────────────────────────────────────────────────────────────────

    public static IEnumerable<OpenWithEntry> ApplicableTo(IEnumerable<OpenWithEntry>? list, SessionKind kind) =>
        (list ?? []).Where(e => e != null && e.AppliesTo(kind));

    /// <summary>The default entry when it applies to this kind; otherwise the first entry that does. Null when none does.</summary>
    public static OpenWithEntry? DefaultFor(IEnumerable<OpenWithEntry>? list, SessionKind kind)
    {
        var applicable = ApplicableTo(list, kind).ToList();
        return applicable.FirstOrDefault(e => e.IsDefault) ?? applicable.FirstOrDefault();
    }

    // ── Sanitize (state.json / import) ────────────────────────────────────────────────────

    /// <summary>
    /// Returns a cleaned copy of a list that came from disk: entries with an unacceptable
    /// program are dropped; templates that don't validate are cleared (and an entry left with
    /// none is dropped); labels and ids are filled in; exactly one default. state.json passes
    /// <paramref name="allowBatch"/> = true (the user may have chosen a batch file in the
    /// editor); an import passes false — see "Open with…" in CLAUDE.md.
    /// </summary>
    public static List<OpenWithEntry> Sanitize(IEnumerable<OpenWithEntry?>? list, bool allowBatch, Action<string>? log = null)
    {
        var result = new List<OpenWithEntry>();
        var ids = new HashSet<string>(StringComparer.Ordinal);
        foreach (var raw in list ?? [])
        {
            if (raw == null) continue;
            if (result.Count >= MaxEntries) { log?.Invoke($"dropped entries beyond {MaxEntries}"); break; }
            var e = raw.Clone();
            e.Executable = e.Executable?.Trim() ?? "";
            string? exeError = ValidateExecutable(e.Executable, allowBatch);
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
    /// to their Electron exe a few folders up; any other batch file is scanned for a quoted
    /// absolute path to an existing <c>.exe</c> (JetBrains Toolbox scripts have that shape).
    /// A non-batch path is returned unchanged. Null when nothing was found. Touches the disk.
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
            foreach (Match m in Regex.Matches(File.ReadAllText(path), "\"([A-Za-z]:\\\\[^\"\\r\\n]+?\\.exe)\"", RegexOptions.IgnoreCase))
            {
                string exe = m.Groups[1].Value;
                if (ValidateExecutable(exe, allowBatch: false) == null && File.Exists(exe)) return exe;
            }
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
    /// Everything <see cref="LaunchAsync"/> does short of starting the process: resolves a
    /// WSL <c>~</c>, checks the folder is an existing directory (only when the template uses
    /// <c>{folder}</c>), checks the program file exists, and builds the argv. Touches the disk.
    /// </summary>
    internal static async Task<(OpenWithCommand? Command, string? Error)> PrepareAsync(OpenWithEntry entry, OpenWithContext ctx)
    {
        string? template = entry.ArgsFor(ctx.Kind);
        bool usesFolder = template?.Contains("{folder}", StringComparison.Ordinal) == true;

        if (ctx.Kind == SessionKind.Wsl && template?.Contains("{linuxPath}", StringComparison.Ordinal) == true)
        {
            // A blank Linux folder means $HOME (see "WSL Sessions"); editors want a real path.
            string lp = ctx.LinuxPath?.Trim() ?? "";
            if (lp.Length == 0 || lp == "~" || lp.StartsWith("~/", StringComparison.Ordinal))
            {
                string? home = await WslDiscoveryService.GetDistroHomeAsync(ctx.Distro, ctx.WslUser).ConfigureAwait(false);
                if (home != null)
                    ctx = ctx with { LinuxPath = lp.Length <= 1 ? home : home.TrimEnd('/') + lp[1..] };
                else if (lp.Length > 0)
                    ctx = ctx with { LinuxPath = "" };   // leave it to the editor's default rather than pass a literal ~
            }
        }

        if (usesFolder)
        {
            // Same check as Open in Explorer: the folder comes from state.json and imports,
            // so it must be an existing directory, normalised to a Windows path.
            string? target = FolderOpener.Target(ctx.Folder);
            if (target == null) return (null, $"The session's folder doesn't exist: {ctx.Folder}");
            ctx = ctx with { Folder = target };
        }

        var built = Build(entry, ctx);
        if (built.Error != null) return built;
        if (!File.Exists(entry.Executable))
            return (null, $"{entry.Label}: program not found at {entry.Executable}. Edit the entry under Open with → Edit…");
        return built;
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

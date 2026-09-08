using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// One installed WSL distro as reported by <c>wsl -l -v</c>.
/// </summary>
/// <param name="Name">Distro name (matches the <c>-d</c> argument to wsl.exe).</param>
/// <param name="Version">WSL version (1 or 2). 0 if the column failed to parse.</param>
/// <param name="IsDefault">True for the distro flagged with <c>*</c> in the listing.</param>
/// <param name="State">Reported lifecycle state, e.g. "Running", "Stopped".</param>
public record WslDistro(string Name, int Version, bool IsDefault, string State);

/// <summary>
/// Enumerates WSL distros installed on the current Windows host. Returns an empty list
/// when wsl.exe is missing or returns an error (e.g. no distros installed).
/// </summary>
public static class WslDiscoveryService
{
    /// <summary>
    /// Returns the currently installed distros. The result is suitable for populating
    /// a UI picker; the default distro (if any) is marked via <see cref="WslDistro.IsDefault"/>.
    /// Never throws — every failure mode collapses to an empty list.
    /// </summary>
    public static async Task<IReadOnlyList<WslDistro>> GetDistrosAsync()
    {
        if (!OperatingSystem.IsWindows()) return Array.Empty<WslDistro>();

        try
        {
            var psi = new ProcessStartInfo("wsl.exe")
            {
                // -l -v is the verbose listing. --quiet is intentionally NOT used so we
                // get the header row and the asterisk marker for the default distro.
                Arguments = "-l -v",
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
                // wsl.exe writes its listings as UTF-16 LE (the same as PowerShell's
                // default). Without this override we'd read each character interleaved
                // with NUL bytes and the parser would see gibberish.
                StandardOutputEncoding = Encoding.Unicode,
                StandardErrorEncoding = Encoding.Unicode,
            };

            var (stdout, _, exit) = await RunWslCaptureAsync(psi, 3000).ConfigureAwait(false);
            if (exit != 0) return Array.Empty<WslDistro>();

            return Parse(stdout);
        }
        catch (Exception)
        {
            // Honor the "Never throws" contract: every failure mode (wsl.exe absent,
            // I/O hiccup, transient process error) collapses to an empty list so the
            // dialog's Loaded handler never crashes the picker. Specific causes were
            // previously caught individually (Win32Exception for missing wsl.exe,
            // FileNotFoundException) but Process.Start + the read pipeline can throw
            // a wider set than that.
            return Array.Empty<WslDistro>();
        }
    }

    /// <summary>
    /// Parses the body of <c>wsl -l -v</c>. Exposed for testing.
    /// </summary>
    internal static IReadOnlyList<WslDistro> Parse(string raw)
    {
        if (string.IsNullOrWhiteSpace(raw)) return Array.Empty<WslDistro>();

        var results = new List<WslDistro>();
        foreach (var line in raw.Replace("\r", "").Split('\n'))
        {
            if (string.IsNullOrWhiteSpace(line)) continue;

            // Header row: "  NAME                   STATE           VERSION".
            // Detect by the presence of the literal "NAME" token and skip.
            if (line.TrimStart().StartsWith("NAME", StringComparison.Ordinal)) continue;

            var tokens = line.Split(new[] { ' ' }, StringSplitOptions.RemoveEmptyEntries);

            bool isDefault = tokens.Length > 0 && tokens[0] == "*";
            int firstNameIdx = isDefault ? 1 : 0;

            // `wsl -l -v` always emits three columns: NAME, STATE, VERSION. NAME can
            // contain spaces if the user `wsl --import`'d a distro with one (rare but
            // legal), so consume from the end instead of the start: last token is
            // VERSION, second-to-last is STATE, anything in between is the name.
            if (tokens.Length - firstNameIdx < 3) continue;

            int versionIdx = tokens.Length - 1;
            int stateIdx = tokens.Length - 2;
            string name = string.Join(' ', tokens, firstNameIdx, stateIdx - firstNameIdx);
            string state = tokens[stateIdx];

            // A real row's VERSION column is always an integer. The header's is the word
            // "VERSION" — which the literal "NAME" check above only catches on an English
            // Windows; a localized header would otherwise land here as a phantom distro
            // with Version = 0. Requiring a parseable version is language-independent.
            if (!int.TryParse(tokens[versionIdx], out int version)) continue;

            results.Add(new WslDistro(name, version, isDefault, state));
        }
        // Stable ordering: default first, then alphabetical.
        return results
            .Where(d => !IsDockerInternalDistro(d.Name))
            .OrderByDescending(d => d.IsDefault)
            .ThenBy(d => d.Name, StringComparer.OrdinalIgnoreCase)
            .ToList();
    }

    /// <summary>
    /// True for Docker Desktop's own internal distros ("docker-desktop" and, on older
    /// versions, "docker-desktop-data") — BusyBox-based plumbing that Docker rebuilds on
    /// its own updates, not a user environment. They have no bash (and are root-only), so
    /// offering them in the picker just hands the user a confusing "bash: not found" the
    /// first time they try to use one — which is exactly what happened during manual
    /// testing here. Exact, case-insensitive match only: a user-imported distro that merely
    /// *contains* the phrase (e.g. "my-docker-desktop-clone") must still be offered.
    /// </summary>
    private static bool IsDockerInternalDistro(string name) =>
        string.Equals(name, "docker-desktop", StringComparison.OrdinalIgnoreCase)
        || string.Equals(name, "docker-desktop-data", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// Resolves the home directory inside a WSL distro for the given user (or the distro's
    /// default user when <paramref name="user"/> is null/empty). Cached per (distro, user) —
    /// shells out once via <c>wsl -d &lt;distro&gt; [-u &lt;user&gt;] -- sh -c "cd ~ &amp;&amp; pwd"</c>
    /// then returns the cached value on subsequent calls. Returns null on failure
    /// (WSL not running, command timeout, or non-zero exit).
    /// </summary>
    public static async Task<string?> GetDistroHomeAsync(string distro, string? user = null)
    {
        if (string.IsNullOrWhiteSpace(distro)) return null;
        string normalizedUser = user?.Trim() ?? "";
        string key = $"{distro}|{normalizedUser}";
        lock (_homeCache)
        {
            if (_homeCache.TryGetValue(key, out var cached)) return cached;
        }

        try
        {
            // QuoteForCmd for parity with the WSL arg builders — distro and user are
            // usually space-free but Parse now accepts space-containing names, so the
            // launcher side must not break on the same input.
            string args = $"-d {Models.ShellSession.QuoteForCmd(distro)}";
            if (!string.IsNullOrEmpty(normalizedUser))
                args += $" -u {Models.ShellSession.QuoteForCmd(normalizedUser)}";
            // -e (not --) for the same reason BuildWslArgs and GetLoginShellAsync use it:
            // `--` runs the tail through the distro's default login shell first, expanding
            // the payload twice. See ShellSession.BuildWslArgs.
            args += " -e sh -c \"cd ~ && pwd\"";

            var psi = new ProcessStartInfo("wsl.exe")
            {
                Arguments = args,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
                StandardOutputEncoding = Encoding.UTF8,
                StandardErrorEncoding = Encoding.UTF8,
            };
            var (stdout, _, exit) = await RunWslCaptureAsync(psi, 3000).ConfigureAwait(false);
            if (exit != 0) return null;

            string home = stdout.Trim();
            if (string.IsNullOrEmpty(home)) return null;
            lock (_homeCache) _homeCache[key] = home;
            return home;
        }
        catch (Exception) { return null; }
    }

    private static readonly Dictionary<string, string> _homeCache = new();

    /// <summary>
    /// Resolves the login shell to use inside a WSL distro — "bash" when it's present,
    /// "sh" otherwise (minimal distros like Alpine/BusyBox images or Docker Desktop's own
    /// "docker-desktop" distro have no bash, so hardcoding it fails every session and run
    /// command there). Cached per (distro, user) exactly like <see cref="GetDistroHomeAsync"/>.
    /// Never throws; any failure (WSL not running, timeout, non-zero exit) returns "bash" —
    /// that preserves today's behaviour rather than silently downgrading a distro that
    /// actually works.
    /// </summary>
    public static async Task<string> GetLoginShellAsync(string distro, string? user = null)
    {
        if (string.IsNullOrWhiteSpace(distro)) return "bash";
        string normalizedUser = user?.Trim() ?? "";
        string key = $"{distro}|{normalizedUser}";
        lock (_shellCache)
        {
            if (_shellCache.TryGetValue(key, out var cached)) return cached;
        }

        try
        {
            string args = $"-d {Models.ShellSession.QuoteForCmd(distro)}";
            if (!string.IsNullOrEmpty(normalizedUser))
                args += $" -u {Models.ShellSession.QuoteForCmd(normalizedUser)}";
            // -e (not --) for the same reason BuildWslArgs uses it — see ShellSession.
            args += " -e sh -c \"command -v bash >/dev/null 2>&1 && echo bash || echo sh\"";

            var psi = new ProcessStartInfo("wsl.exe")
            {
                Arguments = args,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
                StandardOutputEncoding = Encoding.UTF8,
                StandardErrorEncoding = Encoding.UTF8,
            };
            var (stdout, _, exit) = await RunWslCaptureAsync(psi, 3000).ConfigureAwait(false);
            if (exit != 0) return "bash";

            string result = stdout.Trim();
            string shell = result == "sh" ? "sh" : "bash";
            lock (_shellCache) _shellCache[key] = shell;
            return shell;
        }
        catch (Exception) { return "bash"; }
    }

    private static readonly Dictionary<string, string> _shellCache = new();

    /// <summary>
    /// Runs a prepared wsl.exe probe and captures both streams, entirely off the calling
    /// thread. Returns exit -1 for "did not run or did not finish in time".
    ///
    /// Task.Run is the point of this helper. Process.Start is synchronous and sits before
    /// the first await, so without it process creation ran on whichever thread called in —
    /// and every caller here is the UI thread (the New Session dialog's Loaded/Start
    /// handlers, and LaunchSessionAsync inside the restore loop). That is the exact defect
    /// issue #70 fixed in GitService, and wsl.exe is the worse offender: it can boot a
    /// stopped distro VM, which is seconds, not milliseconds. See CLAUDE.md, "Never spawn a
    /// process on the UI thread".
    ///
    /// Both streams are always drained: awaiting only stdout lets a chatty stderr fill its
    /// pipe buffer and wedge the child until the timeout.
    /// </summary>
    private static Task<(string stdout, string stderr, int exit)> RunWslCaptureAsync(
        ProcessStartInfo psi, int timeoutMs) => Task.Run(async () =>
    {
        using var process = Process.Start(psi);
        if (process is null) return ("", "", -1);

        var outTask = process.StandardOutput.ReadToEndAsync();
        var errTask = process.StandardError.ReadToEndAsync();
        var bothTask = Task.WhenAll(outTask, errTask);

        var completed = await Task.WhenAny(bothTask, Task.Delay(timeoutMs)).ConfigureAwait(false);
        if (completed != bothTask)
        {
            try { process.Kill(); } catch { }
            return ("", "", -1);
        }

        try { await process.WaitForExitAsync().ConfigureAwait(false); } catch { }

        string stdout = outTask.IsCompletedSuccessfully ? outTask.Result : "";
        string stderr = errTask.IsCompletedSuccessfully ? errTask.Result : "";
        return (stdout, stderr, process.HasExited ? process.ExitCode : -1);
    });

    /// <summary>
    /// Converts a WSL distro + Linux-style path to the Windows UNC view of that path
    /// (<c>\\wsl$\Ubuntu\home\alice</c>). Used by GitService and PseudoTerminal's
    /// working-directory argument so Windows-native tools can read the WSL filesystem.
    /// Returns an empty string when either input is empty.
    /// </summary>
    public static string ToUncPath(string distro, string linuxPath)
    {
        if (string.IsNullOrWhiteSpace(distro)) return "";
        if (string.IsNullOrWhiteSpace(linuxPath)) return $@"\\wsl$\{distro}";
        string trimmed = linuxPath.TrimStart('/').Replace('/', '\\');
        return $@"\\wsl$\{distro}\{trimmed}";
    }

    /// <summary>
    /// Re-derives <see cref="ShellSession.WorkingFolder"/> from
    /// <see cref="ShellSession.WslDistro"/> + <see cref="ShellSession.WslWorkingFolder"/> when
    /// the session is WSL — the "UNC mirror invariant" (see CLAUDE.md "WSL Sessions"). A no-op
    /// for Local/Ssh sessions. One shared helper so every path that creates or edits a WSL
    /// session — dialog creation, duplicate/worktree, session-config edit, reopen-from-history —
    /// derives <see cref="ShellSession.WorkingFolder"/> the same way instead of trusting a value
    /// that may have been hand-edited or copied from a stale source (e.g. state.json,
    /// RecentlyClosed, an import file).
    /// </summary>
    public static void ResyncWslWorkingFolder(ShellSession session)
    {
        if (session.Kind != SessionKind.Wsl) return;
        session.WorkingFolder = ToUncPath((session.WslDistro ?? "").Trim(), session.WslWorkingFolder);
    }

    /// <summary>
    /// Splits a WSL UNC (<c>\\wsl$\Ubuntu\home\alice</c> or <c>\\wsl.localhost\…</c>, either
    /// slash direction) into (distro, linuxPath). linuxPath is "/" for the distro root.
    /// Returns (null, "") for anything that isn't a WSL UNC. The single parser for the
    /// whole app — GitService and NewSessionDialog both delegate here.
    /// </summary>
    public static (string? distro, string linuxPath) TryParseUncPath(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return (null, "");
        string normalized = path.Replace('/', '\\').TrimEnd('\\');
        foreach (var prefix in new[] { @"\\wsl$\", @"\\wsl.localhost\" })
        {
            if (!normalized.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) continue;
            string rest = normalized[prefix.Length..];
            if (string.IsNullOrEmpty(rest)) return (null, "");
            int slash = rest.IndexOf('\\');
            string distro = slash < 0 ? rest : rest[..slash];
            if (string.IsNullOrEmpty(distro)) return (null, "");
            string linuxRest = slash < 0 ? "" : rest[(slash + 1)..];
            return (distro, string.IsNullOrEmpty(linuxRest) ? "/" : "/" + linuxRest.Replace('\\', '/'));
        }
        return (null, "");
    }
}

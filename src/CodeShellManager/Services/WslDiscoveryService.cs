using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Text;
using System.Threading.Tasks;

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

            using var process = Process.Start(psi);
            if (process is null) return Array.Empty<WslDistro>();

            var outTask = process.StandardOutput.ReadToEndAsync();
            var bothTask = Task.WhenAll(outTask, process.StandardError.ReadToEndAsync());
            var completed = await Task.WhenAny(bothTask, Task.Delay(3000));
            if (completed != bothTask)
            {
                try { process.Kill(); } catch { }
                return Array.Empty<WslDistro>();
            }
            try { await process.WaitForExitAsync(); } catch { }
            if (process.ExitCode != 0) return Array.Empty<WslDistro>();

            return Parse(outTask.Result);
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
            int.TryParse(tokens[versionIdx], out int version);

            results.Add(new WslDistro(name, version, isDefault, state));
        }
        // Stable ordering: default first, then alphabetical.
        return results
            .OrderByDescending(d => d.IsDefault)
            .ThenBy(d => d.Name, StringComparer.OrdinalIgnoreCase)
            .ToList();
    }

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
            args += " -- sh -c \"cd ~ && pwd\"";

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
            using var process = Process.Start(psi);
            if (process is null) return null;

            // Drain BOTH stdout and stderr. If we only awaited stdout, a chatty
            // wsl.exe error (e.g. distro stopped, transient init message) could
            // fill the stderr pipe buffer and block the child — the stdout await
            // would never complete and we'd silently fall through to the timeout.
            var outTask = process.StandardOutput.ReadToEndAsync();
            var errTask = process.StandardError.ReadToEndAsync();
            var bothTask = Task.WhenAll(outTask, errTask);
            var completed = await Task.WhenAny(bothTask, Task.Delay(3000));
            if (completed != bothTask) { try { process.Kill(); } catch { } return null; }
            try { await process.WaitForExitAsync(); } catch { }
            if (process.ExitCode != 0) return null;

            string home = outTask.Result.Trim();
            if (string.IsNullOrEmpty(home)) return null;
            lock (_homeCache) _homeCache[key] = home;
            return home;
        }
        catch (Exception) { return null; }
    }

    private static readonly Dictionary<string, string> _homeCache = new();

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
}

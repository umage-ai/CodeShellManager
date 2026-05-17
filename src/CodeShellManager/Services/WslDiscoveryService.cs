using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
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
        catch (Win32Exception)
        {
            // wsl.exe not on PATH — WSL feature isn't installed.
            return Array.Empty<WslDistro>();
        }
        catch (FileNotFoundException)
        {
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
            if (tokens.Length < 2) continue;

            bool isDefault = tokens[0] == "*";
            int idx = isDefault ? 1 : 0;
            if (tokens.Length - idx < 1) continue;

            string name = tokens[idx];
            string state = tokens.Length - idx >= 2 ? tokens[idx + 1] : "";
            int version = 0;
            if (tokens.Length - idx >= 3) int.TryParse(tokens[idx + 2], out version);

            results.Add(new WslDistro(name, version, isDefault, state));
        }
        // Stable ordering: default first, then alphabetical.
        return results
            .OrderByDescending(d => d.IsDefault)
            .ThenBy(d => d.Name, StringComparer.OrdinalIgnoreCase)
            .ToList();
    }

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

using System;
using System.IO;
using System.Net.Http;
using System.Reflection;
using System.Text.Json;
using System.Threading.Tasks;

namespace CodeShellManager.Services;

/// <summary>
/// Checks GitHub Releases for a newer version of the app.
/// Results are cached locally for 24 hours so startup is instant on repeat runs.
/// </summary>
public static class UpdateService
{
    private const string ApiUrl =
        "https://api.github.com/repos/umage-ai/CodeShellManager/releases/latest";

    private static readonly string CachePath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "CodeShellManager", "update-cache.json");

    private const int CacheHours = 24;

    // ── Public API ────────────────────────────────────────────────────────────

    public record UpdateResult(string CurrentVersion, string LatestVersion, string ReleaseUrl);

    /// <summary>
    /// Returns an <see cref="UpdateResult"/> when a newer release exists, otherwise null.
    /// Never throws — network/parse errors are swallowed silently.
    /// </summary>
    public static async Task<UpdateResult?> CheckAsync()
    {
        try
        {
            string current = GetCurrentVersion();
            var (tag, url) = await FetchLatestTagAsync();
            if (tag == null) return null;

            string latestClean = tag.TrimStart('v');
            if (!Version.TryParse(latestClean, out var latest)) return null;
            if (!Version.TryParse(PadVersion(current), out var cur)) return null;

            return latest > cur
                ? new UpdateResult(current, latestClean, url ?? string.Empty)
                : null;
        }
        catch { return null; }
    }

    /// <summary>Returns the running assembly version as "Major.Minor.Build".</summary>
    public static string GetCurrentVersion()
    {
        var v = Assembly.GetEntryAssembly()?.GetName().Version;
        if (v == null) return "0.0.0";
        return $"{v.Major}.{v.Minor}.{v.Build}";
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    private static async Task<(string? tag, string? url)> FetchLatestTagAsync()
    {
        // Try cache first — avoids a network round-trip on every launch
        var cached = await LoadCacheAsync();
        if (cached != null && (DateTime.UtcNow - cached.CheckedAt).TotalHours < CacheHours)
            return (cached.Tag, cached.Url);

        // Fetch from GitHub Releases API
        try
        {
            using var http = new HttpClient();
            http.DefaultRequestHeaders.UserAgent.ParseAdd(
                $"CodeShellManager/{GetCurrentVersion()} (+https://github.com/umage-ai/CodeShellManager)");
            http.Timeout = TimeSpan.FromSeconds(10);

            var json = await http.GetStringAsync(ApiUrl);
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;

            string? tag = root.TryGetProperty("tag_name", out var tn) ? tn.GetString() : null;
            string? url = root.TryGetProperty("html_url",  out var hu) ? hu.GetString() : null;

            // Persist so next startup uses the cache
            if (tag != null)
                await SaveCacheAsync(new UpdateCache(DateTime.UtcNow, tag, url ?? string.Empty));

            return (tag, url);
        }
        catch
        {
            // If we had a stale cache entry, still return it rather than nothing
            return (cached?.Tag, cached?.Url);
        }
    }

    // ── Cache ─────────────────────────────────────────────────────────────────

    private record UpdateCache(DateTime CheckedAt, string Tag, string Url);

    private static async Task<UpdateCache?> LoadCacheAsync()
    {
        try
        {
            if (!File.Exists(CachePath)) return null;
            var json = await File.ReadAllTextAsync(CachePath);
            return JsonSerializer.Deserialize<UpdateCache>(json);
        }
        catch { return null; }
    }

    private static async Task SaveCacheAsync(UpdateCache cache)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(CachePath)!);
            await File.WriteAllTextAsync(CachePath,
                JsonSerializer.Serialize(cache, new JsonSerializerOptions { WriteIndented = false }));
        }
        catch { }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// <summary>Pad a short version string to at least 3 parts so Version.TryParse succeeds.</summary>
    private static string PadVersion(string v)
    {
        var parts = v.Split('.');
        return parts.Length switch
        {
            1 => $"{v}.0.0",
            2 => $"{v}.0",
            _ => v
        };
    }
}

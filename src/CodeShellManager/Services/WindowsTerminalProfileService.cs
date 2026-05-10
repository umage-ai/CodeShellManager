using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text.Json;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// Reads Windows Terminal settings.json (Stable / Preview / Unpackaged) and
/// produces flattened, xterm-mapped <see cref="WindowsTerminalProfile"/>
/// instances. Errors are swallowed; unreadable or malformed files yield an
/// empty enumeration.
/// </summary>
public static class WindowsTerminalProfileService
{
    private static readonly (string Source, string Path)[] KnownPaths =
    {
        ("Stable", System.IO.Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Packages", "Microsoft.WindowsTerminal_8wekyb3d8bbwe", "LocalState", "settings.json")),
        ("Preview", System.IO.Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Packages", "Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe", "LocalState", "settings.json")),
        ("Unpackaged", System.IO.Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Microsoft", "Windows Terminal", "settings.json")),
    };

    public static IReadOnlyList<WindowsTerminalProfile> GetProfiles()
    {
        var all = new List<WindowsTerminalProfile>();
        foreach (var (source, path) in KnownPaths)
            all.AddRange(ParseFile(path, source));

        // Disambiguate display names when multiple sources yield the same name
        var byName = all.GroupBy(p => p.Name);
        var result = new List<WindowsTerminalProfile>(all.Count);
        foreach (var group in byName)
        {
            if (group.Count() == 1) { result.Add(group.First()); continue; }
            foreach (var p in group)
            {
                result.Add(new WindowsTerminalProfile
                {
                    Guid = p.Guid, Name = $"{p.Name} ({p.Source})", Source = p.Source,
                    Commandline = p.Commandline, StartingDirectory = p.StartingDirectory,
                    FontFamily = p.FontFamily, FontSize = p.FontSize, FontWeight = p.FontWeight,
                    FontLigatures = p.FontLigatures, CursorShape = p.CursorShape, CursorBlink = p.CursorBlink,
                    Padding = p.Padding, BackgroundOpacity = p.BackgroundOpacity,
                    RetroEffect = p.RetroEffect, ColorSchemeJson = p.ColorSchemeJson,
                });
            }
        }
        return result;
    }

    public static IEnumerable<WindowsTerminalProfile> ParseFile(string path, string source)
    {
        if (!File.Exists(path)) yield break;
        JsonDocument? doc = null;
        try
        {
            doc = JsonDocument.Parse(File.ReadAllText(path),
                new JsonDocumentOptions { CommentHandling = JsonCommentHandling.Skip, AllowTrailingCommas = true });
        }
        catch { yield break; }

        using (doc)
        {
            var root = doc.RootElement;
            if (!root.TryGetProperty("profiles", out var profilesEl)) yield break;

            JsonElement defaults = default;
            bool hasDefaults = profilesEl.TryGetProperty("defaults", out defaults)
                && defaults.ValueKind == JsonValueKind.Object;

            if (!profilesEl.TryGetProperty("list", out var list) || list.ValueKind != JsonValueKind.Array)
                yield break;

            var schemes = new Dictionary<string, JsonElement>(StringComparer.OrdinalIgnoreCase);
            if (root.TryGetProperty("schemes", out var schemesEl) && schemesEl.ValueKind == JsonValueKind.Array)
            {
                foreach (var scheme in schemesEl.EnumerateArray())
                {
                    if (scheme.TryGetProperty("name", out var n) && n.ValueKind == JsonValueKind.String)
                        schemes[n.GetString()!] = scheme.Clone();
                }
            }

            foreach (var profile in list.EnumerateArray())
            {
                if (Get<bool>(profile, "hidden") == true) continue;

                var built = BuildProfile(profile, hasDefaults ? defaults : (JsonElement?)null, schemes, source);
                if (built != null) yield return built;
            }
        }
    }

    private static WindowsTerminalProfile? BuildProfile(
        JsonElement profile, JsonElement? defaults,
        Dictionary<string, JsonElement> schemes, string source)
    {
        string name = GetString(profile, "name") ?? "";
        if (string.IsNullOrEmpty(name)) return null;

        // Expand env vars (e.g. %SystemRoot%) — Win32 CreateProcess does not, so an
        // unexpanded path lands as ERROR_FILE_NOT_FOUND when we hit PseudoTerminal.
        string commandline = Environment.ExpandEnvironmentVariables(
            GetMerged(profile, defaults, "commandline") ?? "cmd.exe");
        string startingDirectory = ExpandStartingDirectory(GetMerged(profile, defaults, "startingDirectory") ?? "");

        var (cursorStyle, forcedBlink) = CursorShapeMapper.Map(GetMerged(profile, defaults, "cursorShape"));

        string? padding = PaddingParser.Parse(GetMerged(profile, defaults, "padding") ?? "");

        double opacity = GetDoubleMerged(profile, defaults, "opacity") ?? 1.0;
        bool useAcrylic = GetBoolMerged(profile, defaults, "useAcrylic") ?? false;
        double? backgroundOpacity = (useAcrylic || opacity < 1.0) ? opacity : (double?)null;

        bool? retro = GetBoolMerged(profile, defaults, "experimental.retroTerminalEffect");

        string? schemeName = GetMerged(profile, defaults, "colorScheme");
        JsonElement? scheme = null;
        if (!string.IsNullOrEmpty(schemeName))
        {
            if (schemes.TryGetValue(schemeName, out var found)) scheme = found;
            else scheme = BuiltInTerminalSchemes.Lookup(schemeName);
        }
        string? colorSchemeJson = SchemeMapper.ToXtermThemeJson(scheme, opacity);

        var (face, size, weight, ligatures) = ResolveFont(profile, defaults);

        return new WindowsTerminalProfile
        {
            Guid = GetString(profile, "guid") ?? "",
            Name = name,
            Source = source,
            Commandline = commandline,
            StartingDirectory = startingDirectory,
            FontFamily = face,
            FontSize = size,
            FontWeight = weight,
            FontLigatures = ligatures,
            CursorShape = cursorStyle,
            CursorBlink = forcedBlink,
            Padding = padding,
            BackgroundOpacity = backgroundOpacity,
            RetroEffect = retro,
            ColorSchemeJson = colorSchemeJson,
        };
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    private static string? GetString(JsonElement el, string name) =>
        el.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static T? Get<T>(JsonElement el, string name) where T : struct
    {
        if (!el.TryGetProperty(name, out var v)) return null;
        if (typeof(T) == typeof(bool) && (v.ValueKind == JsonValueKind.True || v.ValueKind == JsonValueKind.False))
            return (T)(object)v.GetBoolean();
        if (typeof(T) == typeof(int) && v.ValueKind == JsonValueKind.Number && v.TryGetInt32(out var i))
            return (T)(object)i;
        if (typeof(T) == typeof(double) && v.ValueKind == JsonValueKind.Number && v.TryGetDouble(out var d))
            return (T)(object)d;
        return null;
    }

    private static string? GetMerged(JsonElement profile, JsonElement? defaults, string name) =>
        GetString(profile, name) ?? (defaults.HasValue ? GetString(defaults.Value, name) : null);

    private static bool? GetBoolMerged(JsonElement profile, JsonElement? defaults, string name) =>
        Get<bool>(profile, name) ?? (defaults.HasValue ? Get<bool>(defaults.Value, name) : null);

    private static double? GetDoubleMerged(JsonElement profile, JsonElement? defaults, string name) =>
        Get<double>(profile, name) ?? (defaults.HasValue ? Get<double>(defaults.Value, name) : null);

    private static (string? Face, int? Size, string? Weight, bool? Ligatures) ResolveFont(
        JsonElement profile, JsonElement? defaults)
    {
        JsonElement? Pick(string parent, string child)
        {
            if (profile.TryGetProperty(parent, out var pParent)
                && pParent.ValueKind == JsonValueKind.Object
                && pParent.TryGetProperty(child, out var pChild))
                return pChild;
            if (defaults.HasValue
                && defaults.Value.TryGetProperty(parent, out var dParent)
                && dParent.ValueKind == JsonValueKind.Object
                && dParent.TryGetProperty(child, out var dChild))
                return dChild;
            return null;
        }

        string? face = Pick("font", "face") is { ValueKind: JsonValueKind.String } f ? f.GetString() : null;
        int? size = Pick("font", "size") is { ValueKind: JsonValueKind.Number } s && s.TryGetInt32(out var iv) ? iv : null;
        string? weight = Pick("font", "weight") switch
        {
            { ValueKind: JsonValueKind.String } w => w.GetString(),
            { ValueKind: JsonValueKind.Number } w => w.GetInt32().ToString(),
            _ => null,
        };

        bool? ligatures = null;
        var calt = Pick("font", "features") is { ValueKind: JsonValueKind.Object } features
            && features.TryGetProperty("calt", out var caltEl) ? caltEl : (JsonElement?)null;
        if (calt.HasValue && calt.Value.ValueKind == JsonValueKind.Number
            && calt.Value.TryGetInt32(out int caltInt))
            ligatures = caltInt != 0;

        return (face, size, weight, ligatures);
    }

    private static string ExpandStartingDirectory(string dir)
    {
        if (string.IsNullOrEmpty(dir)) return "";
        if (dir.StartsWith("~")) dir = "%USERPROFILE%" + dir[1..];
        return Environment.ExpandEnvironmentVariables(dir);
    }
}

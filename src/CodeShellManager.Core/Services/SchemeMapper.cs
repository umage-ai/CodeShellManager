using System.Collections.Generic;
using System.Globalization;
using System.Text.Json;

namespace CodeShellManager.Services;

/// <summary>
/// Translates a Windows Terminal scheme object into an xterm.js theme JSON
/// string. Renames <c>purple</c> → <c>magenta</c> and (when opacity &lt; 1.0) rewrites
/// <c>background</c> from <c>#RRGGBB</c> to <c>rgba(r, g, b, alpha)</c>.
/// </summary>
public static class SchemeMapper
{
    private static readonly Dictionary<string, string> KeyMap = new()
    {
        ["background"] = "background",
        ["foreground"] = "foreground",
        ["cursorColor"] = "cursor",
        ["selectionBackground"] = "selectionBackground",
        ["black"] = "black",
        ["red"] = "red",
        ["green"] = "green",
        ["yellow"] = "yellow",
        ["blue"] = "blue",
        ["purple"] = "magenta",
        ["cyan"] = "cyan",
        ["white"] = "white",
        ["brightBlack"] = "brightBlack",
        ["brightRed"] = "brightRed",
        ["brightGreen"] = "brightGreen",
        ["brightYellow"] = "brightYellow",
        ["brightBlue"] = "brightBlue",
        ["brightPurple"] = "brightMagenta",
        ["brightCyan"] = "brightCyan",
        ["brightWhite"] = "brightWhite",
    };

    public static string? ToXtermThemeJson(JsonElement? scheme, double opacity)
    {
        if (scheme is null) return null;
        var src = scheme.Value;
        if (src.ValueKind != JsonValueKind.Object) return null;

        var theme = new Dictionary<string, string>();
        foreach (var kv in KeyMap)
        {
            if (src.TryGetProperty(kv.Key, out var prop) && prop.ValueKind == JsonValueKind.String)
                theme[kv.Value] = prop.GetString()!;
        }

        if (opacity < 1.0 && theme.TryGetValue("background", out var bg))
            theme["background"] = HexToRgba(bg, opacity);

        return JsonSerializer.Serialize(theme);
    }

    private static string HexToRgba(string hex, double opacity)
    {
        if (hex.Length != 7 || hex[0] != '#') return hex;
        if (!byte.TryParse(hex.AsSpan(1, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out byte r)) return hex;
        if (!byte.TryParse(hex.AsSpan(3, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out byte g)) return hex;
        if (!byte.TryParse(hex.AsSpan(5, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out byte b)) return hex;
        return string.Format(CultureInfo.InvariantCulture, "rgba({0}, {1}, {2}, {3})", r, g, b, opacity);
    }
}

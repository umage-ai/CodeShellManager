using System.Windows.Media;

namespace CodeShellManager.Services;

public static class ColorService
{
    // 12 colours spaced ~30° apart on the HSL wheel, all at similar saturation/lightness
    // so every project folder gets a clearly distinct accent colour.
    private static readonly string[] Palette =
    [
        "#FF6B6B",  // red
        "#FF9E42",  // orange
        "#FFD166",  // yellow
        "#AEDE68",  // lime
        "#51CF66",  // green
        "#38D9A9",  // emerald
        "#66D9E8",  // cyan
        "#4DABF7",  // blue
        "#748FFC",  // indigo
        "#9775FA",  // violet
        "#F783AC",  // pink
        "#FF6B95",  // rose
    ];

    public static string GetHexColor(string folderPath)
    {
        uint hash = Fnv1a(folderPath.ToLowerInvariant().TrimEnd('/', '\\'));
        return Palette[hash % (uint)Palette.Length];
    }

    public static System.Windows.Media.Color GetColor(string folderPath)
    {
        return (System.Windows.Media.Color)System.Windows.Media.ColorConverter.ConvertFromString(GetHexColor(folderPath));
    }

    public static SolidColorBrush GetBrush(string folderPath) =>
        new(GetColor(folderPath));

    private static uint Fnv1a(string s)
    {
        uint hash = 2166136261u;
        foreach (char c in s) { hash ^= (byte)c; hash *= 16777619u; }
        return hash;
    }
}

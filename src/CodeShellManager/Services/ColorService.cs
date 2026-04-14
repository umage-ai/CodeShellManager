using System.Windows.Media;

namespace CodeShellManager.Services;

public static class ColorService
{
    private static readonly string[] Palette =
    [
        "#E57373", "#64B5F6", "#81C784", "#BA68C8",
        "#FFB74D", "#4DB6AC", "#F06292", "#A1887F",
        "#90A4AE", "#DCE775", "#FF8A65", "#4FC3F7",
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

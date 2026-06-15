using System.Linq;

namespace CodeShellManager.Services;

/// <summary>
/// Parses Windows Terminal <c>padding</c> shorthand into CSS-shorthand.
/// Accepts 1, 2, or 4 numbers separated by commas.
/// </summary>
public static class PaddingParser
{
    public static string? Parse(string input)
    {
        if (string.IsNullOrWhiteSpace(input)) return null;
        var parts = input.Split(',');
        if (parts.Length is not (1 or 2 or 4)) return null;

        var nums = new int[parts.Length];
        for (int i = 0; i < parts.Length; i++)
        {
            if (!int.TryParse(parts[i].Trim(), out nums[i])) return null;
            if (nums[i] < 0) return null;
        }
        return string.Join(' ', nums.Select(n => $"{n}px"));
    }
}

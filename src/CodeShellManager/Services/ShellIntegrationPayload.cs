using System;
using System.Text;

namespace CodeShellManager.Services;

/// <summary>
/// Validation and normalisation rules for the OSC 9001 shell-integration channel
/// (see <c>docs/shell-integration.md</c>). Kept WPF-free so the rules are unit-testable.
/// <para>
/// Every value here is untrusted: it arrives from whatever is printing to the terminal —
/// a remote host, a <c>cat</c> of an arbitrary file, a hook — and some of it ends up
/// persisted in <c>state.json</c>. Reject what cannot be validated, trim and cap the rest.
/// </para>
/// </summary>
public static class ShellIntegrationPayload
{
    /// <summary>Upper bound on a title pushed through OSC 9001. Long enough for any
    /// sensible tab name; short enough that a runaway program can't bloat state.json
    /// or the sidebar row.</summary>
    public const int MaxTitleLength = 80;

    /// <summary>
    /// Cap for an OSC 9001 <c>git-branch</c> value. Generous next to any real ref name —
    /// this bounds a hostile or buggy emitter, it does not police legitimate branches.
    /// </summary>
    public const int MaxBranchLength = 200;

    /// <summary>
    /// Accepts <c>#rgb</c>, <c>#rrggbb</c> and <c>#rrggbbaa</c>. Returns the string in the
    /// form WPF's <c>ColorConverter</c> expects: 3- and 6-digit values unchanged, 8-digit
    /// values reordered from the integrator convention (alpha last) to <c>#aarrggbb</c>
    /// (alpha first). Anything else — named colours, <c>rgb()</c>, wrong length, non-hex —
    /// is rejected.
    /// </summary>
    public static bool TryNormalizeColor(string? input, out string? wpfHex)
    {
        wpfHex = null;
        if (string.IsNullOrEmpty(input) || input[0] != '#') return false;
        if (input.Length != 4 && input.Length != 7 && input.Length != 9) return false;
        for (int i = 1; i < input.Length; i++)
            if (!Uri.IsHexDigit(input[i])) return false;

        wpfHex = input.Length == 9
            ? "#" + input[7..9] + input[1..7]
            : input;
        return true;
    }

    /// <summary>Only <c>1</c> and <c>true</c> (any case) mean dirty; everything else is clean.</summary>
    public static bool ParseDirty(string? input)
        => input == "1" || string.Equals(input, "true", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// Strips control characters, trims, and caps at <see cref="MaxTitleLength"/> without
    /// splitting a surrogate pair. Returns <c>null</c> when nothing usable is left, so the
    /// caller can leave the existing name alone rather than blank it.
    /// </summary>
    public static string? SanitizeTitle(string? input)
    {
        string? clean = StripControls(input);
        if (clean is null) return null;
        if (clean.Length <= MaxTitleLength) return clean;

        int cut = MaxTitleLength;
        if (char.IsHighSurrogate(clean[cut - 1])) cut--;
        return clean[..cut].TrimEnd();
    }

    /// <summary>
    /// Strips control characters, trims, and caps length. Returns <c>null</c> for an empty
    /// result, which the caller treats as "no branch" (detached HEAD, not a repo).
    ///
    /// The cap is not cosmetic. This value comes from whatever printed to the terminal, and
    /// it is rendered into a sidebar row; an unbounded branch let any program wedge the UI
    /// with a megabyte-long "branch name". Titles were already capped — branches were not.
    /// Git's own ref names are far below this limit, so nothing legitimate is truncated.
    /// </summary>
    public static string? SanitizeBranch(string? input)
    {
        string? clean = StripControls(input);
        if (clean is null) return null;
        if (clean.Length <= MaxBranchLength) return clean;

        int cut = MaxBranchLength;
        if (char.IsHighSurrogate(clean[cut - 1])) cut--;
        string trimmed = clean[..cut].TrimEnd();
        return trimmed.Length == 0 ? null : trimmed;
    }

    private static string? StripControls(string? input)
    {
        if (string.IsNullOrWhiteSpace(input)) return null;
        var sb = new StringBuilder(input.Length);
        foreach (char c in input)
            if (!char.IsControl(c)) sb.Append(c);
        string result = sb.ToString().Trim();
        return result.Length == 0 ? null : result;
    }
}

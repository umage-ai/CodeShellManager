namespace CodeShellManager.Models;

/// <summary>
/// A Windows Terminal profile flattened with its inherited defaults and with
/// appearance fields already mapped to xterm.js equivalents.
/// </summary>
public sealed class WindowsTerminalProfile
{
    public string Guid { get; init; } = "";
    public string Name { get; init; } = "";
    public string Source { get; init; } = "";    // "Stable" | "Preview" | "Unpackaged"

    public string Commandline { get; init; } = "";
    public string StartingDirectory { get; init; } = "";

    public string? FontFamily { get; init; }
    public int? FontSize { get; init; }
    public string? FontWeight { get; init; }
    public bool? FontLigatures { get; init; }
    public string? CursorShape { get; init; }    // already mapped to xterm style
    public bool? CursorBlink { get; init; }
    public string? Padding { get; init; }         // CSS shorthand
    public double? BackgroundOpacity { get; init; }
    public bool? RetroEffect { get; init; }
    public string? ColorSchemeJson { get; init; } // pre-baked xterm theme JSON
}

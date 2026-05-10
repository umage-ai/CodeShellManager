namespace CodeShellManager.Services;

/// <summary>
/// Maps Windows Terminal `cursorShape` values to xterm.js `cursorStyle` (and
/// optionally a forced `cursorBlink` value when no exact equivalent exists).
/// </summary>
public static class CursorShapeMapper
{
    public static (string? style, bool? blink) Map(string? wtShape) => wtShape switch
    {
        "bar"              => ("bar", null),
        "filledBox"        => ("block", null),
        "vintage"          => ("block", null),
        "emptyBox"         => ("block", false),  // closest visual approximation
        "underscore"       => ("underline", null),
        "doubleUnderscore" => ("underline", null),
        _ => (null, null),
    };
}

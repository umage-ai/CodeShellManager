# Windows Terminal Profile Import — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in setting to import Windows Terminal profiles into the New Session dialog, with per-session font/cursor/padding/transparency/retro/color-scheme overrides applied to xterm.js.

**Architecture:** A new `WindowsTerminalProfileService` reads `settings.json` from one of three known locations and emits `WindowsTerminalProfile` POCOs with WT fields already mapped to xterm equivalents. The New Session dialog gains an optional Profile combobox that pre-fills folder/command/name and stamps appearance overrides onto the resulting `ShellSession`. After bridge init, `TerminalBridge.ApplyProfileOverrides(session)` posts a `setOptions` message that extends the existing handler in `terminal.html`. Transparent sessions navigate to a sibling `terminal-transparent.html` because xterm's `allowTransparency` must be set in the constructor.

**Tech Stack:** .NET 10 / WPF / WebView2, xunit, System.Text.Json, xterm.js.

**Spec reference:** `docs/superpowers/specs/2026-05-08-windows-terminal-profiles-design.md`

---

## File overview

### Created

| File | Responsibility |
|---|---|
| `src/CodeShellManager/Models/WindowsTerminalProfile.cs` | POCO: profile metadata + already-mapped xterm overrides |
| `src/CodeShellManager/Services/CommandLineSplitter.cs` | Quote-aware `Split(string) → (exe, args)` |
| `src/CodeShellManager/Services/SchemeMapper.cs` | Map a WT scheme JsonElement → xterm theme JSON string |
| `src/CodeShellManager/Services/BuiltInTerminalSchemes.cs` | Static lookup of WT-shipped schemes |
| `src/CodeShellManager/Services/WindowsTerminalProfileService.cs` | Discover settings.json; parse; flatten defaults; produce profiles |
| `src/CodeShellManager/Assets/terminal-init.js` | Shared xterm/init JS used by both HTML entries |
| `src/CodeShellManager/Assets/terminal-transparent.html` | Transparent variant constructor (`allowTransparency: true`) |
| `tests/CodeShellManager.Tests/Fixtures/wt/happy.json` | Fixture: two profiles, one scheme |
| `tests/CodeShellManager.Tests/Fixtures/wt/hidden.json` | Fixture: profile with `hidden: true` |
| `tests/CodeShellManager.Tests/Fixtures/wt/inheritance.json` | Fixture: defaults supplies `commandline`/`fontFace` |
| `tests/CodeShellManager.Tests/Fixtures/wt/malformed.json` | Fixture: invalid JSON |
| `tests/CodeShellManager.Tests/CommandLineSplitterTests.cs` | Unit tests |
| `tests/CodeShellManager.Tests/SchemeMapperTests.cs` | Unit tests |
| `tests/CodeShellManager.Tests/BuiltInTerminalSchemesTests.cs` | Unit tests |
| `tests/CodeShellManager.Tests/WindowsTerminalProfileServiceTests.cs` | Unit tests against fixtures |
| `tests/CodeShellManager.Tests/PaddingParserTests.cs` | Unit tests |
| `tests/CodeShellManager.Tests/CursorShapeMapperTests.cs` | Unit tests |

### Modified

| File | Change |
|---|---|
| `src/CodeShellManager/Models/AppState.cs` | Add `AppSettings.ImportWindowsTerminalProfiles` |
| `src/CodeShellManager/Models/ShellSession.cs` | Add nullable per-session override fields |
| `src/CodeShellManager/Terminal/TerminalBridge.cs` | Add `ApplyProfileOverrides(ShellSession)`, expose `bool UseTransparentHtml` so callers can pick the right asset path |
| `src/CodeShellManager/Assets/terminal.html` | Strip script body to `terminal-init.js`; add `cursorStyle / cursorBlink / theme / padding / retro` to setOptions; add retro CSS |
| `src/CodeShellManager/Views/NewSessionDialog.xaml` | New `ProfilePanel` row above Working Folder |
| `src/CodeShellManager/Views/NewSessionDialog.xaml.cs` | Constructor accepts profiles; `ProfileCombo_SelectionChanged` pre-fills fields and exposes overrides |
| `src/CodeShellManager/Views/SettingsWindow.xaml` | New checkbox in APPEARANCE section |
| `src/CodeShellManager/Views/SettingsWindow.xaml.cs` | Wire checkbox to `ImportWindowsTerminalProfiles` |
| `src/CodeShellManager/MainWindow.xaml.cs` | `OpenNewSessionDialog` passes profiles when setting on; copy override fields to session; `LaunchSessionAsync` picks `terminal-transparent.html` when needed; calls `ApplyProfileOverrides` after `ApplyFontSettings` |
| `src/CodeShellManager/CodeShellManager.csproj` | Add `terminal-init.js` and `terminal-transparent.html` to Content list |

---

## Task ordering rationale

We work bottom-up: pure helpers with no dependencies first (so they're easy to TDD), then the service that uses them, then the model + setting, then UI plumbing, then bridge wiring, then end-to-end. Each layer is independently committable.

---

### Task 1: ShellSession override fields + AppSettings flag

**Files:**
- Modify: `src/CodeShellManager/Models/ShellSession.cs`
- Modify: `src/CodeShellManager/Models/AppState.cs`
- Modify: `src/CodeShellManager/Views/SettingsWindow.xaml.cs:23-45` (clone block)

- [ ] **Step 1: Add nullable override fields to `ShellSession.cs`**

Insert after the existing SSH fields block (before `FullCommandLine`):

```csharp
// Per-session appearance overrides (typically populated from a Windows
// Terminal profile via NewSessionDialog). All nullable — null means "use the
// global terminal settings". Persisted to state.json so a session relaunches
// with the same look.
public string? ProfileFontFamily { get; set; }
public int? ProfileFontSize { get; set; }
public string? ProfileFontWeight { get; set; }
public bool? ProfileFontLigatures { get; set; }
public string? ProfileCursorShape { get; set; }
public bool? ProfileCursorBlink { get; set; }
public string? ProfilePadding { get; set; }
public double? ProfileBackgroundOpacity { get; set; }
public bool? ProfileRetroEffect { get; set; }
/// <summary>Pre-baked xterm theme object (JSON), or null for xterm default.</summary>
public string? ProfileColorSchemeJson { get; set; }
```

- [ ] **Step 2: Add the setting field to `AppSettings`**

In `src/CodeShellManager/Models/AppState.cs` after `ShowTerminalStatusDot` (line 18):

```csharp
public bool ImportWindowsTerminalProfiles { get; set; } = false;
```

- [ ] **Step 3: Update SettingsWindow clone block**

In `src/CodeShellManager/Views/SettingsWindow.xaml.cs`, in the `_edited = new AppSettings { ... }` block (lines 23-45), add:

```csharp
ImportWindowsTerminalProfiles = current.ImportWindowsTerminalProfiles,
```

- [ ] **Step 4: Build to verify nothing else broke**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: build succeeds.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Models/ShellSession.cs src/CodeShellManager/Models/AppState.cs src/CodeShellManager/Views/SettingsWindow.xaml.cs
git commit -m "feat: add ShellSession profile override fields and ImportWindowsTerminalProfiles setting"
```

---

### Task 2: CommandLineSplitter

**Files:**
- Create: `src/CodeShellManager/Services/CommandLineSplitter.cs`
- Test: `tests/CodeShellManager.Tests/CommandLineSplitterTests.cs`

- [ ] **Step 1: Write failing tests**

Create `tests/CodeShellManager.Tests/CommandLineSplitterTests.cs`:

```csharp
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class CommandLineSplitterTests
{
    [Fact]
    public void Split_UnquotedExeWithArgs_ReturnsExeAndRest()
    {
        var (exe, args) = CommandLineSplitter.Split("cmd.exe /k foo");
        Assert.Equal("cmd.exe", exe);
        Assert.Equal("/k foo", args);
    }

    [Fact]
    public void Split_QuotedExeWithSpaces_StripsQuotes()
    {
        var (exe, args) = CommandLineSplitter.Split("\"C:\\Program Files\\app.exe\" -x");
        Assert.Equal("C:\\Program Files\\app.exe", exe);
        Assert.Equal("-x", args);
    }

    [Fact]
    public void Split_ExeOnly_ReturnsEmptyArgs()
    {
        var (exe, args) = CommandLineSplitter.Split("pwsh");
        Assert.Equal("pwsh", exe);
        Assert.Equal("", args);
    }

    [Fact]
    public void Split_WslWithFlags_ReturnsExeAndArgs()
    {
        var (exe, args) = CommandLineSplitter.Split("wsl.exe -d Ubuntu");
        Assert.Equal("wsl.exe", exe);
        Assert.Equal("-d Ubuntu", args);
    }

    [Fact]
    public void Split_ArgsContainQuotes_PreservesArgsVerbatim()
    {
        var (exe, args) = CommandLineSplitter.Split("bash -c \"echo hello\"");
        Assert.Equal("bash", exe);
        Assert.Equal("-c \"echo hello\"", args);
    }

    [Fact]
    public void Split_EmptyOrWhitespace_ReturnsEmpty()
    {
        var (exe, args) = CommandLineSplitter.Split("   ");
        Assert.Equal("", exe);
        Assert.Equal("", args);
    }

    [Fact]
    public void Split_LeadingWhitespace_Trims()
    {
        var (exe, args) = CommandLineSplitter.Split("  cmd /c echo hi");
        Assert.Equal("cmd", exe);
        Assert.Equal("/c echo hi", args);
    }
}
```

- [ ] **Step 2: Run tests and verify they fail**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~CommandLineSplitterTests"`
Expected: FAIL with "type or namespace `CommandLineSplitter` could not be found".

- [ ] **Step 3: Implement `CommandLineSplitter.cs`**

Create `src/CodeShellManager/Services/CommandLineSplitter.cs`:

```csharp
namespace CodeShellManager.Services;

/// <summary>
/// Splits a Windows-style commandline into (exe, args) using a simple
/// quote-aware single-pass scanner. The exe is unquoted; the args are returned
/// verbatim (including any internal quoting).
/// </summary>
public static class CommandLineSplitter
{
    public static (string exe, string args) Split(string commandLine)
    {
        if (string.IsNullOrWhiteSpace(commandLine)) return ("", "");
        var s = commandLine.TrimStart();

        if (s.StartsWith('"'))
        {
            int closing = s.IndexOf('"', 1);
            if (closing < 0) return (s.Trim('"'), "");
            string exe = s.Substring(1, closing - 1);
            string rest = s.Length > closing + 1 ? s[(closing + 1)..].TrimStart() : "";
            return (exe, rest);
        }

        int sp = s.IndexOf(' ');
        if (sp < 0) return (s, "");
        return (s[..sp], s[(sp + 1)..].TrimStart());
    }
}
```

- [ ] **Step 4: Run tests and verify they pass**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~CommandLineSplitterTests"`
Expected: 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Services/CommandLineSplitter.cs tests/CodeShellManager.Tests/CommandLineSplitterTests.cs
git commit -m "feat: add CommandLineSplitter with quote-aware exe/args parsing"
```

---

### Task 3: PaddingParser

The padding parser is small enough to live as a static method; we'll put it in a `Services/PaddingParser.cs` file for unit testability rather than burying it in the WT service.

**Files:**
- Create: `src/CodeShellManager/Services/PaddingParser.cs`
- Test: `tests/CodeShellManager.Tests/PaddingParserTests.cs`

- [ ] **Step 1: Write failing tests**

Create `tests/CodeShellManager.Tests/PaddingParserTests.cs`:

```csharp
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class PaddingParserTests
{
    [Theory]
    [InlineData("8", "8px")]
    [InlineData("0", "0px")]
    [InlineData("8, 12", "8px 12px")]
    [InlineData("4, 8, 4, 8", "4px 8px 4px 8px")]
    [InlineData("  6  ,7  ", "6px 7px")]
    [InlineData("8,8,8,8", "8px 8px 8px 8px")]
    public void Parse_ValidShorthand_ReturnsCss(string input, string expected)
        => Assert.Equal(expected, PaddingParser.Parse(input));

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("abc")]
    [InlineData("8, 8, 8")]              // unsupported 3-value form
    [InlineData("8, 8, 8, 8, 8")]        // too many values
    public void Parse_InvalidOrUnsupported_ReturnsNull(string input)
        => Assert.Null(PaddingParser.Parse(input));
}
```

- [ ] **Step 2: Run tests and verify they fail**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~PaddingParserTests"`
Expected: FAIL with "type or namespace `PaddingParser` could not be found".

- [ ] **Step 3: Implement `PaddingParser.cs`**

Create `src/CodeShellManager/Services/PaddingParser.cs`:

```csharp
using System.Linq;

namespace CodeShellManager.Services;

/// <summary>
/// Parses Windows Terminal `padding` shorthand into CSS-shorthand.
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
```

- [ ] **Step 4: Run tests and verify they pass**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~PaddingParserTests"`
Expected: all theory rows pass.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Services/PaddingParser.cs tests/CodeShellManager.Tests/PaddingParserTests.cs
git commit -m "feat: add PaddingParser for Windows Terminal padding shorthand"
```

---

### Task 4: CursorShapeMapper

Same shape: small static helper with focused tests.

**Files:**
- Create: `src/CodeShellManager/Services/CursorShapeMapper.cs`
- Test: `tests/CodeShellManager.Tests/CursorShapeMapperTests.cs`

- [ ] **Step 1: Write failing tests**

Create `tests/CodeShellManager.Tests/CursorShapeMapperTests.cs`:

```csharp
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class CursorShapeMapperTests
{
    [Theory]
    [InlineData("bar", "bar", null)]
    [InlineData("filledBox", "block", null)]
    [InlineData("vintage", "block", null)]
    [InlineData("emptyBox", "block", false)]
    [InlineData("underscore", "underline", null)]
    [InlineData("doubleUnderscore", "underline", null)]
    public void Map_KnownShapes_ReturnsExpected(string wtShape, string expectedStyle, bool? expectedBlink)
    {
        var (style, blink) = CursorShapeMapper.Map(wtShape);
        Assert.Equal(expectedStyle, style);
        Assert.Equal(expectedBlink, blink);
    }

    [Theory]
    [InlineData("")]
    [InlineData(null)]
    [InlineData("nonsense")]
    public void Map_UnknownOrEmpty_ReturnsNullStyle(string? wtShape)
    {
        var (style, blink) = CursorShapeMapper.Map(wtShape);
        Assert.Null(style);
        Assert.Null(blink);
    }
}
```

- [ ] **Step 2: Run tests and verify they fail**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~CursorShapeMapperTests"`
Expected: FAIL.

- [ ] **Step 3: Implement `CursorShapeMapper.cs`**

Create `src/CodeShellManager/Services/CursorShapeMapper.cs`:

```csharp
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
```

- [ ] **Step 4: Run tests and verify they pass**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~CursorShapeMapperTests"`
Expected: all theory rows pass.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Services/CursorShapeMapper.cs tests/CodeShellManager.Tests/CursorShapeMapperTests.cs
git commit -m "feat: add CursorShapeMapper for WT → xterm cursor shape translation"
```

---

### Task 5: SchemeMapper

`SchemeMapper.ToXtermThemeJson(JsonElement scheme, double opacity)` takes a single WT scheme object as a parsed `JsonElement` and produces an xterm theme JSON string. Opacity is applied at this stage by rewriting `background` to an `rgba()` string.

**Files:**
- Create: `src/CodeShellManager/Services/SchemeMapper.cs`
- Test: `tests/CodeShellManager.Tests/SchemeMapperTests.cs`

- [ ] **Step 1: Write failing tests**

Create `tests/CodeShellManager.Tests/SchemeMapperTests.cs`:

```csharp
using System.Text.Json;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class SchemeMapperTests
{
    private static JsonElement Parse(string json)
        => JsonDocument.Parse(json).RootElement.Clone();

    [Fact]
    public void Map_BasicScheme_ProducesXtermKeys()
    {
        var scheme = Parse("""
            {
              "name": "Demo",
              "background": "#0C0C0C",
              "foreground": "#CCCCCC",
              "cursorColor": "#FFFFFF",
              "selectionBackground": "#264F78",
              "black": "#000000", "red": "#C50F1F", "green": "#13A10E",
              "yellow": "#C19C00", "blue": "#0037DA", "purple": "#881798",
              "cyan": "#3A96DD", "white": "#CCCCCC",
              "brightBlack": "#767676", "brightRed": "#E74856",
              "brightGreen": "#16C60C", "brightYellow": "#F9F1A5",
              "brightBlue": "#3B78FF", "brightPurple": "#B4009E",
              "brightCyan": "#61D6D6", "brightWhite": "#F2F2F2"
            }
            """);

        string json = SchemeMapper.ToXtermThemeJson(scheme, opacity: 1.0)!;
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;

        Assert.Equal("#0C0C0C", root.GetProperty("background").GetString());
        Assert.Equal("#CCCCCC", root.GetProperty("foreground").GetString());
        Assert.Equal("#FFFFFF", root.GetProperty("cursor").GetString());
        Assert.Equal("#264F78", root.GetProperty("selectionBackground").GetString());
        Assert.Equal("#881798", root.GetProperty("magenta").GetString());
        Assert.Equal("#B4009E", root.GetProperty("brightMagenta").GetString());
        Assert.False(root.TryGetProperty("purple", out _));
        Assert.False(root.TryGetProperty("brightPurple", out _));
    }

    [Fact]
    public void Map_MissingCursor_OmitsKey()
    {
        var scheme = Parse("""{ "name":"x", "background":"#000", "foreground":"#FFF" }""");
        string json = SchemeMapper.ToXtermThemeJson(scheme, 1.0)!;
        using var doc = JsonDocument.Parse(json);
        Assert.False(doc.RootElement.TryGetProperty("cursor", out _));
    }

    [Fact]
    public void Map_Opacity_RewritesBackgroundAsRgba()
    {
        var scheme = Parse("""{ "name":"x", "background":"#0C0C0C", "foreground":"#FFFFFF" }""");
        string json = SchemeMapper.ToXtermThemeJson(scheme, opacity: 0.8)!;
        using var doc = JsonDocument.Parse(json);
        Assert.Equal("rgba(12, 12, 12, 0.8)", doc.RootElement.GetProperty("background").GetString());
    }

    [Fact]
    public void Map_Null_ReturnsNull()
        => Assert.Null(SchemeMapper.ToXtermThemeJson(null, 1.0));
}
```

- [ ] **Step 2: Run tests and verify they fail**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~SchemeMapperTests"`
Expected: FAIL.

- [ ] **Step 3: Implement `SchemeMapper.cs`**

Create `src/CodeShellManager/Services/SchemeMapper.cs`:

```csharp
using System.Collections.Generic;
using System.Globalization;
using System.Text.Json;

namespace CodeShellManager.Services;

/// <summary>
/// Translates a Windows Terminal scheme object into an xterm.js theme JSON
/// string. Renames `purple` → `magenta` and (when opacity &lt; 1.0) rewrites
/// `background` from `#RRGGBB` to `rgba(r, g, b, alpha)`.
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
        // Accept "#RRGGBB"; if anything else, return unchanged.
        if (hex.Length != 7 || hex[0] != '#') return hex;
        if (!byte.TryParse(hex.AsSpan(1, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out byte r)) return hex;
        if (!byte.TryParse(hex.AsSpan(3, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out byte g)) return hex;
        if (!byte.TryParse(hex.AsSpan(5, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out byte b)) return hex;
        return string.Format(CultureInfo.InvariantCulture, "rgba({0}, {1}, {2}, {3})", r, g, b, opacity);
    }
}
```

- [ ] **Step 4: Run tests and verify they pass**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~SchemeMapperTests"`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Services/SchemeMapper.cs tests/CodeShellManager.Tests/SchemeMapperTests.cs
git commit -m "feat: add SchemeMapper for WT scheme → xterm theme translation"
```

---

### Task 6: BuiltInTerminalSchemes

A small static lookup of WT-shipped schemes for which the user's settings.json doesn't repeat the definition. We embed the JSON as a const string and parse on demand. We ship Campbell, Campbell Powershell, Vintage, One Half Dark, One Half Light, Solarized Dark, Solarized Light, Tango Dark, Tango Light.

**Files:**
- Create: `src/CodeShellManager/Services/BuiltInTerminalSchemes.cs`
- Test: `tests/CodeShellManager.Tests/BuiltInTerminalSchemesTests.cs`

- [ ] **Step 1: Write failing tests**

Create `tests/CodeShellManager.Tests/BuiltInTerminalSchemesTests.cs`:

```csharp
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class BuiltInTerminalSchemesTests
{
    [Theory]
    [InlineData("Campbell")]
    [InlineData("Campbell Powershell")]
    [InlineData("Vintage")]
    [InlineData("One Half Dark")]
    [InlineData("One Half Light")]
    [InlineData("Solarized Dark")]
    [InlineData("Solarized Light")]
    [InlineData("Tango Dark")]
    [InlineData("Tango Light")]
    public void Lookup_BuiltInName_ReturnsScheme(string name)
        => Assert.NotNull(BuiltInTerminalSchemes.Lookup(name));

    [Theory]
    [InlineData("nonsense")]
    [InlineData("")]
    public void Lookup_UnknownName_ReturnsNull(string name)
        => Assert.Null(BuiltInTerminalSchemes.Lookup(name));

    [Fact]
    public void Lookup_BuiltInScheme_HasBackgroundField()
    {
        var scheme = BuiltInTerminalSchemes.Lookup("Campbell")!.Value;
        Assert.True(scheme.TryGetProperty("background", out var bg));
        Assert.StartsWith("#", bg.GetString());
    }
}
```

- [ ] **Step 2: Run tests and verify they fail**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~BuiltInTerminalSchemesTests"`
Expected: FAIL.

- [ ] **Step 3: Implement `BuiltInTerminalSchemes.cs`**

Create `src/CodeShellManager/Services/BuiltInTerminalSchemes.cs`:

```csharp
using System.Collections.Generic;
using System.Text.Json;

namespace CodeShellManager.Services;

/// <summary>
/// Static lookup of color schemes that ship with Windows Terminal but are not
/// usually duplicated in a user's settings.json. Color values copied from the
/// Microsoft.Terminal repository defaults.
/// </summary>
public static class BuiltInTerminalSchemes
{
    private static readonly Dictionary<string, string> Schemes = new()
    {
        ["Campbell"] = """
            {"name":"Campbell","background":"#0C0C0C","foreground":"#CCCCCC","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#0C0C0C","red":"#C50F1F","green":"#13A10E","yellow":"#C19C00","blue":"#0037DA","purple":"#881798","cyan":"#3A96DD","white":"#CCCCCC",
             "brightBlack":"#767676","brightRed":"#E74856","brightGreen":"#16C60C","brightYellow":"#F9F1A5","brightBlue":"#3B78FF","brightPurple":"#B4009E","brightCyan":"#61D6D6","brightWhite":"#F2F2F2"}
            """,
        ["Campbell Powershell"] = """
            {"name":"Campbell Powershell","background":"#012456","foreground":"#CCCCCC","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#0C0C0C","red":"#C50F1F","green":"#13A10E","yellow":"#C19C00","blue":"#0037DA","purple":"#881798","cyan":"#3A96DD","white":"#CCCCCC",
             "brightBlack":"#767676","brightRed":"#E74856","brightGreen":"#16C60C","brightYellow":"#F9F1A5","brightBlue":"#3B78FF","brightPurple":"#B4009E","brightCyan":"#61D6D6","brightWhite":"#F2F2F2"}
            """,
        ["Vintage"] = """
            {"name":"Vintage","background":"#000000","foreground":"#C0C0C0","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#000000","red":"#800000","green":"#008000","yellow":"#808000","blue":"#000080","purple":"#800080","cyan":"#008080","white":"#C0C0C0",
             "brightBlack":"#808080","brightRed":"#FF0000","brightGreen":"#00FF00","brightYellow":"#FFFF00","brightBlue":"#0000FF","brightPurple":"#FF00FF","brightCyan":"#00FFFF","brightWhite":"#FFFFFF"}
            """,
        ["One Half Dark"] = """
            {"name":"One Half Dark","background":"#282C34","foreground":"#DCDFE4","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#282C34","red":"#E06C75","green":"#98C379","yellow":"#E5C07B","blue":"#61AFEF","purple":"#C678DD","cyan":"#56B6C2","white":"#DCDFE4",
             "brightBlack":"#5A6374","brightRed":"#E06C75","brightGreen":"#98C379","brightYellow":"#E5C07B","brightBlue":"#61AFEF","brightPurple":"#C678DD","brightCyan":"#56B6C2","brightWhite":"#DCDFE4"}
            """,
        ["One Half Light"] = """
            {"name":"One Half Light","background":"#FAFAFA","foreground":"#383A42","cursorColor":"#4F525D","selectionBackground":"#FFFFFF",
             "black":"#383A42","red":"#E45649","green":"#50A14F","yellow":"#C18301","blue":"#0184BC","purple":"#A626A4","cyan":"#0997B3","white":"#FAFAFA",
             "brightBlack":"#4F525D","brightRed":"#DF6C75","brightGreen":"#98C379","brightYellow":"#E4C07A","brightBlue":"#61AFEF","brightPurple":"#C577DD","brightCyan":"#56B5C1","brightWhite":"#FFFFFF"}
            """,
        ["Solarized Dark"] = """
            {"name":"Solarized Dark","background":"#002B36","foreground":"#839496","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#002B36","red":"#DC322F","green":"#859900","yellow":"#B58900","blue":"#268BD2","purple":"#D33682","cyan":"#2AA198","white":"#EEE8D5",
             "brightBlack":"#073642","brightRed":"#CB4B16","brightGreen":"#586E75","brightYellow":"#657B83","brightBlue":"#839496","brightPurple":"#6C71C4","brightCyan":"#93A1A1","brightWhite":"#FDF6E3"}
            """,
        ["Solarized Light"] = """
            {"name":"Solarized Light","background":"#FDF6E3","foreground":"#657B83","cursorColor":"#002B36","selectionBackground":"#FFFFFF",
             "black":"#002B36","red":"#DC322F","green":"#859900","yellow":"#B58900","blue":"#268BD2","purple":"#D33682","cyan":"#2AA198","white":"#EEE8D5",
             "brightBlack":"#073642","brightRed":"#CB4B16","brightGreen":"#586E75","brightYellow":"#657B83","brightBlue":"#839496","brightPurple":"#6C71C4","brightCyan":"#93A1A1","brightWhite":"#FDF6E3"}
            """,
        ["Tango Dark"] = """
            {"name":"Tango Dark","background":"#000000","foreground":"#D3D7CF","cursorColor":"#FFFFFF","selectionBackground":"#FFFFFF",
             "black":"#000000","red":"#CC0000","green":"#4E9A06","yellow":"#C4A000","blue":"#3465A4","purple":"#75507B","cyan":"#06989A","white":"#D3D7CF",
             "brightBlack":"#555753","brightRed":"#EF2929","brightGreen":"#8AE234","brightYellow":"#FCE94F","brightBlue":"#729FCF","brightPurple":"#AD7FA8","brightCyan":"#34E2E2","brightWhite":"#EEEEEC"}
            """,
        ["Tango Light"] = """
            {"name":"Tango Light","background":"#FFFFFF","foreground":"#555753","cursorColor":"#000000","selectionBackground":"#FFFFFF",
             "black":"#000000","red":"#CC0000","green":"#4E9A06","yellow":"#C4A000","blue":"#3465A4","purple":"#75507B","cyan":"#06989A","white":"#D3D7CF",
             "brightBlack":"#555753","brightRed":"#EF2929","brightGreen":"#8AE234","brightYellow":"#FCE94F","brightBlue":"#729FCF","brightPurple":"#AD7FA8","brightCyan":"#34E2E2","brightWhite":"#EEEEEC"}
            """,
    };

    public static JsonElement? Lookup(string? name)
    {
        if (string.IsNullOrEmpty(name)) return null;
        if (!Schemes.TryGetValue(name, out var json)) return null;
        return JsonDocument.Parse(json).RootElement.Clone();
    }
}
```

- [ ] **Step 4: Run tests and verify they pass**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~BuiltInTerminalSchemesTests"`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/Services/BuiltInTerminalSchemes.cs tests/CodeShellManager.Tests/BuiltInTerminalSchemesTests.cs
git commit -m "feat: add BuiltInTerminalSchemes with WT-shipped color schemes"
```

---

### Task 7: WindowsTerminalProfile model

**Files:**
- Create: `src/CodeShellManager/Models/WindowsTerminalProfile.cs`

- [ ] **Step 1: Create the POCO**

Create `src/CodeShellManager/Models/WindowsTerminalProfile.cs`:

```csharp
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
```

- [ ] **Step 2: Build to verify**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: build succeeds.

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/Models/WindowsTerminalProfile.cs
git commit -m "feat: add WindowsTerminalProfile model"
```

---

### Task 8: WindowsTerminalProfileService

The service is the workhorse. It probes the three known paths, parses each found `settings.json`, flattens defaults inheritance, resolves color schemes, and builds `WindowsTerminalProfile` instances.

For testability, the service exposes a static `ParseFile(path, source)` that does all the work for one settings.json. The discovery loop is a thin wrapper. Tests work directly against `ParseFile`.

**Files:**
- Create: `src/CodeShellManager/Services/WindowsTerminalProfileService.cs`
- Create: `tests/CodeShellManager.Tests/Fixtures/wt/happy.json`
- Create: `tests/CodeShellManager.Tests/Fixtures/wt/hidden.json`
- Create: `tests/CodeShellManager.Tests/Fixtures/wt/inheritance.json`
- Create: `tests/CodeShellManager.Tests/Fixtures/wt/malformed.json`
- Test: `tests/CodeShellManager.Tests/WindowsTerminalProfileServiceTests.cs`
- Modify: `tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj` (copy fixtures to output)

- [ ] **Step 1: Add fixtures**

Create `tests/CodeShellManager.Tests/Fixtures/wt/happy.json`:

```json
{
  "profiles": {
    "defaults": {},
    "list": [
      {
        "guid": "{aaa}",
        "name": "PowerShell",
        "commandline": "pwsh.exe -NoLogo",
        "startingDirectory": "%USERPROFILE%",
        "colorScheme": "Demo",
        "font": { "face": "Cascadia Code", "size": 12, "weight": "normal" },
        "cursorShape": "bar",
        "padding": "8, 8, 8, 8",
        "useAcrylic": false,
        "opacity": 1.0
      },
      {
        "guid": "{bbb}",
        "name": "Ubuntu",
        "commandline": "wsl.exe -d Ubuntu",
        "startingDirectory": "~",
        "colorScheme": "One Half Dark",
        "cursorShape": "underscore",
        "experimental.retroTerminalEffect": true
      }
    ]
  },
  "schemes": [
    { "name": "Demo", "background": "#0C0C0C", "foreground": "#CCCCCC",
      "cursorColor": "#FFFFFF", "selectionBackground": "#264F78",
      "black": "#000000", "red": "#C50F1F", "green": "#13A10E",
      "yellow": "#C19C00", "blue": "#0037DA", "purple": "#881798",
      "cyan": "#3A96DD", "white": "#CCCCCC",
      "brightBlack": "#767676", "brightRed": "#E74856",
      "brightGreen": "#16C60C", "brightYellow": "#F9F1A5",
      "brightBlue": "#3B78FF", "brightPurple": "#B4009E",
      "brightCyan": "#61D6D6", "brightWhite": "#F2F2F2" }
  ]
}
```

Create `tests/CodeShellManager.Tests/Fixtures/wt/hidden.json`:

```json
{
  "profiles": {
    "list": [
      { "guid": "{visible}", "name": "Visible", "commandline": "cmd.exe" },
      { "guid": "{hidden}", "name": "Hidden", "commandline": "cmd.exe", "hidden": true }
    ]
  }
}
```

Create `tests/CodeShellManager.Tests/Fixtures/wt/inheritance.json`:

```json
{
  "profiles": {
    "defaults": {
      "commandline": "pwsh.exe",
      "font": { "face": "Cascadia Mono", "size": 11 },
      "padding": "4"
    },
    "list": [
      { "guid": "{x}", "name": "Inherits" },
      { "guid": "{y}", "name": "Overrides", "commandline": "cmd.exe", "padding": "12" }
    ]
  }
}
```

Create `tests/CodeShellManager.Tests/Fixtures/wt/malformed.json`:

```
{ this is not json
```

- [ ] **Step 2: Wire fixtures into the test csproj**

In `tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj`, add inside the existing `<Project>` element (e.g. before the closing tag):

```xml
<ItemGroup>
  <None Update="Fixtures\wt\*.json">
    <CopyToOutputDirectory>PreserveNewest</CopyToOutputDirectory>
  </None>
</ItemGroup>
```

- [ ] **Step 3: Write failing tests**

Create `tests/CodeShellManager.Tests/WindowsTerminalProfileServiceTests.cs`:

```csharp
using System.IO;
using System.Linq;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class WindowsTerminalProfileServiceTests
{
    private static string Fixture(string name) =>
        Path.Combine(AppContext.BaseDirectory, "Fixtures", "wt", name);

    [Fact]
    public void Parse_HappyPath_ReturnsProfilesWithMappedFields()
    {
        var profiles = WindowsTerminalProfileService.ParseFile(Fixture("happy.json"), "Stable").ToList();

        Assert.Equal(2, profiles.Count);

        var ps = profiles.Single(p => p.Name == "PowerShell");
        Assert.Equal("pwsh.exe -NoLogo", ps.Commandline);
        Assert.Equal("Cascadia Code", ps.FontFamily);
        Assert.Equal(12, ps.FontSize);
        Assert.Equal("normal", ps.FontWeight);
        Assert.Equal("bar", ps.CursorShape);
        Assert.Equal("8px 8px 8px 8px", ps.Padding);
        Assert.Null(ps.RetroEffect); // not set in fixture
        Assert.NotNull(ps.ColorSchemeJson);
        Assert.Contains("\"background\":\"#0C0C0C\"", ps.ColorSchemeJson);

        var ubuntu = profiles.Single(p => p.Name == "Ubuntu");
        Assert.Equal("wsl.exe -d Ubuntu", ubuntu.Commandline);
        Assert.Equal("underline", ubuntu.CursorShape); // mapped from underscore
        Assert.True(ubuntu.RetroEffect);
        Assert.NotNull(ubuntu.ColorSchemeJson); // resolved from BuiltInTerminalSchemes
    }

    [Fact]
    public void Parse_HiddenProfile_IsExcluded()
    {
        var profiles = WindowsTerminalProfileService.ParseFile(Fixture("hidden.json"), "Stable").ToList();
        Assert.Single(profiles);
        Assert.Equal("Visible", profiles[0].Name);
    }

    [Fact]
    public void Parse_DefaultsInheritance_FlattensFields()
    {
        var profiles = WindowsTerminalProfileService.ParseFile(Fixture("inheritance.json"), "Stable").ToList();

        var inherits = profiles.Single(p => p.Name == "Inherits");
        Assert.Equal("pwsh.exe", inherits.Commandline);
        Assert.Equal("Cascadia Mono", inherits.FontFamily);
        Assert.Equal(11, inherits.FontSize);
        Assert.Equal("4px", inherits.Padding);

        var overrides = profiles.Single(p => p.Name == "Overrides");
        Assert.Equal("cmd.exe", overrides.Commandline);
        Assert.Equal("Cascadia Mono", overrides.FontFamily); // still inherited
        Assert.Equal("12px", overrides.Padding);              // overridden
    }

    [Fact]
    public void Parse_Malformed_ReturnsEmpty()
    {
        var profiles = WindowsTerminalProfileService.ParseFile(Fixture("malformed.json"), "Stable").ToList();
        Assert.Empty(profiles);
    }

    [Fact]
    public void Parse_MissingFile_ReturnsEmpty()
    {
        var profiles = WindowsTerminalProfileService.ParseFile(Fixture("does-not-exist.json"), "Stable").ToList();
        Assert.Empty(profiles);
    }
}
```

- [ ] **Step 4: Run tests and verify they fail**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~WindowsTerminalProfileServiceTests"`
Expected: FAIL.

- [ ] **Step 5: Implement `WindowsTerminalProfileService.cs`**

Create `src/CodeShellManager/Services/WindowsTerminalProfileService.cs`:

```csharp
using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// Reads Windows Terminal settings.json (Stable / Preview / Unpackaged) and
/// produces flattened, xterm-mapped <see cref="WindowsTerminalProfile"/>
/// instances. All errors swallowed and logged via App.LogPath; unreadable or
/// malformed files yield an empty enumeration.
/// </summary>
public static class WindowsTerminalProfileService
{
    private static readonly (string Source, string Path)[] KnownPaths =
    {
        ("Stable", Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Packages", "Microsoft.WindowsTerminal_8wekyb3d8bbwe", "LocalState", "settings.json")),
        ("Preview", Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Packages", "Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe", "LocalState", "settings.json")),
        ("Unpackaged", Path.Combine(
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
        try { doc = JsonDocument.Parse(File.ReadAllText(path), new JsonDocumentOptions { CommentHandling = JsonCommentHandling.Skip, AllowTrailingCommas = true }); }
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

            // Build a scheme lookup by name from the file's top-level "schemes" array
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

        string commandline = GetMerged(profile, defaults, "commandline") ?? "cmd.exe";
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

        // Font: profile.font.{face,size,weight,features.calt}, with merged defaults
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

        // ligatures: font.features.calt: 0 → false. Any other state → null (use default).
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
```

- [ ] **Step 6: Run tests and verify they pass**

Run: `dotnet test tests/CodeShellManager.Tests/ --filter "FullyQualifiedName~WindowsTerminalProfileServiceTests"`
Expected: 5 tests pass.

- [ ] **Step 7: Run the full test suite to make sure nothing else broke**

Run: `dotnet test tests/CodeShellManager.Tests/`
Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add src/CodeShellManager/Services/WindowsTerminalProfileService.cs tests/CodeShellManager.Tests/WindowsTerminalProfileServiceTests.cs tests/CodeShellManager.Tests/Fixtures/wt/ tests/CodeShellManager.Tests/CodeShellManager.Tests.csproj
git commit -m "feat: add WindowsTerminalProfileService with fixture-based tests"
```

---

### Task 9: NewSessionDialog — Profile combobox

**Files:**
- Modify: `src/CodeShellManager/Views/NewSessionDialog.xaml`
- Modify: `src/CodeShellManager/Views/NewSessionDialog.xaml.cs`

- [ ] **Step 1: Add the Profile row to the XAML**

In `src/CodeShellManager/Views/NewSessionDialog.xaml`:

In the `<Grid.RowDefinitions>` block (lines 147-156), add one more `<RowDefinition Height="Auto"/>` so there are 9 rows.

Bump every existing `Grid.Row="N"` (for `LocalPanel`, `SshPanel`, the Command StackPanel, the CustomArgsPanel, the Name StackPanel, and the buttons StackPanel) up by one.

Then insert a new row at `Grid.Row="1"` (between Session Type and Working Folder):

```xml
<StackPanel Grid.Row="1" x:Name="ProfilePanel" Visibility="Collapsed" Margin="0,0,0,12">
  <TextBlock Text="Windows Terminal Profile (optional)" Style="{StaticResource Label}"/>
  <ComboBox x:Name="ProfileCombo"
            AutomationProperties.AutomationId="NewSessionProfileCombo"
            SelectionChanged="ProfileCombo_SelectionChanged"/>
</StackPanel>
```

The final row indices should be: 0=SessionType, 1=ProfilePanel, 2=LocalPanel, 3=SshPanel, 4=Command, 5=CustomArgs, 6=Name, 7=spacer ("*"), 8=buttons.

- [ ] **Step 2: Update the dialog code-behind**

Replace the contents of `src/CodeShellManager/Views/NewSessionDialog.xaml.cs` with:

```csharp
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Windows;
using System.Windows.Controls;
using CodeShellManager.Models;
using CodeShellManager.Services;

namespace CodeShellManager.Views;

public partial class NewSessionDialog : Window
{
    private static readonly string[] DefaultCommands =
    [
        "claude", "claude --continue", "claude --model claude-opus-4-6",
        "claude --dangerously-skip-permissions", "codex", "gh copilot suggest", "pwsh", "cmd"
    ];

    // Local session output
    public string SelectedFolder { get; private set; } = "";
    public string SelectedCommand { get; private set; } = "claude";
    public string SelectedArgs { get; private set; } = "";
    public string SessionName { get; private set; } = "";
    public string SelectedGroupId { get; private set; } = "";

    // SSH session output
    public bool IsRemote { get; private set; } = false;
    public string SshHost { get; private set; } = "";
    public int SshPort { get; private set; } = 22;
    public string SshUser { get; private set; } = "";
    public string SshRemoteFolder { get; private set; } = "";

    // Profile-driven appearance overrides (null when no profile picked)
    public string? ProfileFontFamily { get; private set; }
    public int? ProfileFontSize { get; private set; }
    public string? ProfileFontWeight { get; private set; }
    public bool? ProfileFontLigatures { get; private set; }
    public string? ProfileCursorShape { get; private set; }
    public bool? ProfileCursorBlink { get; private set; }
    public string? ProfilePadding { get; private set; }
    public double? ProfileBackgroundOpacity { get; private set; }
    public bool? ProfileRetroEffect { get; private set; }
    public string? ProfileColorSchemeJson { get; private set; }

    private readonly IReadOnlyList<WindowsTerminalProfile> _profiles;

    public NewSessionDialog(
        string defaultFolder = "",
        IEnumerable<string>? launchCommands = null,
        IReadOnlyList<WindowsTerminalProfile>? profiles = null)
    {
        InitializeComponent();
        FolderBox.Text = defaultFolder;
        _profiles = profiles ?? Array.Empty<WindowsTerminalProfile>();

        var customItem = CommandCombo.Items[0];
        CommandCombo.Items.Clear();
        foreach (var cmd in launchCommands ?? DefaultCommands)
            CommandCombo.Items.Add(new ComboBoxItem { Content = cmd, Tag = cmd });
        CommandCombo.Items.Add(customItem);
        CommandCombo.SelectedIndex = 0;

        if (_profiles.Count > 0)
        {
            ProfilePanel.Visibility = Visibility.Visible;
            ProfileCombo.Items.Add(new ComboBoxItem { Content = "— none —", Tag = null });
            foreach (var p in _profiles)
                ProfileCombo.Items.Add(new ComboBoxItem { Content = p.Name, Tag = p });
            ProfileCombo.SelectedIndex = 0;
        }

        FolderBox.TextChanged += (_, _) => AutoFillName();
        SshHostBox.TextChanged += (_, _) => AutoFillName();
    }

    private bool IsRemoteMode => RemoteRadio?.IsChecked == true;

    private void AutoFillName()
    {
        if (!string.IsNullOrWhiteSpace(NameBox.Text)) return;

        if (IsRemoteMode)
        {
            var raw = SshHostBox.Text.Trim();
            if (!string.IsNullOrWhiteSpace(raw))
            {
                try { NameBox.Text = raw.Split(':')[0]; }
                catch { }
            }
        }
        else
        {
            if (!string.IsNullOrWhiteSpace(FolderBox.Text))
            {
                try { NameBox.Text = Path.GetFileName(FolderBox.Text.TrimEnd('/', '\\')); }
                catch { }
            }
        }
    }

    private void SessionType_Changed(object sender, RoutedEventArgs e)
    {
        if (LocalPanel == null) return;
        LocalPanel.Visibility = IsRemoteMode ? Visibility.Collapsed : Visibility.Visible;
        SshPanel.Visibility = IsRemoteMode ? Visibility.Visible : Visibility.Collapsed;
        // Profile combobox is local-only
        if (ProfilePanel != null && _profiles.Count > 0)
            ProfilePanel.Visibility = IsRemoteMode ? Visibility.Collapsed : Visibility.Visible;
        CommandLabel.Text = IsRemoteMode ? "Remote Shell" : "Command";
        NameBox.Text = "";
        AutoFillName();
    }

    private void BrowseFolder_Click(object sender, RoutedEventArgs e)
    {
        using var dialog = new System.Windows.Forms.FolderBrowserDialog
        {
            Description = "Select working folder",
            UseDescriptionForTitle = true,
            SelectedPath = FolderBox.Text
        };
        if (dialog.ShowDialog() == System.Windows.Forms.DialogResult.OK)
        {
            FolderBox.Text = dialog.SelectedPath;
            AutoFillName();
        }
    }

    private void CommandCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (CustomArgsPanel == null) return;
        var selected = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString();
        CustomArgsPanel.Visibility = selected == "custom" ? Visibility.Visible : Visibility.Collapsed;
    }

    private void ProfileCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var profile = (ProfileCombo.SelectedItem as ComboBoxItem)?.Tag as WindowsTerminalProfile;

        if (profile == null)
        {
            // — none — clears all overrides; folder/name/command stay as the user left them.
            ProfileFontFamily = null;
            ProfileFontSize = null;
            ProfileFontWeight = null;
            ProfileFontLigatures = null;
            ProfileCursorShape = null;
            ProfileCursorBlink = null;
            ProfilePadding = null;
            ProfileBackgroundOpacity = null;
            ProfileRetroEffect = null;
            ProfileColorSchemeJson = null;
            return;
        }

        // Pre-fill empty fields only — preserve any user edits.
        if (string.IsNullOrWhiteSpace(FolderBox.Text) && !string.IsNullOrEmpty(profile.StartingDirectory))
            FolderBox.Text = profile.StartingDirectory;

        if (string.IsNullOrWhiteSpace(NameBox.Text))
            NameBox.Text = profile.Name;

        // Add the profile's commandline as a transient entry in CommandCombo and select it
        var cmdString = profile.Commandline;
        var existing = CommandCombo.Items.OfType<ComboBoxItem>()
            .FirstOrDefault(it => it.Tag?.ToString() == cmdString);
        if (existing != null)
        {
            CommandCombo.SelectedItem = existing;
        }
        else
        {
            // Insert just before the [custom] item (which is always last)
            var item = new ComboBoxItem { Content = cmdString, Tag = cmdString };
            CommandCombo.Items.Insert(CommandCombo.Items.Count - 1, item);
            CommandCombo.SelectedItem = item;
        }

        // Stash overrides
        ProfileFontFamily = profile.FontFamily;
        ProfileFontSize = profile.FontSize;
        ProfileFontWeight = profile.FontWeight;
        ProfileFontLigatures = profile.FontLigatures;
        ProfileCursorShape = profile.CursorShape;
        ProfileCursorBlink = profile.CursorBlink;
        ProfilePadding = profile.Padding;
        ProfileBackgroundOpacity = profile.BackgroundOpacity;
        ProfileRetroEffect = profile.RetroEffect;
        ProfileColorSchemeJson = profile.ColorSchemeJson;
    }

    private void Start_Click(object sender, RoutedEventArgs e)
    {
        IsRemote = IsRemoteMode;
        SessionName = NameBox.Text.Trim();

        if (IsRemote)
        {
            if (string.IsNullOrWhiteSpace(SshHostBox.Text))
            {
                System.Windows.MessageBox.Show(
                    "Please enter a host (e.g. user@hostname).",
                    "Host required", MessageBoxButton.OK, MessageBoxImage.Warning);
                SshHostBox.Focus();
                return;
            }

            var hostRaw = SshHostBox.Text.Trim();
            var atIdx = hostRaw.IndexOf('@');
            if (atIdx > 0)
            {
                SshUser = hostRaw[..atIdx];
                SshHost = hostRaw[(atIdx + 1)..];
            }
            else
            {
                SshUser = "";
                SshHost = hostRaw;
            }

            SshPort = int.TryParse(SshPortBox.Text.Trim(), out int port) && port is > 0 and <= 65535
                ? port : 22;

            SshRemoteFolder = SshRemoteFolderBox.Text.Trim();

            var selectedTag = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "bash";
            if (selectedTag == "custom")
            {
                var (exe, args) = CommandLineSplitter.Split(CustomArgsBox.Text.Trim());
                SelectedCommand = string.IsNullOrEmpty(exe) ? "bash" : exe;
                SelectedArgs = args;
            }
            else
            {
                var (exe, args) = CommandLineSplitter.Split(selectedTag);
                SelectedCommand = string.IsNullOrEmpty(exe) ? "bash" : exe;
                SelectedArgs = args;
            }

            SelectedFolder = "";
        }
        else
        {
            SelectedFolder = FolderBox.Text.Trim();

            var selectedTag = (CommandCombo.SelectedItem as ComboBoxItem)?.Tag?.ToString() ?? "claude";
            if (selectedTag == "custom")
            {
                var (exe, args) = CommandLineSplitter.Split(CustomArgsBox.Text.Trim());
                SelectedCommand = string.IsNullOrEmpty(exe) ? "claude" : exe;
                SelectedArgs = args;
            }
            else
            {
                var (exe, args) = CommandLineSplitter.Split(selectedTag);
                SelectedCommand = string.IsNullOrEmpty(exe) ? "claude" : exe;
                SelectedArgs = args;
            }
        }

        DialogResult = true;
        Close();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }
}
```

(Note: this also replaces the old `string.Split(' ', 2)` parsing with `CommandLineSplitter.Split` so quoted profile commandlines round-trip correctly.)

- [ ] **Step 3: Build to verify**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/Views/NewSessionDialog.xaml src/CodeShellManager/Views/NewSessionDialog.xaml.cs
git commit -m "feat: add Profile combobox to NewSessionDialog with override stashing"
```

---

### Task 10: SettingsWindow checkbox

**Files:**
- Modify: `src/CodeShellManager/Views/SettingsWindow.xaml`
- Modify: `src/CodeShellManager/Views/SettingsWindow.xaml.cs`

- [ ] **Step 1: Add checkbox to XAML**

In `src/CodeShellManager/Views/SettingsWindow.xaml`, in the APPEARANCE StackPanel (lines 216-219), add:

```xml
<CheckBox x:Name="ImportWindowsTerminalProfilesCheck"
          Content="Show Windows Terminal profiles when creating sessions"
          ToolTip="Reads %LOCALAPPDATA%\…\settings.json. Read-only — profiles can't be edited from CodeShellManager."/>
```

- [ ] **Step 2: Wire up the checkbox in code-behind**

In `src/CodeShellManager/Views/SettingsWindow.xaml.cs`, in the constructor (after the other appearance checkboxes around line 53):

```csharp
ImportWindowsTerminalProfilesCheck.IsChecked = _edited.ImportWindowsTerminalProfiles;
```

In `Save_Click` (after the `ShowGitBranch` line around 114):

```csharp
_edited.ImportWindowsTerminalProfiles = ImportWindowsTerminalProfilesCheck.IsChecked == true;
```

- [ ] **Step 3: Build to verify**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add src/CodeShellManager/Views/SettingsWindow.xaml src/CodeShellManager/Views/SettingsWindow.xaml.cs
git commit -m "feat: add Show Windows Terminal profiles checkbox to settings"
```

---

### Task 11: MainWindow — pass profiles into dialog, copy overrides to session

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

- [ ] **Step 1: Pass profiles into dialog and copy override fields**

In `OpenNewSessionDialog` (line 238), replace the existing dialog construction and session-creation block with:

```csharp
private void OpenNewSessionDialog(string defaultFolder = "")
{
    var profiles = _vm.Settings.ImportWindowsTerminalProfiles
        ? Services.WindowsTerminalProfileService.GetProfiles()
        : null;

    var dialog = new NewSessionDialog(
        string.IsNullOrEmpty(defaultFolder) ? _vm.Settings.DefaultWorkingFolder : defaultFolder,
        _vm.Settings.LaunchCommands,
        profiles)
    {
        Owner = this
    };

    if (dialog.ShowDialog() != true) return;

    var session = _sessionManager.CreateSession(
        dialog.SessionName,
        dialog.SelectedFolder,
        dialog.SelectedCommand,
        dialog.SelectedArgs,
        dialog.SelectedGroupId);

    if (dialog.IsRemote)
    {
        session.IsRemote = true;
        session.SshUser = dialog.SshUser;
        session.SshHost = dialog.SshHost;
        session.SshPort = dialog.SshPort;
        session.SshRemoteFolder = dialog.SshRemoteFolder;
    }

    // Copy any profile-driven overrides onto the session so they persist + apply on launch
    session.ProfileFontFamily = dialog.ProfileFontFamily;
    session.ProfileFontSize = dialog.ProfileFontSize;
    session.ProfileFontWeight = dialog.ProfileFontWeight;
    session.ProfileFontLigatures = dialog.ProfileFontLigatures;
    session.ProfileCursorShape = dialog.ProfileCursorShape;
    session.ProfileCursorBlink = dialog.ProfileCursorBlink;
    session.ProfilePadding = dialog.ProfilePadding;
    session.ProfileBackgroundOpacity = dialog.ProfileBackgroundOpacity;
    session.ProfileRetroEffect = dialog.ProfileRetroEffect;
    session.ProfileColorSchemeJson = dialog.ProfileColorSchemeJson;

    _ = LaunchSessionAsync(session);
}
```

- [ ] **Step 2: Build to verify**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: build succeeds.

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat: pass WT profiles into NewSessionDialog and copy overrides to session"
```

---

### Task 12: Extract terminal-init.js + add transparent variant + extend setOptions handler

**Files:**
- Create: `src/CodeShellManager/Assets/terminal-init.js`
- Create: `src/CodeShellManager/Assets/terminal-transparent.html`
- Modify: `src/CodeShellManager/Assets/terminal.html`
- Modify: `src/CodeShellManager/CodeShellManager.csproj`

- [ ] **Step 1: Extract terminal-init.js**

Copy the entire body of the current `<script>` tag in `src/CodeShellManager/Assets/terminal.html` (lines 35-232 — everything from `const term = new Terminal({` to the final `term.focus();`) into a new file `src/CodeShellManager/Assets/terminal-init.js`.

In `terminal-init.js`, change the constructor's first line so it picks up window-level configuration:

```js
const term = new Terminal(Object.assign({
  cursorBlink: true,
  fontSize: 14,
  fontFamily: "'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace",
  fontLigatures: true,
  theme: { /* same default theme as before */ },
  scrollback: 5000,
  allowProposedApi: true,
  macOptionIsMeta: false,
  windowsMode: true,
}, window.__termOptions || {}));
```

(Keep the original default-theme block verbatim — this is a copy-paste, not a rewrite.)

In the `setOptions` message handler block (originally lines 87-96), extend it to:

```js
else if (msg.type === 'setOptions') {
  const opts = msg.options;
  if (opts.fontFamily    !== undefined) term.options.fontFamily    = opts.fontFamily;
  if (opts.fontSize      !== undefined) term.options.fontSize      = opts.fontSize;
  if (opts.fontLigatures !== undefined) term.options.fontLigatures = opts.fontLigatures;
  if (opts.fontWeight    !== undefined) term.options.fontWeight    = opts.fontWeight;
  if (opts.letterSpacing !== undefined) term.options.letterSpacing = opts.letterSpacing;
  if (opts.lineHeight    !== undefined) term.options.lineHeight    = opts.lineHeight;
  if (opts.theme         !== undefined) term.options.theme         = opts.theme;
  if (opts.cursorStyle   !== undefined) term.options.cursorStyle   = opts.cursorStyle;
  if (opts.cursorBlink   !== undefined) term.options.cursorBlink   = opts.cursorBlink;
  if (opts.padding       !== undefined) document.getElementById('terminal').style.padding = opts.padding;
  if (opts.retro         !== undefined) document.body.classList.toggle('retro', !!opts.retro);
  fitAddon.fit();
}
```

- [ ] **Step 2: Replace terminal.html script body with the include**

In `src/CodeShellManager/Assets/terminal.html`, replace the entire `<script>...</script>` block (lines 34-233) with:

```html
<script src="xterm.js"></script>
<script src="xterm-addon-fit.js"></script>
<script>
  // Default opaque body background. terminal-transparent.html overrides this.
  document.body.style.background = '#1e1e1e';
  // No allowTransparency in the constructor — terminal-init.js handles defaults.
</script>
<script src="terminal-init.js"></script>
```

Add the retro-overlay CSS to the `<style>` block in the `<head>`:

```css
body.retro::before {
  content: "";
  position: fixed; inset: 0;
  pointer-events: none;
  background:
    repeating-linear-gradient(
      to bottom,
      rgba(0,0,0,0) 0,
      rgba(0,0,0,0) 2px,
      rgba(0,0,0,0.18) 3px
    );
  mix-blend-mode: multiply;
  z-index: 50;
}
```

- [ ] **Step 3: Create terminal-transparent.html**

Create `src/CodeShellManager/Assets/terminal-transparent.html` with the same content as `terminal.html`, but:

1. Remove the `background: #1e1e1e` rule from `html, body` in the `<style>` block — set it to `transparent` instead.
2. In the inline `<script>` before `terminal-init.js`, set `window.__termOptions = { allowTransparency: true };` and remove the `document.body.style.background = '#1e1e1e';` line.

```html
<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8" />
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  html, body { width: 100%; height: 100%; background: transparent; overflow: hidden; }
  #terminal { width: 100%; height: 100%; }

  /* Drag-over overlay (same as terminal.html) */
  #dropOverlay { display: none; position: fixed; inset: 0;
    background: rgba(137, 180, 250, 0.15); border: 2px dashed #89b4fa;
    z-index: 100; pointer-events: none; align-items: center; justify-content: center;
    font-family: 'Segoe UI', sans-serif; font-size: 16px; color: #89b4fa; }
  #dropOverlay.active { display: flex; }

  body.retro::before {
    content: "";
    position: fixed; inset: 0;
    pointer-events: none;
    background:
      repeating-linear-gradient(
        to bottom,
        rgba(0,0,0,0) 0,
        rgba(0,0,0,0) 2px,
        rgba(0,0,0,0.18) 3px
      );
    mix-blend-mode: multiply;
    z-index: 50;
  }
</style>
<link rel="stylesheet" href="xterm.css" />
</head>
<body>
<div id="terminal"></div>
<div id="dropOverlay">Drop files to insert path(s)</div>

<script src="xterm.js"></script>
<script src="xterm-addon-fit.js"></script>
<script>
  window.__termOptions = { allowTransparency: true };
</script>
<script src="terminal-init.js"></script>
</body>
</html>
```

- [ ] **Step 4: Add new assets to csproj**

In `src/CodeShellManager/CodeShellManager.csproj`, in the `<ItemGroup>` containing `Content` items (lines 24-45), add:

```xml
<Content Include="Assets\terminal-init.js">
  <CopyToOutputDirectory>PreserveNewest</CopyToOutputDirectory>
  <ExcludeFromSingleFile>true</ExcludeFromSingleFile>
</Content>
<Content Include="Assets\terminal-transparent.html">
  <CopyToOutputDirectory>PreserveNewest</CopyToOutputDirectory>
  <ExcludeFromSingleFile>true</ExcludeFromSingleFile>
</Content>
```

- [ ] **Step 5: Build and run a smoke test**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: build succeeds.

Run: `dotnet run --project src/CodeShellManager/CodeShellManager.csproj`

Manually:
1. Open the app.
2. Click + New Session, choose any command (e.g. `pwsh`), Start.
3. Assert the terminal renders normally with the existing dark theme — no regression from refactoring the script.
4. Close the app.

- [ ] **Step 6: Commit**

```bash
git add src/CodeShellManager/Assets/terminal.html src/CodeShellManager/Assets/terminal-init.js src/CodeShellManager/Assets/terminal-transparent.html src/CodeShellManager/CodeShellManager.csproj
git commit -m "feat: extract terminal-init.js, add transparent variant, extend setOptions"
```

---

### Task 13: TerminalBridge — ApplyProfileOverrides

**Files:**
- Modify: `src/CodeShellManager/Terminal/TerminalBridge.cs`

- [ ] **Step 1: Add `ApplyProfileOverrides` method**

In `src/CodeShellManager/Terminal/TerminalBridge.cs`, after the existing `ApplyFontSettings` method (line 272), add:

```csharp
public void ApplyProfileOverrides(ShellSession session)
{
    if (!_ready) return;
    if (!HasAnyOverride(session)) return;

    var opts = new System.Collections.Generic.Dictionary<string, object?>();
    if (session.ProfileFontFamily    != null) opts["fontFamily"]    = session.ProfileFontFamily;
    if (session.ProfileFontSize      != null) opts["fontSize"]      = session.ProfileFontSize;
    if (session.ProfileFontWeight    != null) opts["fontWeight"]    = session.ProfileFontWeight;
    if (session.ProfileFontLigatures != null) opts["fontLigatures"] = session.ProfileFontLigatures;
    if (session.ProfileCursorShape   != null) opts["cursorStyle"]   = session.ProfileCursorShape;
    if (session.ProfileCursorBlink   != null) opts["cursorBlink"]   = session.ProfileCursorBlink;
    if (session.ProfilePadding       != null) opts["padding"]       = session.ProfilePadding;
    if (session.ProfileRetroEffect   != null) opts["retro"]         = session.ProfileRetroEffect;
    if (!string.IsNullOrEmpty(session.ProfileColorSchemeJson))
        opts["theme"] = JsonSerializer.Deserialize<JsonElement>(session.ProfileColorSchemeJson);

    string json = JsonSerializer.Serialize(new { type = "setOptions", options = opts });
    WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
    {
        try { _webView.CoreWebView2?.PostWebMessageAsString(json); }
        catch { }
    });
}

private static bool HasAnyOverride(ShellSession s) =>
    s.ProfileFontFamily != null || s.ProfileFontSize != null
    || s.ProfileFontWeight != null || s.ProfileFontLigatures != null
    || s.ProfileCursorShape != null || s.ProfileCursorBlink != null
    || s.ProfilePadding != null || s.ProfileRetroEffect != null
    || !string.IsNullOrEmpty(s.ProfileColorSchemeJson);
```

Add `using CodeShellManager.Models;` at the top of the file if not already present. (`JsonElement` and `JsonSerializer` come from `System.Text.Json` which is already imported.)

- [ ] **Step 2: Build to verify**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: build succeeds.

- [ ] **Step 3: Commit**

```bash
git add src/CodeShellManager/Terminal/TerminalBridge.cs
git commit -m "feat: add TerminalBridge.ApplyProfileOverrides"
```

---

### Task 14: MainWindow — pick HTML entry point and apply overrides on launch

**Files:**
- Modify: `src/CodeShellManager/MainWindow.xaml.cs`

- [ ] **Step 1: Use the right HTML for transparent sessions and apply overrides**

In `LaunchSessionAsync`, replace the block around lines 329-333:

```csharp
string assetsDir = Path.Combine(AppContext.BaseDirectory, "Assets");
string htmlPath = new Uri(Path.Combine(assetsDir, "terminal.html")).AbsoluteUri;

await bridge.InitializeAsync(htmlPath);
bridge.ApplyFontSettings(_vm.Settings);
```

with:

```csharp
string assetsDir = Path.Combine(AppContext.BaseDirectory, "Assets");
bool wantTransparent = session.ProfileBackgroundOpacity is < 1.0;
string htmlFile = wantTransparent ? "terminal-transparent.html" : "terminal.html";
string htmlPath = new Uri(Path.Combine(assetsDir, htmlFile)).AbsoluteUri;

await bridge.InitializeAsync(htmlPath);
bridge.ApplyFontSettings(_vm.Settings);
bridge.ApplyProfileOverrides(session);
```

- [ ] **Step 2: Build to verify**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj`
Expected: build succeeds.

- [ ] **Step 3: Manual smoke test — sessions without profile**

Run: `dotnet run --project src/CodeShellManager/CodeShellManager.csproj`

1. Open the app.
2. Open Settings — confirm the new "Show Windows Terminal profiles when creating sessions" checkbox is present and unchecked.
3. Click + New Session — confirm there is **no** Profile combobox (setting is off).
4. Start a normal `pwsh` session — confirm it works exactly as before.
5. Close the app.

- [ ] **Step 4: Manual smoke test — sessions with profile**

(Skip this step if no Windows Terminal install is present on the dev machine.)

1. Open the app.
2. Open Settings, tick the new checkbox, Save.
3. Click + New Session — confirm the Profile combobox is present, with `— none —` plus your installed WT profiles.
4. Pick a profile that has a custom color scheme. Confirm:
   - Folder is auto-filled (if profile has `startingDirectory`).
   - Command field shows the profile's `commandline`.
   - Name is auto-filled.
5. Start the session. Confirm the terminal renders with the profile's color scheme and font.
6. Close the app, relaunch. Confirm the session restores with the same look (overrides persisted).
7. Close the app.

- [ ] **Step 5: Commit**

```bash
git add src/CodeShellManager/MainWindow.xaml.cs
git commit -m "feat: route transparent sessions to terminal-transparent.html and apply profile overrides"
```

---

### Task 15: UI test for the Profile combobox visibility

**Files:**
- Create: `tests/CodeShellManager.UITests/ProfilesTests.cs`

- [ ] **Step 1: Look at existing UI test pattern**

Read `tests/CodeShellManager.UITests/AppFixture.cs` and one existing test class (e.g. `SettingsTests.cs`) to confirm the helper API for opening Settings and the New Session dialog. Mirror that pattern.

- [ ] **Step 2: Write the test**

Create `tests/CodeShellManager.UITests/ProfilesTests.cs`:

```csharp
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Definitions;
using Xunit;

namespace CodeShellManager.UITests;

public class ProfilesTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _app;

    public ProfilesTests(AppFixture app) => _app = app;

    [Fact]
    public void ProfileCombo_HiddenWhenSettingOff_VisibleWhenOn()
    {
        // Default: setting is off
        var dialog = _app.OpenNewSessionDialog();
        var combo = dialog.FindFirstDescendant(c => c.ByAutomationId("NewSessionProfileCombo"));
        // When the StackPanel is collapsed, the combo is either not in the tree
        // or its parent is not visible. FlaUI returns null in either case.
        Assert.True(combo == null || !combo.IsAvailable || combo.Properties.IsOffscreen.Value);
        dialog.Cancel();

        // Toggle the setting on
        var settings = _app.OpenSettings();
        settings.FindFirstDescendant(c => c.ByAutomationId("ImportWindowsTerminalProfilesCheck"))
            !.AsCheckBox().IsChecked = true;
        settings.Save();

        // Re-open the New Session dialog
        dialog = _app.OpenNewSessionDialog();
        combo = dialog.FindFirstDescendant(c => c.ByAutomationId("NewSessionProfileCombo"));
        Assert.NotNull(combo);
        Assert.True(combo!.IsAvailable);
        dialog.Cancel();

        // Restore default
        settings = _app.OpenSettings();
        settings.FindFirstDescendant(c => c.ByAutomationId("ImportWindowsTerminalProfilesCheck"))
            !.AsCheckBox().IsChecked = false;
        settings.Save();
    }
}
```

(If `AppFixture` does not yet expose `OpenSettings()` / `OpenNewSessionDialog()` / `Save()` / `Cancel()` helpers in exactly this shape, adapt the test to use whatever helpers exist. The two assertions to keep are: combo absent/offscreen when setting is off, present and available when setting is on.)

You will need to add an `AutomationProperties.AutomationId="ImportWindowsTerminalProfilesCheck"` to the checkbox in `SettingsWindow.xaml` if not already present from Task 10. Verify that.

- [ ] **Step 3: Run the test**

Run: `dotnet test tests/CodeShellManager.UITests/`
Expected: the new test passes; no other UI tests regress.

(UI tests need a live desktop session. If running in a headless environment, skip this step and verify locally.)

- [ ] **Step 4: Commit**

```bash
git add tests/CodeShellManager.UITests/ProfilesTests.cs src/CodeShellManager/Views/SettingsWindow.xaml
git commit -m "test: verify Profile combobox visibility tracks setting"
```

---

### Task 16: Update CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add a new "Windows Terminal Profile Import" section**

Insert after the "SSH Remote Sessions" section in `CLAUDE.md`:

```markdown
## Windows Terminal Profile Import (opt-in)

When `AppSettings.ImportWindowsTerminalProfiles` is on, the New Session dialog reads the user's Windows Terminal `settings.json` and offers each profile in a "Profile (optional)" combobox.

**Service:** `WindowsTerminalProfileService.GetProfiles()` probes Stable / Preview / Unpackaged install paths, parses each `settings.json`, flattens `profiles.defaults`, filters hidden profiles, and emits `WindowsTerminalProfile` POCOs with appearance fields already mapped to xterm equivalents.

**Per-session overrides** (all on `ShellSession`, all nullable, all persisted to `state.json`):

- `ProfileFontFamily`, `ProfileFontSize`, `ProfileFontWeight`, `ProfileFontLigatures`
- `ProfileCursorShape` (`"block" | "underline" | "bar"`), `ProfileCursorBlink`
- `ProfilePadding` (CSS shorthand)
- `ProfileBackgroundOpacity` (0.0–1.0; 1.0 = opaque)
- `ProfileRetroEffect` (CSS scanlines overlay only — not a real CRT shader)
- `ProfileColorSchemeJson` (pre-baked xterm theme)

When any override is set, `LaunchSessionAsync` calls `bridge.ApplyProfileOverrides(session)` after `ApplyFontSettings`, posting a `setOptions` message that wins over the global font.

**Transparency:** xterm.js requires `allowTransparency` in the constructor, so transparent sessions navigate to `Assets/terminal-transparent.html` instead of `terminal.html`. Both files share `Assets/terminal-init.js`. (Acrylic blur is not reachable from WebView2 — we get flat alpha over the WPF chrome instead.)

**Once stamped, profile overrides are independent.** A session keeps its appearance even if the user later edits or deletes the source profile in Windows Terminal.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document Windows Terminal profile import in CLAUDE.md"
```

---

## Self-review

**Spec coverage check:**

| Spec section | Covered by |
|---|---|
| `ImportWindowsTerminalProfiles` setting | Task 1, Task 10 |
| `WindowsTerminalProfileService` discovery + parsing | Task 8 |
| `WindowsTerminalProfile` model | Task 7 |
| `BuiltInTerminalSchemes` lookup | Task 6 |
| `SchemeMapper` purple→magenta + opacity rewrite | Task 5 |
| `CommandLineSplitter` quote-aware split | Task 2 |
| Padding 1/2/4-value parsing | Task 3 |
| Cursor shape mapping (six WT shapes) | Task 4 |
| `ShellSession` per-session override fields | Task 1 |
| New Session dialog Profile row + override stashing | Task 9 |
| Settings checkbox | Task 10 |
| Pass profiles into dialog from MainWindow | Task 11 |
| Extract terminal-init.js + extend setOptions handler + retro CSS | Task 12 |
| Transparent HTML variant | Task 12 |
| `ApplyProfileOverrides` on bridge | Task 13 |
| Pick correct HTML entry point + call ApplyProfileOverrides | Task 14 |
| UI test for combobox visibility | Task 15 |
| Doc update | Task 16 |

**Placeholder scan:** No "TBD", "TODO", or vague-instruction steps. All code blocks are complete. Every method, property, and AutomationId referenced in later tasks is defined in earlier tasks.

**Type consistency:** Profile override property names match across `WindowsTerminalProfile` (init-only on the model), `ShellSession` (mutable for assignment), `NewSessionDialog` outputs, and `ApplyProfileOverrides` payload keys. Names verified: `ProfileFontFamily / ProfileFontSize / ProfileFontWeight / ProfileFontLigatures / ProfileCursorShape / ProfileCursorBlink / ProfilePadding / ProfileBackgroundOpacity / ProfileRetroEffect / ProfileColorSchemeJson`. xterm-side keys verified: `fontFamily / fontSize / fontWeight / fontLigatures / cursorStyle / cursorBlink / padding / retro / theme`.

**Scope:** One implementation plan; ~16 small tasks, each 2–10 minutes. No subsystem decomposition needed.

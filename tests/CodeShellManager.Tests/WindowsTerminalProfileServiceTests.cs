using System;
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
        Assert.Null(ps.RetroEffect);
        Assert.NotNull(ps.ColorSchemeJson);
        Assert.Contains("\"background\":\"#0C0C0C\"", ps.ColorSchemeJson);

        var ubuntu = profiles.Single(p => p.Name == "Ubuntu");
        Assert.Equal("wsl.exe -d Ubuntu", ubuntu.Commandline);
        Assert.Equal("underline", ubuntu.CursorShape);
        Assert.True(ubuntu.RetroEffect);
        Assert.NotNull(ubuntu.ColorSchemeJson);
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
        Assert.Equal("Cascadia Mono", overrides.FontFamily);
        Assert.Equal("12px", overrides.Padding);
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

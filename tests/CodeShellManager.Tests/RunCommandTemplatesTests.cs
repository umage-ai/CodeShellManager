using System.IO;
using System.Linq;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class RunCommandTemplatesTests : System.IDisposable
{
    private readonly string _tmp;

    public RunCommandTemplatesTests()
    {
        _tmp = Path.Combine(Path.GetTempPath(), "csm-tpl-" + System.Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_tmp);
    }

    public void Dispose() { try { Directory.Delete(_tmp, recursive: true); } catch { } }

    [Fact]
    public void Detect_DotnetProject_ReturnsDotnetTemplate()
    {
        File.WriteAllText(Path.Combine(_tmp, "MyApp.csproj"), "<Project/>");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.NotNull(seed);
        Assert.Equal("dotnet", seed!.ProjectType);
        Assert.Contains(seed.Items, i => i.CommandLine == "dotnet run" && i.IsDefault);
        Assert.Contains(seed.Items, i => i.CommandLine == "dotnet build");
        Assert.Contains(seed.Items, i => i.CommandLine == "dotnet test");
    }

    [Fact]
    public void Detect_CargoProject_ReturnsCargoTemplate()
    {
        File.WriteAllText(Path.Combine(_tmp, "Cargo.toml"), "[package]");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("cargo", seed!.ProjectType);
        Assert.Contains(seed.Items, i => i.CommandLine == "cargo run" && i.IsDefault);
    }

    [Fact]
    public void Detect_NodeProject_DefaultsToNpm()
    {
        File.WriteAllText(Path.Combine(_tmp, "package.json"), "{}");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("node", seed!.ProjectType);
        Assert.Contains(seed.Items, i => i.CommandLine == "npm start" && i.IsDefault);
    }

    [Fact]
    public void Detect_NodeProject_WithPnpmLock_UsesPnpm()
    {
        File.WriteAllText(Path.Combine(_tmp, "package.json"), "{}");
        File.WriteAllText(Path.Combine(_tmp, "pnpm-lock.yaml"), "");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Contains(seed!.Items, i => i.CommandLine.StartsWith("pnpm "));
    }

    [Fact]
    public void Detect_NodeProject_WithYarnLock_UsesYarn()
    {
        File.WriteAllText(Path.Combine(_tmp, "package.json"), "{}");
        File.WriteAllText(Path.Combine(_tmp, "yarn.lock"), "");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Contains(seed!.Items, i => i.CommandLine.StartsWith("yarn"));
    }

    [Fact]
    public void Detect_NodeProject_WithBunLockb_UsesBun()
    {
        File.WriteAllText(Path.Combine(_tmp, "package.json"), "{}");
        File.WriteAllText(Path.Combine(_tmp, "bun.lockb"), "");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Contains(seed!.Items, i => i.CommandLine.StartsWith("bun "));
    }

    [Fact]
    public void Detect_PythonProject_ReturnsPythonTemplate()
    {
        File.WriteAllText(Path.Combine(_tmp, "pyproject.toml"), "[project]");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("python", seed!.ProjectType);
    }

    [Fact]
    public void Detect_Makefile_ReturnsMakeTemplate()
    {
        File.WriteAllText(Path.Combine(_tmp, "Makefile"), "run:");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("make", seed!.ProjectType);
    }

    [Fact]
    public void Detect_DotnetBeatsCargo()
    {
        // Both markers present — dotnet template wins because it's first in the priority list.
        File.WriteAllText(Path.Combine(_tmp, "MyApp.csproj"), "<Project/>");
        File.WriteAllText(Path.Combine(_tmp, "Cargo.toml"), "[package]");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal("dotnet", seed!.ProjectType);
    }

    [Fact]
    public void Detect_EmptyFolder_ReturnsNull()
    {
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Null(seed);
    }

    [Fact]
    public void Detect_NonexistentFolder_ReturnsNull()
    {
        var seed = RunCommandTemplatesService.SeedFor(Path.Combine(_tmp, "does-not-exist"));
        Assert.Null(seed);
    }

    [Fact]
    public void Detect_NoRecursiveScan_IgnoresMatchInSubfolder()
    {
        var sub = Path.Combine(_tmp, "subdir");
        Directory.CreateDirectory(sub);
        File.WriteAllText(Path.Combine(sub, "MyApp.csproj"), "<Project/>");
        // Top-level is empty → must NOT detect.
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Null(seed);
    }

    [Fact]
    public void SeedItems_HaveExactlyOneDefault()
    {
        File.WriteAllText(Path.Combine(_tmp, "MyApp.csproj"), "<Project/>");
        var seed = RunCommandTemplatesService.SeedFor(_tmp);
        Assert.Equal(1, seed!.Items.Count(i => i.IsDefault));
    }

    [Fact]
    public void SeedItems_HaveFreshIds()
    {
        File.WriteAllText(Path.Combine(_tmp, "MyApp.csproj"), "<Project/>");
        var a = RunCommandTemplatesService.SeedFor(_tmp)!.Items;
        var b = RunCommandTemplatesService.SeedFor(_tmp)!.Items;
        // Two seedings of the same folder must produce DIFFERENT ids — otherwise
        // multiple sessions on the same folder would share RunCommandItem.Id, which
        // collides in the per-session RunInstance dictionary.
        Assert.NotEqual(a[0].Id, b[0].Id);
    }
}

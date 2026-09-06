using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// One project-type template — a label plus a seed list of run commands that will
/// be COPIED into a new session's <see cref="ShellSession.RunCommands"/> on creation.
/// </summary>
public record RunCommandTemplate(string ProjectType, IReadOnlyList<RunCommandItem> Items);

/// <summary>
/// Resolves a working folder to the matching project-type template (first match wins).
/// Detection is non-recursive (top-level files only) and runs once at session creation.
/// </summary>
public static class RunCommandTemplatesService
{
    /// <summary>
    /// Returns the matching template with fresh (new-Guid) item Ids, or null if no
    /// detector matched (empty folder, unknown project type, or non-existent path).
    /// </summary>
    public static RunCommandTemplate? SeedFor(string workingFolder)
    {
        if (string.IsNullOrWhiteSpace(workingFolder) || !Directory.Exists(workingFolder))
            return null;

        // Enumerate ONCE — repeated File.Exists is slow on network shares.
        // EnumerateFiles is non-recursive by default.
        HashSet<string> files;
        try
        {
            files = new HashSet<string>(
                Directory.EnumerateFiles(workingFolder).Select(p => Path.GetFileName(p) ?? ""),
                StringComparer.OrdinalIgnoreCase);
        }
        catch { return null; }

        bool Has(string name) => files.Contains(name);
        bool HasExt(string ext) => files.Any(f => f.EndsWith(ext, StringComparison.OrdinalIgnoreCase));

        // Priority order: dotnet → cargo → node → python → make. First match wins.
        if (HasExt(".sln") || HasExt(".csproj"))
            return Build("dotnet",
                ("Run",   "dotnet run",   IsDefault: true),
                ("Build", "dotnet build", IsDefault: false),
                ("Test",  "dotnet test",  IsDefault: false));

        if (Has("Cargo.toml"))
            return Build("cargo",
                ("Run",    "cargo run",    IsDefault: true),
                ("Build",  "cargo build",  IsDefault: false),
                ("Test",   "cargo test",   IsDefault: false),
                ("Clippy", "cargo clippy", IsDefault: false));

        if (Has("package.json"))
        {
            string pm =
                Has("pnpm-lock.yaml") ? "pnpm"
              : Has("yarn.lock")      ? "yarn"
              : Has("bun.lockb")      ? "bun"
              : "npm";

            // yarn's invocation differs slightly: `yarn start` (no `run`) is conventional.
            string runPrefix = pm == "yarn" ? "yarn" : $"{pm} run";
            return Build("node",
                ("Start", $"{pm} start",         IsDefault: true),
                ("Test",  $"{pm} test",          IsDefault: false),
                ("Build", $"{runPrefix} build",  IsDefault: false));
        }

        if (Has("pyproject.toml") || Has("requirements.txt"))
            return Build("python",
                ("Run",  "python main.py",     IsDefault: true),
                ("Test", "python -m pytest",   IsDefault: false));

        if (Has("Makefile") || Has("makefile"))
            return Build("make",
                ("Run",   "make",       IsDefault: true),
                ("Test",  "make test",  IsDefault: false),
                ("Clean", "make clean", IsDefault: false));

        return null;
    }

    private static RunCommandTemplate Build(string projectType, params (string Label, string Cmd, bool IsDefault)[] items)
    {
        var list = items.Select(t => new RunCommandItem
        {
            Id = Guid.NewGuid().ToString(),
            Label = t.Label,
            CommandLine = t.Cmd,
            IsDefault = t.IsDefault,
        }).ToList();
        return new RunCommandTemplate(projectType, list);
    }
}

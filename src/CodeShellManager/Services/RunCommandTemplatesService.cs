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
                ("Run",   "dotnet run",   isDefault: true),
                ("Build", "dotnet build", isDefault: false),
                ("Test",  "dotnet test",  isDefault: false));

        if (Has("Cargo.toml"))
            return Build("cargo",
                ("Run",    "cargo run",    isDefault: true),
                ("Build",  "cargo build",  isDefault: false),
                ("Test",   "cargo test",   isDefault: false),
                ("Clippy", "cargo clippy", isDefault: false));

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
                ("Start", $"{pm} start",         isDefault: true),
                ("Test",  $"{pm} test",          isDefault: false),
                ("Build", $"{runPrefix} build",  isDefault: false));
        }

        if (Has("pyproject.toml") || Has("requirements.txt"))
            return Build("python",
                ("Run",  "python main.py",     isDefault: true),
                ("Test", "python -m pytest",   isDefault: false));

        if (Has("Makefile") || Has("makefile"))
            return Build("make",
                ("Run",   "make",       isDefault: true),
                ("Test",  "make test",  isDefault: false),
                ("Clean", "make clean", isDefault: false));

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

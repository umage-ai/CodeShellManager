using System;
using System.IO;
using System.Linq;

namespace CodeShellManager.Services;

/// <summary>
/// Finds the most recent Claude Code session ID for a given working folder,
/// enabling precise --resume on session restore.
/// Sessions are stored at: ~/.claude/projects/<path-as-dashes>/<uuid>.jsonl
/// </summary>
public static class ClaudeSessionService
{
    /// <summary>
    /// Returns true if the command is a Claude Code invocation.
    /// </summary>
    public static bool IsClaudeCommand(string command) =>
        command.Equals("claude", StringComparison.OrdinalIgnoreCase) ||
        command.StartsWith("claude ", StringComparison.OrdinalIgnoreCase);

    /// <summary>
    /// Resolves the directory Claude Code keeps its data in. CLAUDE_CONFIG_DIR wins when
    /// set; otherwise the default ~/.claude.
    ///
    /// This matters because a machine that has ever run without the env var keeps a stale
    /// ~/.claude/projects/ tree. Reading that one yields a session id from the wrong store
    /// and `claude --resume &lt;id&gt;` fails with "No conversation found with session ID".
    /// </summary>
    internal static string ResolveClaudeHome(string? configDir, string userProfile) =>
        string.IsNullOrWhiteSpace(configDir)
            ? Path.Combine(userProfile, ".claude")
            : configDir;

    /// <summary>
    /// Finds the most recently modified session ID for the given working folder.
    /// Returns null if no session exists (new project or claude not yet run there).
    /// </summary>
    public static string? GetLastSessionId(string workingFolder) =>
        GetLastSessionId(workingFolder, ResolveClaudeHome(
            Environment.GetEnvironmentVariable("CLAUDE_CONFIG_DIR"),
            Environment.GetFolderPath(Environment.SpecialFolder.UserProfile)));

    /// <param name="claudeHome">Claude's data directory — see <see cref="ResolveClaudeHome"/>.</param>
    internal static string? GetLastSessionId(string workingFolder, string claudeHome)
    {
        if (string.IsNullOrWhiteSpace(workingFolder)) return null;

        try
        {
            string projectDir = ToProjectDirName(workingFolder);
            string claudeProjectsPath = Path.Combine(claudeHome, "projects", projectDir);

            if (!Directory.Exists(claudeProjectsPath)) return null;

            var latest = Directory.GetFiles(claudeProjectsPath, "*.jsonl")
                .Select(f => new FileInfo(f))
                .OrderByDescending(fi => fi.LastWriteTimeUtc)
                .FirstOrDefault();

            if (latest == null) return null;
            return Path.GetFileNameWithoutExtension(latest.Name);
        }
        catch
        {
            return null;
        }
    }

    /// <summary>
    /// Converts a Windows absolute path to the ~/.claude/projects/ directory name.
    /// e.g.  C:\Github\foo  →  C--Github-foo
    /// Rule:  replace ':' with '-'  and  '\' or '/' with '-'
    /// </summary>
    private static string ToProjectDirName(string path) =>
        path.Replace(":", "-").Replace("\\", "-").Replace("/", "-");
}

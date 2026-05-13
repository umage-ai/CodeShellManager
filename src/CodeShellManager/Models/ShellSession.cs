using System;
using System.Collections.Generic;
using System.Text;

namespace CodeShellManager.Models;

public enum SessionStatus { Idle, Running, NeedsAttention, Exited }

public class ShellSession
{
    public string Id { get; set; } = Guid.NewGuid().ToString();
    public string Name { get; set; } = "";
    public string WorkingFolder { get; set; } = "";
    public string Command { get; set; } = "claude";
    public string Args { get; set; } = "";
    public string GroupId { get; set; } = "";
    public string? ColorOverride { get; set; }
    public SessionStatus Status { get; set; } = SessionStatus.Idle;
    public DateTime CreatedAt { get; set; } = DateTime.UtcNow;

    /// <summary>
    /// Last time the user gave this session focus (clicked it in the sidebar, woke it,
    /// or it was selected via Ctrl+Tab). Persisted so "Sort by last active" survives
    /// restarts. Defaults to <see cref="CreatedAt"/> for sessions that have never been
    /// activated since the field was introduced.
    /// </summary>
    public DateTime LastActivityAt { get; set; } = DateTime.UtcNow;

    /// <summary>
    /// When true, the session has no live PTY/terminal — it is a placeholder
    /// in the sidebar that can be "woken" later. Persisted to state.json.
    /// </summary>
    public bool IsDormant { get; set; }

    // SSH / remote session fields
    public bool IsRemote { get; set; }
    public string SshUser { get; set; } = "";
    public string SshHost { get; set; } = "";
    public int SshPort { get; set; } = 22;
    public string SshRemoteFolder { get; set; } = "";

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

    /// <summary>
    /// Configured run commands for this session — the source for the toolbar ▶ button
    /// and the chips strip. Seeded at session creation from <see cref="Services.RunCommandTemplatesService"/>.
    /// Exactly one item has IsDefault=true (when the list is non-empty);
    /// see <see cref="RunCommandItem.EnsureSingleDefault"/>.
    /// </summary>
    public List<RunCommandItem> RunCommands { get; set; } = new();

    // Full command line for display and passthrough.
    // For remote sessions: "ssh <BuildSshArgs()>"
    // For local sessions: "Command [Args]"
    public string FullCommandLine => IsRemote
        ? $"ssh {BuildSshArgs()}"
        : (string.IsNullOrWhiteSpace(Args) ? Command : $"{Command} {Args}");

    /// <summary>
    /// Builds the argument string passed to the ssh executable.
    /// Example: "-t alice@dev.example.com \"cd '/proj' && bash\""
    /// </summary>
    internal string BuildSshArgs()
    {
        if (string.IsNullOrWhiteSpace(SshHost))
            throw new InvalidOperationException("SshHost must be set for remote sessions.");
        var sb = new StringBuilder();
        if (SshPort != 22)
            sb.Append($"-p {SshPort} ");
        sb.Append("-t ");
        var userAtHost = string.IsNullOrWhiteSpace(SshUser) ? SshHost : $"{SshUser}@{SshHost}";
        sb.Append(userAtHost);
        sb.Append(" \"");
        if (!string.IsNullOrWhiteSpace(SshRemoteFolder))
            sb.Append($"cd '{SshRemoteFolder}' && ");
        var shell = string.IsNullOrWhiteSpace(Command) ? "bash" : Command;
        sb.Append(shell);
        if (!string.IsNullOrWhiteSpace(Args))
            sb.Append($" {Args}");
        sb.Append("\"");
        return sb.ToString();
    }
}

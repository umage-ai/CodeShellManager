using System;
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

    // SSH / remote session fields
    public bool IsRemote { get; set; } = false;
    public string SshUser { get; set; } = "";
    public string SshHost { get; set; } = "";
    public int SshPort { get; set; } = 22;
    public string SshRemoteFolder { get; set; } = "";

    // Full command line for display and passthrough.
    // For remote sessions: "ssh <BuildSshArgs()>"
    // For local sessions: "Command [Args]"
    public string FullCommandLine => IsRemote
        ? $"ssh {BuildSshArgs()}"
        : (string.IsNullOrWhiteSpace(Args) ? Command : $"{Command} {Args}");

    /// <summary>
    /// Builds the argument string passed to the ssh executable.
    /// Example: "-t alice@dev.example.com \"cd \"/proj\" && bash\""
    /// </summary>
    public string BuildSshArgs()
    {
        var sb = new StringBuilder();
        if (SshPort != 22)
            sb.Append($"-p {SshPort} ");
        sb.Append("-t ");
        var userAtHost = string.IsNullOrWhiteSpace(SshUser) ? SshHost : $"{SshUser}@{SshHost}";
        sb.Append(userAtHost);
        sb.Append(" \"");
        if (!string.IsNullOrWhiteSpace(SshRemoteFolder))
            sb.Append($"cd \\\"{SshRemoteFolder}\\\" && ");
        var shell = string.IsNullOrWhiteSpace(Command) ? "bash" : Command;
        sb.Append(shell);
        if (!string.IsNullOrWhiteSpace(Args))
            sb.Append($" {Args}");
        sb.Append("\"");
        return sb.ToString();
    }
}

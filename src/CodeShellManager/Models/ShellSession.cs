using System;

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

    // Full command line: Command + space + Args
    public string FullCommandLine => string.IsNullOrWhiteSpace(Args)
        ? Command
        : $"{Command} {Args}";
}

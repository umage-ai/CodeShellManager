using System;

namespace CodeShellManager.Models;

public enum AlertType { InputRequired, ToolApproval, Question, Idle }

public class AlertEvent
{
    public string SessionId { get; set; } = "";
    public string SessionName { get; set; } = "";
    public string Message { get; set; } = "";
    public AlertType Type { get; set; }
    public DateTime Timestamp { get; set; } = DateTime.UtcNow;
}

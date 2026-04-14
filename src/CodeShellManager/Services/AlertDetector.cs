using System;
using System.Text.RegularExpressions;
using System.Threading;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// Monitors raw terminal output for patterns indicating Claude Code (or any AI CLI)
/// needs user interaction — tool approvals, yes/no questions, waiting for input.
/// </summary>
public partial class AlertDetector : IDisposable
{
    private readonly string _sessionId;
    private readonly string _sessionName;
    private string _lastLine = "";
    private System.Threading.Timer? _idleTimer;
    private bool _alertFired;

    public event Action<AlertEvent>? AlertRaised;
    public event Action<string>? AlertCleared; // sessionId

    public AlertDetector(string sessionId, string sessionName)
    {
        _sessionId = sessionId;
        _sessionName = sessionName;
    }

    public void Feed(string rawOutput)
    {
        string clean = StripAnsi(rawOutput);

        var lines = clean.Split('\n', StringSplitOptions.RemoveEmptyEntries);
        foreach (var line in lines)
        {
            string trimmed = line.Trim();
            if (trimmed.Length > 0) _lastLine = trimmed;
        }

        _idleTimer?.Dispose();
        _idleTimer = new System.Threading.Timer(OnIdle, null, 1500, Timeout.Infinite);
    }

    public void NotifyUserInteracted()
    {
        _alertFired = false;
        AlertCleared?.Invoke(_sessionId);
        _idleTimer?.Dispose();
        _idleTimer = null;
    }

    private void OnIdle(object? _)
    {
        if (_alertFired) return;
        if (string.IsNullOrWhiteSpace(_lastLine)) return;

        AlertType? alertType = null;

        if (s_claudeApproval.IsMatch(_lastLine))
            alertType = AlertType.ToolApproval;
        else if (s_prompt.IsMatch(_lastLine))
            alertType = AlertType.InputRequired;

        if (alertType.HasValue)
        {
            _alertFired = true;
            AlertRaised?.Invoke(new AlertEvent
            {
                SessionId = _sessionId,
                SessionName = _sessionName,
                Message = _lastLine.Length > 100 ? _lastLine[..100] + "…" : _lastLine,
                Type = alertType.Value
            });
        }
    }

    private static string StripAnsi(string raw) =>
        s_ansi.Replace(raw, "");

    private static readonly Regex s_ansi =
        new(@"\x1B\[[0-9;]*[mGKHFJABCDsuhl]|\x1B\].*?\x07|\x1B[=>]", RegexOptions.Compiled);

    private static readonly Regex s_prompt =
        new(@"(\[y/N\]|\[Y/n\]|\[yes/no\]|\(y/n\)|\(yes/no\)|>\s*$|\?\s*›)", RegexOptions.IgnoreCase | RegexOptions.Compiled);

    private static readonly Regex s_claudeApproval =
        new(@"(Do you want to|Allow|Approve|Bash command|Continue\?|Proceed\?|tool_use|permission)", RegexOptions.IgnoreCase | RegexOptions.Compiled);

    public void Dispose()
    {
        _idleTimer?.Dispose();
    }
}

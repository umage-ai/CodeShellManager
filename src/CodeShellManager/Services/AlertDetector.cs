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
    private volatile bool _stoodDown;

    public event Action<AlertEvent>? AlertRaised;
    public event Action<string>? AlertCleared; // sessionId

    public AlertDetector(string sessionId, string sessionName)
    {
        _sessionId = sessionId;
        _sessionName = sessionName;
    }

    /// <summary>True once <see cref="StandDown"/> ran; sticky for this detector's lifetime.</summary>
    public bool IsStoodDown => _stoodDown;

    /// <summary>
    /// The program reports its own state through OSC 7501, so the screen heuristic stops
    /// guessing — its regexes would otherwise fight the reported state, e.g. raise
    /// "approval needed" off a line of output while the program says it is working. Same
    /// pattern as the git poller standing down after an OSC 9001 branch. User-interaction
    /// notifications still fire: they are how a session acknowledges a done/error state.
    /// </summary>
    public void StandDown()
    {
        _stoodDown = true;
        _idleTimer?.Dispose();
        _idleTimer = null;
    }

    public void Feed(string rawOutput)
    {
        if (_stoodDown) return;
        string clean = StripAnsi(rawOutput);

        var lines = clean.Split('\n', StringSplitOptions.RemoveEmptyEntries);
        foreach (var line in lines)
        {
            string trimmed = line.Trim();
            if (trimmed.Length > 0) _lastLine = trimmed;
        }

        _idleTimer?.Dispose();
        // 1.5s idle: long enough for Claude's prompt redraw bursts to settle, short enough to feel responsive.
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
        if (_alertFired || _stoodDown) return;
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

    internal static string StripAnsi(string raw) =>
        s_ansi.Replace(raw, "");

    // OSC strings end in BEL (\x07) or ST (ESC \). Matching only BEL made an ST-terminated
    // OSC either leak its payload into prompt matching or lazily swallow real output up to
    // the next BEL. The `?` in the CSI class covers private-mode sequences (ESC[?25h).
    // Same pattern as OutputIndexer.AnsiPattern — keep the two in step.
    private static readonly Regex s_ansi =
        new(@"\x1B\[[?0-9;]*[mGKHFJABCDsuhlct]|\x1B\].*?(?:\x07|\x1B\\)|\x1B[=>]", RegexOptions.Compiled);

    // Matches Claude's "❯" prompt (U+276F), generic y/n prompts, and "?" questions
    private static readonly Regex s_prompt =
        new(@"(\[y/N\]|\[Y/n\]|\[yes/no\]|\(y/n\)|\(yes/no\)|>\s*$|\u276f\s*$|\?\s*›|\?\s*$)", RegexOptions.IgnoreCase | RegexOptions.Compiled);

    private static readonly Regex s_claudeApproval =
        new(@"(Do you want to|Allow|Approve|Bash command|Continue\?|Proceed\?|tool_use|permission)", RegexOptions.IgnoreCase | RegexOptions.Compiled);

    public void Dispose()
    {
        _idleTimer?.Dispose();
    }
}

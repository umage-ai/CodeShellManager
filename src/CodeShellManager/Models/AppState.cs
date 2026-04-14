using System.Collections.Generic;

namespace CodeShellManager.Models;

public class AppSettings
{
    public bool AutoRestoreSessions { get; set; } = true;
    public bool ShowToastNotifications { get; set; } = true;
    public string AnthropicApiKey { get; set; } = "";
    public string DefaultCommand { get; set; } = "claude";
    public string DefaultWorkingFolder { get; set; } = "";
    public bool ShowGitBranch { get; set; } = true;
    public bool SearchCollapseAfterNavigate { get; set; } = true;
    public string Theme { get; set; } = "dark";
    public int MaxSearchResults { get; set; } = 100;
    public bool ShowTerminalStatusDot { get; set; } = true;

    public List<string> LaunchCommands { get; set; } =
    [
        "claude",
        "claude --continue",
        "claude --model claude-opus-4-6",
        "claude --dangerously-skip-permissions",
        "codex",
        "gh copilot suggest",
        "pwsh",
        "cmd",
    ];
}

public class AppState
{
    public List<ShellSession> Sessions { get; set; } = [];
    public List<SessionGroup> Groups { get; set; } = [new SessionGroup { Name = "Default" }];
    public string LastLayout { get; set; } = "Single";
    public AppSettings Settings { get; set; } = new();
}

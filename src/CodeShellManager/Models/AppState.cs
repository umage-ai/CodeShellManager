using System.Collections.Generic;

namespace CodeShellManager.Models;

public class AppSettings
{
    public bool AutoRestoreSessions { get; set; } = true;
    public bool ShowToastNotifications { get; set; } = true;
    public string AnthropicApiKey { get; set; } = "";
    public string DefaultCommand { get; set; } = "claude";
    public string DefaultWorkingFolder { get; set; } = "";
}

public class AppState
{
    public List<ShellSession> Sessions { get; set; } = [];
    public List<SessionGroup> Groups { get; set; } = [new SessionGroup { Name = "Default" }];
    public string LastLayout { get; set; } = "Single";
    public AppSettings Settings { get; set; } = new();
}

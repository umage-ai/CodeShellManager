using System.Collections.Generic;

namespace CodeShellManager.Services;

public record CommandPreset(string Label, string Command, string Args = "", string Description = "");

public static class CommandPresetsService
{
    public static IReadOnlyList<CommandPreset> LaunchPresets =>
    [
        new("Claude (default)",       "claude",   "",                          "Start Claude Code"),
        new("Claude - Continue",      "claude",   "--continue",                "Resume last conversation"),
        new("Claude - Opus 4",        "claude",   "--model claude-opus-4-6",   "Use Opus 4 model"),
        new("Claude - Sonnet 4",      "claude",   "--model claude-sonnet-4-6", "Use Sonnet 4 model"),
        new("Claude - No confirm",    "claude",   "--dangerously-skip-permissions", "Skip all confirmations"),
        new("Claude - Print mode",    "claude",   "--print",                   "Non-interactive output"),
        new("OpenAI Codex",           "codex",    "",                          "OpenAI Codex CLI"),
        new("GitHub Copilot",         "gh",       "copilot suggest",           "GitHub Copilot CLI"),
        new("PowerShell",             "pwsh",     "",                          "PowerShell 7+"),
        new("CMD",                    "cmd",      "",                          "Command Prompt"),
    ];

    public static IReadOnlyList<CommandPreset> InSessionShortcuts =>
    [
        new("/help",      "/help",    "", "Show Claude help"),
        new("/clear",     "/clear",   "", "Clear conversation"),
        new("/compact",   "/compact", "", "Compact conversation"),
        new("/status",    "/status",  "", "Show status"),
        new("/memory",    "/memory",  "", "Show memory"),
        new("/init",      "/init",    "", "Initialize project"),
        new("--continue", "",         "", "Flag: resume session"),
        new("exit",       "exit",     "", "Exit the shell"),
    ];
}

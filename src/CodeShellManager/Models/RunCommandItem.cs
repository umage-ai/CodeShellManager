using System;
using System.Collections.Generic;

namespace CodeShellManager.Models;

/// <summary>
/// How the run-command's command line is launched.
///   Process:    cmd /c "<cmd>"  — bare ConPTY child, the historical default.
///   PowerShell: pwsh.exe -EncodedCommand <b64>  (falls back to powershell.exe if pwsh is missing)
///               needed when the command relies on pipes, $env:, cmdlets, or other PS syntax.
/// SSH parents ignore this — remote runs always go through bash.
/// </summary>
public enum RunMode
{
    Process,
    PowerShell,
}

/// <summary>
/// One configured "run" command on a session. The user can have many of these;
/// exactly one is the default (driven by the toolbar ▶ button and F5 keybinding).
/// Persisted to state.json under <see cref="ShellSession.RunCommands"/>.
/// </summary>
public class RunCommandItem
{
    public string Id { get; set; } = Guid.NewGuid().ToString();
    public string Label { get; set; } = "";
    public string CommandLine { get; set; } = "";
    public bool IsDefault { get; set; }

    /// <summary>Host process for the command. Defaults to Process for back-compat with pre-existing state.json entries.</summary>
    public RunMode Mode { get; set; } = RunMode.Process;

    /// <summary>
    /// Optional URL opened in the default browser when the command exits with code 0.
    /// Null / empty disables. Invoked via ShellExecute, so the OS handles validation.
    /// </summary>
    public string? PostRunUrl { get; set; }

    /// <summary>
    /// Normalizes the list so exactly one item has IsDefault=true (when non-empty).
    /// If multiple are marked, the LAST one wins — this matches the editor dialog's
    /// "click to promote" UX, where the most recent user action is authoritative.
    /// If none are marked, the first item is promoted.
    /// </summary>
    public static void EnsureSingleDefault(List<RunCommandItem> items)
    {
        if (items.Count == 0) return;

        // Find the LAST item flagged default (or fall back to index 0 if none).
        int keep = -1;
        for (int i = items.Count - 1; i >= 0; i--)
        {
            if (items[i].IsDefault) { keep = i; break; }
        }
        if (keep < 0) keep = 0;

        for (int i = 0; i < items.Count; i++)
            items[i].IsDefault = (i == keep);
    }
}

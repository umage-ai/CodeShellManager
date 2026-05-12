using System;
using System.Collections.Generic;

namespace CodeShellManager.Models;

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

using System;
using System.IO;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>
/// What an edit changes about a session, and whether the running terminal can absorb it.
/// </summary>
/// <param name="AnyChange">False when the draft is identical to the session.</param>
/// <param name="RequiresRelaunch">
/// True when the change can only take effect by tearing down and restarting the PTY —
/// see <see cref="SessionConfigEditor.Diff"/> for the exact rules.
/// </param>
/// <param name="WorkingFolderChanged">True when the local working folder moved (git info must be re-resolved).</param>
/// <param name="AppearanceChanged">True when any per-session appearance override differs.</param>
public readonly record struct SessionConfigChange(
    bool AnyChange,
    bool RequiresRelaunch,
    bool WorkingFolderChanged,
    bool AppearanceChanged);

/// <summary>
/// Diffs and applies a <see cref="SessionConfigDraft"/> onto an existing
/// <see cref="ShellSession"/>. Pure logic — no WPF, no PTY — so the "does this need a
/// restart?" rules are unit-testable.
/// </summary>
public static class SessionConfigEditor
{
    public static SessionConfigChange Diff(ShellSession s, SessionConfigDraft d)
    {
        bool modeChanged = d.IsRemote != s.IsRemote;

        // Only meaningful while the session stays local — a mode flip already forces a
        // relaunch, and stale ssh/folder leftovers from the other mode shouldn't count.
        bool folderChanged = !d.IsRemote && !s.IsRemote
            && !PathsEqual(d.WorkingFolder, s.WorkingFolder);

        bool sshChanged = d.IsRemote && s.IsRemote
            && (!Eq(d.SshUser, s.SshUser)
                || !Eq(d.SshHost, s.SshHost)
                || d.SshPort != s.SshPort
                || !Eq(d.SshRemoteFolder, s.SshRemoteFolder));

        bool launchChanged = !Eq(d.Command, s.Command) || !Eq(d.Args, s.Args);

        bool appearanceChanged =
            d.ProfileFontFamily != s.ProfileFontFamily
            || d.ProfileFontSize != s.ProfileFontSize
            || d.ProfileFontWeight != s.ProfileFontWeight
            || d.ProfileFontLigatures != s.ProfileFontLigatures
            || d.ProfileCursorShape != s.ProfileCursorShape
            || d.ProfileCursorBlink != s.ProfileCursorBlink
            || d.ProfilePadding != s.ProfilePadding
            || d.ProfileBackgroundOpacity != s.ProfileBackgroundOpacity
            || d.ProfileRetroEffect != s.ProfileRetroEffect
            || d.ProfileColorSchemeJson != s.ProfileColorSchemeJson;

        // Transparency picks a different xterm host page (terminal-transparent.html), which
        // is chosen at navigation time — crossing the boundary needs a fresh WebView2 load.
        bool transparencyChanged =
            (d.ProfileBackgroundOpacity is < 1.0) != (s.ProfileBackgroundOpacity is < 1.0);

        // TerminalBridge.ApplyProfileOverrides only ever *sets* options, so an override that
        // goes back to null can't be undone on the live terminal.
        bool overridesCleared = Cleared(d.ProfileFontFamily, s.ProfileFontFamily)
            || Cleared(d.ProfileFontSize, s.ProfileFontSize)
            || Cleared(d.ProfileFontWeight, s.ProfileFontWeight)
            || Cleared(d.ProfileFontLigatures, s.ProfileFontLigatures)
            || Cleared(d.ProfileCursorShape, s.ProfileCursorShape)
            || Cleared(d.ProfileCursorBlink, s.ProfileCursorBlink)
            || Cleared(d.ProfilePadding, s.ProfilePadding)
            || Cleared(d.ProfileRetroEffect, s.ProfileRetroEffect)
            || Cleared(d.ProfileColorSchemeJson, s.ProfileColorSchemeJson);

        bool anyChange = modeChanged || folderChanged || sshChanged || launchChanged
            || appearanceChanged || !Eq(d.Name, s.Name);

        bool requiresRelaunch = modeChanged || folderChanged || sshChanged || launchChanged
            || transparencyChanged || overridesCleared;

        return new SessionConfigChange(anyChange, requiresRelaunch, folderChanged, appearanceChanged);
    }

    /// <summary>
    /// Writes the draft onto the session. Every field the form owns is assigned verbatim —
    /// including blanks — so unchecking/clearing in the dialog actually clears the session.
    /// Runtime state (Id, GroupId, Status, RunCommands, IsDormant) is untouched.
    /// </summary>
    public static void Apply(ShellSession s, SessionConfigDraft d)
    {
        s.Name = d.Name;
        s.Command = d.Command;
        s.Args = d.Args;
        s.IsRemote = d.IsRemote;
        s.WorkingFolder = d.WorkingFolder;
        s.SshUser = d.SshUser;
        s.SshHost = d.SshHost;
        s.SshPort = d.SshPort;
        s.SshRemoteFolder = d.SshRemoteFolder;

        s.ProfileFontFamily = d.ProfileFontFamily;
        s.ProfileFontSize = d.ProfileFontSize;
        s.ProfileFontWeight = d.ProfileFontWeight;
        s.ProfileFontLigatures = d.ProfileFontLigatures;
        s.ProfileCursorShape = d.ProfileCursorShape;
        s.ProfileCursorBlink = d.ProfileCursorBlink;
        s.ProfilePadding = d.ProfilePadding;
        s.ProfileBackgroundOpacity = d.ProfileBackgroundOpacity;
        s.ProfileRetroEffect = d.ProfileRetroEffect;
        s.ProfileColorSchemeJson = d.ProfileColorSchemeJson;
    }

    private static bool Eq(string? a, string? b) =>
        string.Equals(a ?? "", b ?? "", StringComparison.Ordinal);

    private static bool Cleared<T>(T? now, T? before) => now is null && before is not null;
    private static bool Cleared(string? now, string? before) =>
        string.IsNullOrEmpty(now) && !string.IsNullOrEmpty(before);

    /// <summary>Case-insensitive path compare that tolerates trailing slashes and bad input.</summary>
    internal static bool PathsEqual(string a, string b)
    {
        if (string.IsNullOrWhiteSpace(a) || string.IsNullOrWhiteSpace(b))
            return string.IsNullOrWhiteSpace(a) && string.IsNullOrWhiteSpace(b);
        try
        {
            string na = Path.TrimEndingDirectorySeparator(Path.GetFullPath(a));
            string nb = Path.TrimEndingDirectorySeparator(Path.GetFullPath(b));
            return string.Equals(na, nb, StringComparison.OrdinalIgnoreCase);
        }
        catch
        {
            return string.Equals(a.Trim(), b.Trim(), StringComparison.OrdinalIgnoreCase);
        }
    }
}

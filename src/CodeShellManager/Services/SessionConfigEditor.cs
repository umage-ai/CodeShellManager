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
/// <param name="WorkingFolderChanged">True when the local folder or WSL distro/Linux folder moved (git info must be re-resolved).</param>
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
        bool modeChanged = d.Kind != s.Kind;

        // Kind-specific fields only count while the kind is unchanged — a kind flip already
        // forces a relaunch, and leftovers from a previous kind must not read as edits.
        bool sameKind = !modeChanged;
        bool folderChanged = sameKind && s.Kind == SessionKind.Local
            && !PathsEqual(d.WorkingFolder, s.WorkingFolder);

        bool sshChanged = sameKind && s.Kind == SessionKind.Ssh
            && (!Eq(d.SshUser, s.SshUser)
                || !Eq(d.SshHost, s.SshHost)
                || d.SshPort != s.SshPort
                || !Eq(d.SshRemoteFolder, s.SshRemoteFolder));

        bool wslFolderChanged = sameKind && s.Kind == SessionKind.Wsl
            && (!Eq(d.WslDistro, s.WslDistro)
                || !LinuxPathsEqual(d.WslWorkingFolder, s.WslWorkingFolder));
        bool wslChanged = wslFolderChanged
            || (sameKind && s.Kind == SessionKind.Wsl && !Eq(d.WslUser, s.WslUser));

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

        bool anyChange = modeChanged || folderChanged || sshChanged || wslChanged || launchChanged
            || appearanceChanged || !Eq(d.Name, s.Name);

        bool requiresRelaunch = modeChanged || folderChanged || sshChanged || wslChanged || launchChanged
            || transparencyChanged || overridesCleared;

        return new SessionConfigChange(anyChange, requiresRelaunch, folderChanged || wslFolderChanged, appearanceChanged);
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
        s.Kind = d.Kind;
        s.SshUser = d.SshUser;
        s.SshHost = d.SshHost;
        s.SshPort = d.SshPort;
        s.SshRemoteFolder = d.SshRemoteFolder;
        s.WslDistro = d.WslDistro;
        s.WslUser = d.WslUser;
        s.WslWorkingFolder = d.WslWorkingFolder.Trim();
        // WSL sessions keep WorkingFolder as the \\wsl$ UNC mirror of the Linux path so
        // Explorer, git polling and the sidebar need no special-casing (see CLAUDE.md
        // "WSL Sessions"). Derive it here so the two can never drift apart.
        s.WorkingFolder = d.Kind == SessionKind.Wsl
            ? WslDiscoveryService.ToUncPath(d.WslDistro, s.WslWorkingFolder)
            : d.WorkingFolder;

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

    /// <summary>Linux path compare: exact, trailing-slash tolerant, case-sensitive (ext4 is).</summary>
    internal static bool LinuxPathsEqual(string a, string b) =>
        string.Equals((a ?? "").Trim().TrimEnd('/'), (b ?? "").Trim().TrimEnd('/'), StringComparison.Ordinal);

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

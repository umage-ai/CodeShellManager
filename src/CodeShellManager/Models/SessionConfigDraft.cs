using System;

namespace CodeShellManager.Models;

/// <summary>
/// A flat, UI-free snapshot of everything the New/Edit Session form can change on a
/// <see cref="ShellSession"/>. The dialog produces one of these
/// (<c>NewSessionDialog.ToDraft()</c>); <see cref="Services.SessionConfigEditor"/>
/// diffs it against the live session and applies it. Keeping the shape separate from
/// the dialog is what makes the edit rules unit-testable without a WPF window.
/// </summary>
public sealed class SessionConfigDraft
{
    public string Name { get; set; } = "";

    // Local
    public string WorkingFolder { get; set; } = "";

    // Launch
    public string Command { get; set; } = "";
    public string Args { get; set; } = "";

    // Remote
    public bool IsRemote { get; set; }
    public string SshUser { get; set; } = "";
    public string SshHost { get; set; } = "";
    public int SshPort { get; set; } = 22;
    public string SshRemoteFolder { get; set; } = "";

    // Appearance overrides — null means "use the global terminal settings"
    public string? ProfileFontFamily { get; set; }
    public int? ProfileFontSize { get; set; }
    public string? ProfileFontWeight { get; set; }
    public bool? ProfileFontLigatures { get; set; }
    public string? ProfileCursorShape { get; set; }
    public bool? ProfileCursorBlink { get; set; }
    public string? ProfilePadding { get; set; }
    public double? ProfileBackgroundOpacity { get; set; }
    public bool? ProfileRetroEffect { get; set; }
    public string? ProfileColorSchemeJson { get; set; }

    /// <summary>Captures the current configuration of <paramref name="s"/> as a draft.</summary>
    public static SessionConfigDraft FromSession(ShellSession s) => new()
    {
        Name = s.Name,
        WorkingFolder = s.WorkingFolder,
        Command = s.Command,
        Args = s.Args,
        IsRemote = s.IsRemote,
        SshUser = s.SshUser,
        SshHost = s.SshHost,
        SshPort = s.SshPort,
        SshRemoteFolder = s.SshRemoteFolder,
        ProfileFontFamily = s.ProfileFontFamily,
        ProfileFontSize = s.ProfileFontSize,
        ProfileFontWeight = s.ProfileFontWeight,
        ProfileFontLigatures = s.ProfileFontLigatures,
        ProfileCursorShape = s.ProfileCursorShape,
        ProfileCursorBlink = s.ProfileCursorBlink,
        ProfilePadding = s.ProfilePadding,
        ProfileBackgroundOpacity = s.ProfileBackgroundOpacity,
        ProfileRetroEffect = s.ProfileRetroEffect,
        ProfileColorSchemeJson = s.ProfileColorSchemeJson,
    };
}

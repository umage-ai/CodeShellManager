using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json.Serialization;

namespace CodeShellManager.Models;

/// <summary>
/// A snapshot of a recently-closed session, kept so the user can reopen it with
/// <c>Ctrl+Shift+T</c> or via the "Recently closed" list in the New Session dialog.
/// Stored on <see cref="AppState.RecentlyClosed"/> (capped — see
/// <c>MainViewModel.MaxRecentlyClosed</c>) and persisted to <c>state.json</c>.
///
/// Deliberately separate from <see cref="ShellSession"/> so PTY/runtime fields
/// (IsDormant, Status, LastActivityAt) don't leak into the ring buffer and so the
/// model can evolve independently.
/// </summary>
public class RecentlyClosedEntry
{
    public string Name { get; set; } = "";
    public string WorkingFolder { get; set; } = "";
    public string Command { get; set; } = "claude";
    public string Args { get; set; } = "";
    public string GroupId { get; set; } = "";
    public string? ColorOverride { get; set; }

    /// <summary>
    /// Kind of the closed session, so a reopened WSL or SSH session comes back as the same
    /// kind instead of Local at a UNC. Legacy entries carried only <c>IsRemote</c>; see
    /// <see cref="LegacyIsRemote"/>.
    /// </summary>
    public SessionKind Kind { get; set; } = SessionKind.Local;

    [JsonIgnore]
    public bool IsRemote => Kind == SessionKind.Ssh;

    /// <summary>Legacy <c>"IsRemote"</c> JSON slot — see <see cref="ShellSession.LegacyIsRemote"/>.</summary>
    [JsonPropertyName("IsRemote")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? LegacyIsRemote { get; set; }

    public void MigrateLegacyFields()
    {
        if (LegacyIsRemote == true && Kind == SessionKind.Local) Kind = SessionKind.Ssh;
        LegacyIsRemote = null;
    }

    public string SshUser { get; set; } = "";
    public string SshHost { get; set; } = "";
    public int SshPort { get; set; } = 22;
    public string SshRemoteFolder { get; set; } = "";

    public string WslDistro { get; set; } = "";
    public string WslUser { get; set; } = "";
    public string WslWorkingFolder { get; set; } = "";

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

    public List<RunCommandItem> RunCommands { get; set; } = new();

    public DateTime ClosedAt { get; set; } = DateTime.UtcNow;

    /// <summary>
    /// Shallow copies the persistable fields of a <see cref="ShellSession"/>.
    /// RunCommands are deep-copied with fresh Ids so editing the reopened session
    /// can't accidentally mutate the entry still in the ring buffer.
    /// </summary>
    public static RecentlyClosedEntry FromSession(ShellSession s) => new()
    {
        Name = s.Name,
        WorkingFolder = s.WorkingFolder,
        Command = s.Command,
        Args = s.Args,
        GroupId = s.GroupId,
        ColorOverride = s.ColorOverride,
        Kind = s.Kind,
        SshUser = s.SshUser,
        SshHost = s.SshHost,
        SshPort = s.SshPort,
        SshRemoteFolder = s.SshRemoteFolder,
        WslDistro = s.WslDistro,
        WslUser = s.WslUser,
        WslWorkingFolder = s.WslWorkingFolder,
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
        RunCommands = s.RunCommands.Select(r => new RunCommandItem
        {
            Id = Guid.NewGuid().ToString(),
            Label = r.Label,
            CommandLine = r.CommandLine,
            IsDefault = r.IsDefault,
            Mode = r.Mode,
            PostRunUrl = r.PostRunUrl,
        }).ToList(),
        ClosedAt = DateTime.UtcNow,
    };

    /// <summary>Friendly subtitle for the recents UI — kind-specific locator.</summary>
    [JsonIgnore]
    public string Subtitle => Kind switch
    {
        SessionKind.Ssh => string.IsNullOrWhiteSpace(SshUser) ? SshHost : $"{SshUser}@{SshHost}",
        SessionKind.Wsl => string.IsNullOrEmpty(WslWorkingFolder)
            ? WslDistro
            : $"{WslDistro}: {WslWorkingFolder}",
        _ => WorkingFolder,
    };
}

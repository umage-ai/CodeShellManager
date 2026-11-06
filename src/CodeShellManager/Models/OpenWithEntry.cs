using System;
using System.Collections.Generic;

namespace CodeShellManager.Models;

/// <summary>
/// One "Open with…" target — an editor, IDE or folder viewer that a session's folder can be
/// opened in (issue #178). Stored in <see cref="AppSettings.OpenWith"/>; seeded once by
/// <see cref="Services.OpenWithDetector"/>, then owned by the user.
/// </summary>
/// <remarks>
/// Each argument template is a whitespace-separated list of tokens (double quotes group a
/// token containing spaces). Placeholders are substituted <b>inside</b> a token, after the
/// template has been split, so a value can never become more than one argv element. A
/// null/blank template means the entry does not apply to that session kind and is hidden
/// for it. See <see cref="Services.OpenWithService"/> for the placeholders and the rules.
/// </remarks>
public class OpenWithEntry
{
    public string Id { get; set; } = Guid.NewGuid().ToString();
    public string Label { get; set; } = "";
    /// <summary>Absolute path to the program. Never a shell — see <see cref="Services.OpenWithService.ValidateExecutable"/>.</summary>
    public string Executable { get; set; } = "";
    /// <summary>Arguments for a local session — e.g. <c>{folder}</c>.</summary>
    public string? LocalArgs { get; set; }
    /// <summary>Arguments for a WSL session — e.g. <c>--remote wsl+{distro} {linuxPath}</c>.</summary>
    public string? WslArgs { get; set; }
    /// <summary>Arguments for an SSH session — e.g. <c>--remote ssh-remote+{userAtHost} {remoteFolder}</c>.</summary>
    public string? SshArgs { get; set; }
    /// <summary>Exactly one entry is the default — the toolbar <c>{ }</c> button opens it.</summary>
    public bool IsDefault { get; set; }

    public string? ArgsFor(SessionKind kind) => kind switch
    {
        SessionKind.Local => LocalArgs,
        SessionKind.Wsl => WslArgs,
        SessionKind.Ssh => SshArgs,
        _ => null,
    };

    public bool AppliesTo(SessionKind kind) => !string.IsNullOrWhiteSpace(ArgsFor(kind));

    public OpenWithEntry Clone() => new()
    {
        Id = Id,
        Label = Label,
        Executable = Executable,
        LocalArgs = LocalArgs,
        WslArgs = WslArgs,
        SshArgs = SshArgs,
        IsDefault = IsDefault,
    };

    /// <summary>Leaves exactly one default when the list is non-empty: the first flagged, else the first entry.</summary>
    public static void EnsureSingleDefault(IList<OpenWithEntry> list)
    {
        bool seen = false;
        foreach (var e in list)
        {
            if (e.IsDefault && !seen) seen = true;
            else e.IsDefault = false;
        }
        if (!seen && list.Count > 0) list[0].IsDefault = true;
    }
}

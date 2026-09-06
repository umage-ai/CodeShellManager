using System;
using System.Collections.Generic;
using System.Text;
using System.Text.Json.Serialization;

namespace CodeShellManager.Models;

public enum SessionStatus { Idle, Running, NeedsAttention, Exited }

/// <summary>
/// Kind of pseudo-terminal session. <see cref="Local"/> runs a Windows process directly,
/// <see cref="Ssh"/> tunnels through the system ssh client, <see cref="Wsl"/> launches
/// a shell inside a WSL distro via <c>wsl.exe</c>.
/// </summary>
public enum SessionKind { Local, Ssh, Wsl }

public class ShellSession
{
    public string Id { get; set; } = Guid.NewGuid().ToString();
    public string Name { get; set; } = "";
    public string WorkingFolder { get; set; } = "";
    public string Command { get; set; } = "claude";
    public string Args { get; set; } = "";
    public string GroupId { get; set; } = "";
    public string? ColorOverride { get; set; }
    public SessionStatus Status { get; set; } = SessionStatus.Idle;
    public DateTime CreatedAt { get; set; } = DateTime.UtcNow;

    /// <summary>
    /// Last time the user gave this session focus (clicked it in the sidebar, woke it,
    /// or it was selected via Ctrl+Tab). Persisted so "Sort by last active" survives
    /// restarts. Initializer is <c>default</c> so that legacy state.json files (which
    /// don't carry this key) deserialize to a sentinel; <see cref="SessionManager.LoadFromState"/>
    /// then backfills it from <see cref="CreatedAt"/>. New sessions are populated by
    /// <see cref="SessionManager.CreateSession"/>.
    /// </summary>
    public DateTime LastActivityAt { get; set; } = default;

    /// <summary>
    /// When true, the session has no live PTY/terminal — it is a placeholder
    /// in the sidebar that can be "woken" later. Persisted to state.json.
    /// </summary>
    public bool IsDormant { get; set; }

    /// <summary>
    /// Authoritative session kind. Everything that branches on session type reads this.
    /// Legacy state.json files (pre-Kind) only carried <c>IsRemote</c>; see
    /// <see cref="LegacyIsRemote"/> and <see cref="MigrateLegacyFields"/>.
    /// </summary>
    public SessionKind Kind { get; set; } = SessionKind.Local;

    /// <summary>
    /// Convenience view of <see cref="Kind"/> for the SSH case. Setting <c>true</c> makes
    /// the session SSH; setting <c>false</c> on an SSH session makes it Local. It is
    /// deliberately NOT persisted — <see cref="Kind"/> is — and it carries no migration
    /// logic. A WSL session is unaffected by <c>IsRemote = false</c>.
    /// </summary>
    [JsonIgnore]
    public bool IsRemote
    {
        get => Kind == SessionKind.Ssh;
        set
        {
            if (value) Kind = SessionKind.Ssh;
            else if (Kind == SessionKind.Ssh) Kind = SessionKind.Local;
        }
    }

    /// <summary>
    /// Read-only compatibility slot for the pre-<see cref="Kind"/> <c>"IsRemote"</c> JSON
    /// key. Populated only when an old file is deserialised; <see cref="MigrateLegacyFields"/>
    /// folds it into <see cref="Kind"/> and nulls it so it is never written back.
    /// </summary>
    [JsonPropertyName("IsRemote")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? LegacyIsRemote { get; set; }

    /// <summary>
    /// Folds legacy JSON fields into their current representation. Idempotent. Called by
    /// <c>StateService.Normalize</c> for every loaded or imported session — the loader is
    /// the one place that knows it is looking at possibly-old data.
    /// </summary>
    public void MigrateLegacyFields()
    {
        if (LegacyIsRemote == true && Kind == SessionKind.Local) Kind = SessionKind.Ssh;
        LegacyIsRemote = null;
    }

    /// <summary>True iff this session runs inside a WSL distro via wsl.exe.</summary>
    [JsonIgnore]
    public bool IsWsl => Kind == SessionKind.Wsl;

    // SSH / remote session fields
    public string SshUser { get; set; } = "";
    public string SshHost { get; set; } = "";
    public int SshPort { get; set; } = 22;
    public string SshRemoteFolder { get; set; } = "";

    // WSL session fields
    /// <summary>Name of the WSL distro (matches <c>wsl -l -q</c>), e.g. "Ubuntu".</summary>
    public string WslDistro { get; set; } = "";
    /// <summary>Optional WSL user override (<c>wsl -u &lt;user&gt;</c>). Empty = the distro's default user.</summary>
    public string WslUser { get; set; } = "";
    /// <summary>Linux-style working folder inside the distro, e.g. "/home/alice/project". Empty = the user's home.</summary>
    public string WslWorkingFolder { get; set; } = "";

    // Per-session appearance overrides (typically populated from a Windows
    // Terminal profile via NewSessionDialog). All nullable — null means "use the
    // global terminal settings". Persisted to state.json so a session relaunches
    // with the same look.
    public string? ProfileFontFamily { get; set; }
    public int? ProfileFontSize { get; set; }
    public string? ProfileFontWeight { get; set; }
    public bool? ProfileFontLigatures { get; set; }
    public string? ProfileCursorShape { get; set; }
    public bool? ProfileCursorBlink { get; set; }
    public string? ProfilePadding { get; set; }
    public double? ProfileBackgroundOpacity { get; set; }
    public bool? ProfileRetroEffect { get; set; }
    /// <summary>Pre-baked xterm theme object (JSON), or null for xterm default.</summary>
    public string? ProfileColorSchemeJson { get; set; }

    /// <summary>
    /// Configured run commands for this session — the source for the toolbar ▶ button
    /// and the chips strip. Seeded at session creation from <see cref="Services.RunCommandTemplatesService"/>.
    /// Exactly one item has IsDefault=true (when the list is non-empty);
    /// see <see cref="RunCommandItem.EnsureSingleDefault"/>.
    /// </summary>
    public List<RunCommandItem> RunCommands { get; set; } = new();

    /// <summary>
    /// Full command line for display. Never throws: an incomplete session (blank SSH host,
    /// blank WSL distro) shows just the executable — this string is used in error dialogs
    /// on exactly those paths.
    /// </summary>
    [JsonIgnore]
    public string FullCommandLine => Kind switch
    {
        SessionKind.Ssh => string.IsNullOrWhiteSpace(SshHost) ? "ssh" : $"ssh {BuildSshArgs()}",
        SessionKind.Wsl => string.IsNullOrWhiteSpace(WslDistro) ? "wsl.exe" : $"wsl.exe {BuildWslArgs()}",
        _ => string.IsNullOrWhiteSpace(Args) ? Command : $"{Command} {Args}",
    };

    /// <summary>
    /// Null when the session has everything it needs to launch; otherwise a sentence for
    /// the user. Checked at the top of <c>MainWindow.LaunchSessionAsync</c> BEFORE any
    /// WebView2 or PTY is created, because the arg builders throw on these and a throw at
    /// that point leaks the pane. state.json and imports are untrusted input.
    /// </summary>
    [JsonIgnore]
    public string? LaunchValidationError => Kind switch
    {
        SessionKind.Ssh when string.IsNullOrWhiteSpace(SshHost) => "This SSH session has no host. Edit the session and set one.",
        SessionKind.Wsl when string.IsNullOrWhiteSpace(WslDistro) => "This WSL session has no distro. Edit the session and pick one.",
        _ => null,
    };

    /// <summary>
    /// Builds the argument string passed to the ssh executable.
    /// Example: "-t alice@dev.example.com \"cd '/proj' && bash\""
    /// </summary>
    internal string BuildSshArgs()
    {
        if (string.IsNullOrWhiteSpace(SshHost))
            throw new InvalidOperationException("SshHost must be set for remote sessions.");
        var sb = new StringBuilder();
        if (SshPort != 22)
            sb.Append($"-p {SshPort} ");
        sb.Append("-t ");
        var userAtHost = string.IsNullOrWhiteSpace(SshUser) ? SshHost : $"{SshUser}@{SshHost}";
        sb.Append(userAtHost);
        sb.Append(" \"");
        if (!string.IsNullOrWhiteSpace(SshRemoteFolder))
            sb.Append($"cd '{SshRemoteFolder}' && ");
        var shell = string.IsNullOrWhiteSpace(Command) ? "bash" : Command;
        sb.Append(shell);
        if (!string.IsNullOrWhiteSpace(Args))
            sb.Append($" {Args}");
        sb.Append("\"");
        return sb.ToString();
    }

    /// <summary>
    /// Win32 (MSVCRT / CommandLineToArgvW) argument quoting. Space-free, quote-free values
    /// are returned unchanged unless <paramref name="force"/> is set. Inside quotes, a
    /// run of n backslashes followed by <c>"</c> becomes 2n+1 backslashes + quote, and a
    /// trailing run of n backslashes becomes 2n so it cannot eat the closing quote.
    /// Every value that reaches wsl.exe goes through here — no ad-hoc Replace.
    /// </summary>
    internal static string QuoteForCmd(string value, bool force = false)
    {
        value ??= "";
        if (!force && value.Length > 0 && value.IndexOfAny(new[] { ' ', '\t', '"' }) < 0)
            return value;

        var sb = new StringBuilder(value.Length + 2);
        sb.Append('"');
        int backslashes = 0;
        foreach (char c in value)
        {
            if (c == '\\') { backslashes++; continue; }
            if (c == '"')
            {
                sb.Append('\\', backslashes * 2 + 1).Append('"');
                backslashes = 0;
                continue;
            }
            sb.Append('\\', backslashes).Append(c);
            backslashes = 0;
        }
        sb.Append('\\', backslashes * 2);
        sb.Append('"');
        return sb.ToString();
    }

    /// <summary>
    /// Builds the argument string passed to wsl.exe:
    /// <c>-d &lt;distro&gt; [-u &lt;user&gt;] [--cd &lt;linux-folder&gt;] -- bash -lc "&lt;payload&gt;"</c>.
    /// The payload is <see cref="Command"/> + <see cref="Args"/>, or <paramref name="inner"/>
    /// when given (run commands). It is wrapped in <c>bash -lc</c> so PATH-resolved tools
    /// (nvm node, pyenv, …) behave as in a login shell; bash then interprets the payload as
    /// a shell command line, which is the intent. Throws when <see cref="WslDistro"/> is
    /// blank — callers validate first (<see cref="LaunchValidationError"/>).
    /// </summary>
    internal string BuildWslArgs(string? inner = null)
    {
        if (string.IsNullOrWhiteSpace(WslDistro))
            throw new InvalidOperationException("WslDistro must be set for WSL sessions.");
        var sb = new StringBuilder();
        sb.Append("-d ").Append(QuoteForCmd(WslDistro));
        if (!string.IsNullOrWhiteSpace(WslUser))
            sb.Append(" -u ").Append(QuoteForCmd(WslUser));
        if (!string.IsNullOrWhiteSpace(WslWorkingFolder))
            sb.Append(" --cd ").Append(QuoteForCmd(WslWorkingFolder));
        if (inner is null)
        {
            var shell = string.IsNullOrWhiteSpace(Command) ? "bash" : Command;
            inner = string.IsNullOrWhiteSpace(Args) ? shell : $"{shell} {Args}";
        }
        sb.Append(" -- bash -lc ").Append(QuoteForCmd(inner, force: true));
        return sb.ToString();
    }

    // ── Display helpers (single source of truth — see MainWindow sidebar / VM) ────

    /// <summary>
    /// Subtitle-line text for the sidebar: a short, kind-appropriate locator.
    /// Local → working folder leaf; Ssh → host; Wsl → <c>distro:linux-leaf</c>.
    /// </summary>
    [JsonIgnore]
    public string FolderShort => Kind switch
    {
        SessionKind.Ssh => string.IsNullOrWhiteSpace(SshHost) ? "" : SshHost,
        SessionKind.Wsl => BuildWslFolderShort(),
        _ => string.IsNullOrEmpty(WorkingFolder)
            ? ""
            : new System.IO.DirectoryInfo(WorkingFolder).Name,
    };

    /// <summary>
    /// What to show as the session's label when <see cref="Name"/> is blank.
    /// </summary>
    [JsonIgnore]
    public string DefaultDisplayName => Kind switch
    {
        SessionKind.Ssh => string.IsNullOrWhiteSpace(SshHost) ? Command : SshHost,
        SessionKind.Wsl => string.IsNullOrWhiteSpace(WslDistro)
            ? Command
            : (string.IsNullOrEmpty(WslWorkingFolder)
                ? WslDistro
                : $"{WslDistro}: {System.IO.Path.GetFileName(WslWorkingFolder.TrimEnd('/'))}"),
        _ => System.IO.Path.GetFileName(WorkingFolder.TrimEnd('/', '\\')) ?? Command,
    };

    /// <summary>
    /// Key used by ColorService to pick a deterministic accent color. Worktree
    /// siblings share an accent via the repo-root override done in <see cref="ViewModels.SessionViewModel.AccentColor"/>;
    /// this is the base key when no repo-root is known.
    /// </summary>
    [JsonIgnore]
    public string AccentKey => Kind switch
    {
        SessionKind.Ssh => string.IsNullOrWhiteSpace(SshUser) ? SshHost : $"{SshUser}@{SshHost}",
        SessionKind.Wsl => $"wsl://{WslDistro}{WslWorkingFolder}",
        _ => WorkingFolder,
    };

    private string BuildWslFolderShort()
    {
        if (string.IsNullOrWhiteSpace(WslDistro)) return "";
        // Path.GetFileName understands both separators on Windows and returns ""
        // for empty input, so it covers our "WslWorkingFolder might be blank" case.
        string leaf = string.IsNullOrWhiteSpace(WslWorkingFolder)
            ? ""
            : System.IO.Path.GetFileName(WslWorkingFolder.TrimEnd('/'));
        return string.IsNullOrEmpty(leaf) ? WslDistro : $"{WslDistro}: {leaf}";
    }
}

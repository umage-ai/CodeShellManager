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
    /// Compatibility slot for the pre-<see cref="Kind"/> <c>"IsRemote"</c> JSON key.
    /// <c>origin/main</c> persists only this key and has no <see cref="Kind"/> at all — an
    /// older build reading a <c>state.json</c> written by this one must still see an SSH
    /// session as remote, so the getter is computed from <see cref="Kind"/> rather than
    /// left write-only: <c>true</c> for <see cref="SessionKind.Ssh"/>, otherwise <c>null</c>
    /// (omitted from the JSON entirely via <see cref="JsonIgnoreCondition.WhenWritingNull"/>
    /// — Local/Wsl sessions never carry this key).
    ///
    /// The setter does NOT share storage with the getter: during deserialization of an old
    /// file it only records the incoming legacy value into <see cref="_legacyIsRemoteIncoming"/>;
    /// <see cref="MigrateLegacyFields"/> folds that into <see cref="Kind"/> and clears it. A
    /// computed getter has no state to "null itself out" after migration — once Kind is Ssh,
    /// this property reads true again, which is correct (nothing left to migrate).
    /// </summary>
    [JsonPropertyName("IsRemote")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? LegacyIsRemote
    {
        get => Kind == SessionKind.Ssh ? true : (bool?)null;
        set => _legacyIsRemoteIncoming = value;
    }

    private bool? _legacyIsRemoteIncoming;

    /// <summary>
    /// Folds legacy JSON fields into their current representation. Idempotent. Called by
    /// <c>StateService.Normalize</c> for every loaded or imported session — the loader is
    /// the one place that knows it is looking at possibly-old data.
    /// </summary>
    public void MigrateLegacyFields()
    {
        if (_legacyIsRemoteIncoming == true && Kind == SessionKind.Local) Kind = SessionKind.Ssh;
        _legacyIsRemoteIncoming = null;
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

    /// <summary>
    /// Runtime-only cache of the distro's login shell ("bash" or "sh"), resolved by
    /// <see cref="Services.WslDiscoveryService.GetLoginShellAsync"/> in
    /// <c>MainWindow.LaunchSessionAsync</c> before <see cref="BuildWslArgs"/> is called.
    /// Deliberately NOT persisted — a distro's available shells can change between runs
    /// (e.g. a minimal image gains bash after an update), so this is always re-probed at
    /// launch rather than trusted from a prior session. Run commands share this via the
    /// same <see cref="ShellSession"/> instance, so they never re-probe. Null until resolved,
    /// in which case <see cref="BuildWslArgs"/> falls back to "bash".
    /// </summary>
    [JsonIgnore]
    internal string? ResolvedWslShell { get; set; }

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
            // Escaped, not raw. This lands inside single quotes in a remote shell command,
            // and SshRemoteFolder comes from state.json — which this file's own header notes
            // is untrusted input. A `'` in the value closed the quote and ran the remainder
            // on the remote host. Command/Args below are arbitrary by design; the folder is
            // not meant to be.
            sb.Append($"cd {PosixSingleQuote(SshRemoteFolder)} && ");
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
    /// <summary>
    /// Wraps a value in POSIX single quotes, escaping any embedded quote as <c>'\''</c>.
    /// For values interpolated into a *remote shell* command line (ssh), where Windows
    /// argv quoting is the wrong tool entirely. Mirrors RunInstance.SingleQuoteEscape.
    /// </summary>
    internal static string PosixSingleQuote(string value)
        => "'" + (value ?? "").Replace("'", "'\\''") + "'";

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
    /// <c>-d &lt;distro&gt; [-u &lt;user&gt;] [--cd &lt;linux-folder&gt;] -e &lt;shell&gt; -lc "&lt;payload&gt;"</c>.
    /// The payload is <see cref="Command"/> + <see cref="Args"/>, or <paramref name="inner"/>
    /// when given (run commands). It is wrapped in <c>&lt;shell&gt; -lc</c> so PATH-resolved
    /// tools (nvm node, pyenv, …) behave as in a login shell; the shell then interprets the
    /// payload as a shell command line, which is the intent. Shell is
    /// <see cref="ResolvedWslShell"/> when set (probed per-distro by
    /// <see cref="Services.WslDiscoveryService.GetLoginShellAsync"/>), else "bash".
    ///
    /// <c>-e</c> (not <c>--</c>) is deliberate: <c>wsl.exe &lt;cmd&gt; -- …</c> runs the
    /// trailing command through the distro's *default* login shell before anything after
    /// <c>--</c> ever runs, so a payload built for our shell gets expanded twice — once by
    /// that default shell (in the wrong environment) and once by ours. <c>-e</c>/<c>--exec</c>
    /// executes the given program directly, skipping that first pass. <c>--</c> looks more
    /// natural here — resist the urge to change it back; verified empirically:
    /// <c>wsl -d Ubuntu -- bash -lc 'for t in a b; do echo "L=$t"; done'</c> printed
    /// "L=" / "L=" (mangled) while the same command with <c>-e bash</c> printed "L=a" / "L=b".
    ///
    /// Throws when <see cref="WslDistro"/> is blank — callers validate first
    /// (<see cref="LaunchValidationError"/>).
    /// </summary>
    internal string BuildWslArgs(string? inner = null)
    {
        if (string.IsNullOrWhiteSpace(WslDistro))
            throw new InvalidOperationException("WslDistro must be set for WSL sessions.");
        string loginShell = ResolvedWslShell ?? "bash";
        var sb = new StringBuilder();
        sb.Append("-d ").Append(QuoteForCmd(WslDistro));
        if (!string.IsNullOrWhiteSpace(WslUser))
            sb.Append(" -u ").Append(QuoteForCmd(WslUser));
        // A blank folder means "the user's home" — that is what the dialog's "(optional)"
        // label promises. `--cd ~` is wsl.exe's own spelling for it and honours -u. Omitting
        // --cd entirely does NOT do this: wsl then inherits the launching *Windows* process's
        // cwd and lands the session in /mnt/c/... on the slow 9p mount. Pass ~ unquoted;
        // quoting it would make it a literal directory name.
        sb.Append(" --cd ").Append(string.IsNullOrWhiteSpace(WslWorkingFolder)
            ? "~"
            : QuoteForCmd(WslWorkingFolder));
        if (inner is null)
        {
            var shell = string.IsNullOrWhiteSpace(Command) ? loginShell : Command;
            inner = string.IsNullOrWhiteSpace(Args) ? shell : $"{shell} {Args}";
        }
        sb.Append(" -e ").Append(QuoteForCmd(loginShell)).Append(" -lc ").Append(QuoteForCmd(inner, force: true));
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
            // DirectoryInfo(...).Name throws ArgumentException on a path containing an
            // embedded NUL — reachable from state.json on the restore path. Path.GetFileName
            // (what DefaultDisplayName already uses) tolerates it.
            : System.IO.Path.GetFileName(WorkingFolder.TrimEnd('/', '\\')),
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

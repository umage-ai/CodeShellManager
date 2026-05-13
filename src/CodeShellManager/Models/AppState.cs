using System.Collections.Generic;

namespace CodeShellManager.Models;

/// <summary>
/// How the sidebar surfaces session groups.
/// None = no group UI at all (flat session list). FilterStrip = vertical tab strip
/// to the left of the sidebar, one filter at a time (default). InlineHeaders =
/// collapsible group headers inline in the sidebar, all groups visible at once.
/// </summary>
public enum GroupDisplayMode
{
    None,
    FilterStrip,
    InlineHeaders
}

/// <summary>
/// How per-session action icons (close / sleep / spawn) are surfaced on each sidebar
/// row. OnHover (default) keeps the sidebar visually calm — icons fade in only while
/// the row is hovered. Always shows them at all times (the legacy behaviour). Hidden
/// removes them entirely; users rely on the right-click context menu or the terminal
/// toolbar instead. The terminal toolbar always shows its own icons regardless.
/// </summary>
public enum SidebarActionIconsMode
{
    Hidden,
    OnHover,
    Always
}

public class AppSettings
{
    public bool AutoRestoreSessions { get; set; } = true;
    public bool AutoResumeClaude { get; set; } = true;
    /// <summary>
    /// Milliseconds to wait between consecutive Claude session launches (and shutdowns).
    /// The Claude CLI performs an unlocked read-modify-write on <c>~/.claude.json</c> on
    /// startup and exit, so two claude.exe processes touching the file at the same time
    /// can corrupt it. Spacing them out by ~2s avoids the race. 0 disables the stagger.
    /// </summary>
    public int ClaudeLaunchStaggerMs { get; set; } = 2000;
    public bool AutoFocusTerminalOnSelect { get; set; } = true;
    public bool ShowToastNotifications { get; set; } = false;
    public bool ShowNotificationSound { get; set; } = false;
    public string AnthropicApiKey { get; set; } = "";
    public string DefaultCommand { get; set; } = "claude";
    public string DefaultWorkingFolder { get; set; } = "";
    public bool ShowGitBranch { get; set; } = true;
    /// <summary>Authoritative grouping UI selector. Replaces the legacy <see cref="ShowGroupsTab"/> boolean.</summary>
    public GroupDisplayMode GroupDisplayMode { get; set; } = GroupDisplayMode.FilterStrip;
    /// <summary>
    /// In FilterStrip mode with an active group filter, restrict the terminal grid
    /// (multi-pane layouts) to sessions belonging to that group. The sidebar already
    /// hides non-matching rows; with this on, the panes match. Off = the grid keeps
    /// showing every live session regardless of group filter.
    /// </summary>
    public bool FilterGridByActiveGroup { get; set; } = true;
    /// <summary>
    /// Remember the grid layout (Single / TwoByTwo / etc.) separately per group so each
    /// group restores its own layout when selected. See <see cref="AppState.GroupLayouts"/>
    /// for the backing store.
    /// </summary>
    public bool PerGroupLayout { get; set; } = true;
    /// <summary>
    /// Legacy flag — kept for back-compat with older state.json files. When deserialized
    /// as false on a state that still has GroupDisplayMode at its default, the loader
    /// migrates the mode to None. Newer code paths read GroupDisplayMode instead.
    /// </summary>
    public bool ShowGroupsTab { get; set; } = true;
    /// <summary>
    /// When 2+ adjacent visible sessions share a repo root, draw a small header above
    /// them ("📁 repoName (N)") to make the worktree grouping obvious. Off = the
    /// implicit subtitle + shared stripe color are the only signals.
    /// </summary>
    public bool ShowWorktreeClusters { get; set; } = true;
    /// <summary>
    /// Expand/collapse state of the implicit Ungrouped header in InlineHeaders mode.
    /// Real groups carry their own <see cref="SessionGroup.IsExpanded"/> bit; this holds
    /// the equivalent for the Ungrouped pseudo-section so it persists across restarts.
    /// </summary>
    public bool UngroupedSectionExpanded { get; set; } = true;
    /// <summary>
    /// One-shot guard for the legacy auto-created "Default" group migration in
    /// <see cref="Services.SessionManager.LoadFromState"/>. Without this gate the
    /// heuristic (single group, name "Default", SortOrder 0) could wipe a user-named
    /// "Default" group on a later restart. Flipped to true after the first load
    /// regardless of whether the heuristic matched.
    /// </summary>
    public bool LegacyDefaultGroupCleared { get; set; } = false;
    public bool SearchCollapseAfterNavigate { get; set; } = true;
    public string Theme { get; set; } = "dark";
    public int MaxSearchResults { get; set; } = 100;
    public bool ShowTerminalStatusDot { get; set; } = true;
    public bool ImportWindowsTerminalProfiles { get; set; } = false;
    /// <summary>How per-row action icons are surfaced in the sidebar. See <see cref="Models.SidebarActionIconsMode"/>.</summary>
    public SidebarActionIconsMode SidebarActionIconsMode { get; set; } = SidebarActionIconsMode.OnHover;

    // Storage
    public bool IndexTerminalOutput { get; set; } = true;
    public int OutputRetentionDays { get; set; } = 30;  // 0 = keep forever

    /// <summary>
    /// When on, TerminalBridge emits per-keystroke / per-output-chunk timing to
    /// crash.log (prefix [DEBUG-tt]) so intermittent freezes can be diagnosed
    /// after the fact. Off by default — has zero cost when off.
    /// </summary>
    public bool DebugTerminalTrace { get; set; } = false;

    // Terminal font settings
    public string TerminalFontFamily { get; set; } = "'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace";
    public int TerminalFontSize { get; set; } = 14;
    public bool TerminalFontLigatures { get; set; } = true;
    public string TerminalFontWeight { get; set; } = "normal";
    public double TerminalLetterSpacing { get; set; } = 0;
    public double TerminalLineHeight { get; set; } = 1.0;

    public List<string> LaunchCommands { get; set; } =
    [
        "claude",
        "claude --continue",
        "claude --model claude-opus-4-6",
        "claude --dangerously-skip-permissions",
        "codex",
        "gh copilot suggest",
        "pwsh",
        "cmd",
    ];
}

public class WindowBounds
{
    public double Left { get; set; }
    public double Top { get; set; }
    public double Width { get; set; }
    public double Height { get; set; }
}

public class AppState
{
    public List<ShellSession> Sessions { get; set; } = [];
    public List<SessionGroup> Groups { get; set; } = [];
    public string LastLayout { get; set; } = "Single";
    /// <summary>
    /// Per-group grid layouts when <see cref="AppSettings.PerGroupLayout"/> is on.
    /// Key = group Id, <c>GroupFilter.Ungrouped</c>, or <c>"__ALL__"</c> for the
    /// no-filter view. Value = <see cref="ViewModels.LayoutMode"/> name. Missing
    /// keys fall back to <see cref="LastLayout"/>.
    /// </summary>
    public Dictionary<string, string> GroupLayouts { get; set; } = new();
    public AppSettings Settings { get; set; } = new();

    // Window state persistence
    public WindowBounds? LastNormalBounds { get; set; }
    public bool WindowMaximized { get; set; } = false;
}

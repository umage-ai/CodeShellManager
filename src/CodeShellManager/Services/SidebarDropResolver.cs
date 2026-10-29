namespace CodeShellManager.Services;

/// <summary>What a rendered sidebar row is, as far as a session drop is concerned.</summary>
public enum SidebarRowKind
{
    /// <summary>A live session row, or a launching placeholder. The only valid drop anchor.</summary>
    Session,
    /// <summary>A sleeping session. Not draggable and not an anchor: dormant rows always trail
    /// the live rows of their section, so a drop among them means "after the live rows".</summary>
    Dormant,
    /// <summary>An inline group header. Starts a section; <see cref="SidebarRow.Id"/> is the
    /// group id, or null for the Ungrouped section.</summary>
    GroupHeader,
    /// <summary>A "⎇ repo (N)" worktree cluster header. Decoration, not a session.</summary>
    ClusterHeader,
}

/// <summary>
/// One rendered sidebar row, top to bottom, in the sidebar's own coordinates.
/// <see cref="Top"/>/<see cref="Height"/> describe the row's layout SLOT — margins included —
/// so adjacent slots touch, and the boundary between two rows is a single Y.
/// </summary>
public readonly record struct SidebarRow(SidebarRowKind Kind, string? Id, double Top, double Height)
{
    public double Bottom => Top + Height;
    public double Mid => Top + Height / 2;
}

/// <summary>
/// Where a dropped session goes. At most one of <see cref="BeforeSessionId"/> /
/// <see cref="AfterSessionId"/> is set; both null means "keep its position" (a drop into a
/// group section that shows no live rows). <see cref="GroupId"/> is only meaningful when
/// <see cref="AssignsGroup"/> — i.e. in inline-headers mode, where the section under the
/// cursor decides the group. <see cref="IndicatorY"/> is where to draw the insertion line,
/// and is computed from the same decision, so the line can never disagree with the drop.
/// </summary>
public readonly record struct SidebarDropTarget(
    bool AssignsGroup,
    string? GroupId,
    string? BeforeSessionId,
    string? AfterSessionId,
    double IndicatorY);

/// <summary>
/// Maps a drop Y on the sidebar to a target expressed as "before / after session X".
///
/// The previous implementation produced an INDEX, and that was the bug: the index was
/// computed in one list (all visible rows — cluster headers, dormant rows and a filtered
/// group included) and applied to two others (<c>SessionManager.Sessions</c>, which holds
/// dormant sessions and every group; and <c>_vm.Sessions</c>, which holds neither), with a
/// remove-then-insert that overshot every downward move by one. An anchor id means the
/// same thing in every list, so none of that can recur.
///
/// WPF-free so the rules are unit-testable; MainWindow only measures the rows.
/// </summary>
public static class SidebarDropResolver
{
    public static SidebarDropTarget? Resolve(IReadOnlyList<SidebarRow> rows, double y)
    {
        if (rows.Count == 0) return null;

        // Inline-headers mode renders group headers; flat modes (None, FilterStrip) don't.
        // In a flat mode the whole list is one section and the group never changes.
        int start = 0, end = rows.Count;
        bool sectioned = false;
        int headerIndex = -1;
        for (int i = 0; i < rows.Count; i++)
        {
            if (rows[i].Kind != SidebarRowKind.GroupHeader) continue;
            sectioned = true;
            if (headerIndex < 0 || rows[i].Top <= y) headerIndex = i;
            if (rows[i].Top > y) break;
        }
        if (sectioned)
        {
            start = headerIndex + 1;
            end = rows.Count;
            for (int i = start; i < rows.Count; i++)
                if (rows[i].Kind == SidebarRowKind.GroupHeader) { end = i; break; }
        }
        string? groupId = sectioned ? rows[headerIndex].Id : null;

        // First live row whose midline is below the cursor: insert before it.
        int lastSession = -1;
        for (int i = start; i < end; i++)
        {
            if (rows[i].Kind != SidebarRowKind.Session) continue;
            if (y < rows[i].Mid)
            {
                // Draw the line above any cluster header that introduces this row: once the
                // dropped session sits before the run, it renders above that header too.
                int top = i;
                while (top - 1 >= start && rows[top - 1].Kind == SidebarRowKind.ClusterHeader) top--;
                return new SidebarDropTarget(sectioned, groupId, rows[i].Id, null, rows[top].Top);
            }
            lastSession = i;
        }

        if (lastSession >= 0)
            return new SidebarDropTarget(sectioned, groupId, null, rows[lastSession].Id, rows[lastSession].Bottom);

        // A section with no live rows (empty, collapsed, or only sleeping members): the drop
        // assigns the group and leaves the position alone — with no live neighbours there is
        // nothing to be positioned relative to.
        double indicator = sectioned ? rows[headerIndex].Bottom : rows[0].Top;
        return new SidebarDropTarget(sectioned, groupId, null, null, indicator);
    }
}

using System.Collections.ObjectModel;

namespace CodeShellManager.Services;

/// <summary>
/// Order bookkeeping shared by the sidebar (which renders <c>SessionManager.Sessions</c>)
/// and the pane grid (which renders <c>MainViewModel.Sessions</c>).
///
/// The two lists hold different things — the manager's includes dormant sessions, the
/// view model's only live ones — so a position in one is not a position in the other.
/// <c>SessionManager</c> order is the single source of truth (it is what state.json
/// persists and what the sidebar shows); the grid is re-aligned to it by
/// <see cref="AlignTo{T}"/> rather than moved by the same index. Applying one list's index
/// to the other is exactly what used to make a drag land somewhere else.
///
/// WPF-free so it is unit-testable.
/// </summary>
public static class SessionOrder
{
    /// <summary>
    /// Moves <paramref name="id"/> so it sits immediately before (or after) <paramref name="anchorId"/>.
    /// The anchor's index is found AFTER removal, so a downward move doesn't overshoot by one.
    /// Returns true when the order changed. Unknown ids, and a move relative to itself, are no-ops.
    /// </summary>
    public static bool MoveRelative<T>(IList<T> list, Func<T, string> key, string id, string anchorId, bool after)
    {
        if (id == anchorId) return false;
        int from = IndexOf(list, key, id);
        if (from < 0 || IndexOf(list, key, anchorId) < 0) return false;

        var item = list[from];
        list.RemoveAt(from);
        int to = IndexOf(list, key, anchorId) + (after ? 1 : 0);
        list.Insert(to, item);
        return to != from;
    }

    /// <summary>
    /// Reorders <paramref name="list"/> in place, using <see cref="ObservableCollection{T}.Move"/>
    /// so bound views see moves rather than a reset, until its items follow the relative order
    /// of <paramref name="reference"/>. Items missing from the reference keep their relative
    /// order after the known ones. Returns true when anything moved.
    /// </summary>
    public static bool AlignTo<T>(ObservableCollection<T> list, IReadOnlyList<string> reference, Func<T, string> key)
    {
        var rank = new Dictionary<string, int>(reference.Count);
        for (int i = 0; i < reference.Count; i++) rank.TryAdd(reference[i], i);

        // Stable sort: OrderBy preserves original order between equal ranks.
        var desired = list
            .Select((item, i) => (item, r: rank.TryGetValue(key(item), out int v) ? v : int.MaxValue, i))
            .OrderBy(t => t.r).ThenBy(t => t.i)
            .Select(t => t.item)
            .ToList();

        bool moved = false;
        for (int i = 0; i < desired.Count; i++)
        {
            if (EqualityComparer<T>.Default.Equals(list[i], desired[i])) continue;
            int j = -1;
            for (int k = i + 1; k < list.Count; k++)
                if (EqualityComparer<T>.Default.Equals(list[k], desired[i])) { j = k; break; }
            if (j < 0) continue;
            list.Move(j, i);
            moved = true;
        }
        return moved;
    }

    /// <summary>
    /// Index at which to insert <paramref name="id"/> into <paramref name="list"/> so it lands in
    /// the same relative position it has in <paramref name="reference"/>: before the first item
    /// that comes after it there. Used when a live session (re)joins the grid.
    /// </summary>
    public static int InsertionIndex<T>(IReadOnlyList<T> list, IReadOnlyList<string> reference, Func<T, string> key, string id)
    {
        int myRank = -1;
        for (int i = 0; i < reference.Count; i++)
            if (reference[i] == id) { myRank = i; break; }
        if (myRank < 0) return list.Count;

        var rank = new Dictionary<string, int>(reference.Count);
        for (int i = 0; i < reference.Count; i++) rank.TryAdd(reference[i], i);
        for (int i = 0; i < list.Count; i++)
            if (rank.TryGetValue(key(list[i]), out int r) && r > myRank) return i;
        return list.Count;
    }

    /// <summary>
    /// For a NEWLY created session that has just learned its repo root: the session to place it
    /// after, so it joins its worktree siblings — or null when there is nothing to do (no live
    /// sibling in its group, or it already sits next to one).
    ///
    /// Only ever called for sessions created in this run. It never moves anything else, and is
    /// never consulted for restored sessions: the saved order is the user's, and re-sorting it
    /// on every restore is what used to undo a drag.
    /// </summary>
    /// <param name="ordered">Every session, in sidebar (SessionManager) order.</param>
    public static string? ClusterAnchorFor(IReadOnlyList<SessionPlacement> ordered, string id)
    {
        var me = ordered.FirstOrDefault(s => s.Id == id);
        if (me is null || !me.IsLive || string.IsNullOrEmpty(me.RepoRoot)) return null;

        // Only the live rows of its own group are ever displayed next to it.
        var peers = ordered.Where(s => s.IsLive && s.GroupId == me.GroupId).ToList();
        bool IsSibling(SessionPlacement s) =>
            s.Id != id && string.Equals(s.RepoRoot, me.RepoRoot, StringComparison.OrdinalIgnoreCase);

        int at = peers.FindIndex(s => s.Id == id);
        if ((at > 0 && IsSibling(peers[at - 1])) || (at + 1 < peers.Count && IsSibling(peers[at + 1])))
            return null;
        return peers.LastOrDefault(IsSibling)?.Id;
    }

    private static int IndexOf<T>(IList<T> list, Func<T, string> key, string id)
    {
        for (int i = 0; i < list.Count; i++)
            if (key(list[i]) == id) return i;
        return -1;
    }
}

/// <summary>What <see cref="SessionOrder.ClusterAnchorFor"/> needs to know about a session.</summary>
/// <param name="GroupId">Empty for ungrouped.</param>
/// <param name="RepoRoot">Known only for live sessions; null until git resolves it.</param>
public sealed record SessionPlacement(string Id, string GroupId, bool IsLive, string? RepoRoot);

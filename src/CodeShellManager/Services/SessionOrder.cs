namespace CodeShellManager.Services;

/// <summary>WPF-free ordering rules between the sidebar model and the live session list.</summary>
public static class SessionOrder
{
    /// <summary>
    /// Index at which a (re)launched session belongs in the live list
    /// (<c>MainViewModel.Sessions</c>) so it keeps its place relative to the model order
    /// (<c>SessionManager.Sessions</c>), or -1 if the session isn't in the model.
    /// </summary>
    /// <remarks>
    /// The two lists are different index spaces: the model also holds dormant sessions and
    /// sessions the restore loop hasn't launched yet. Restart and wake re-insert a VM whose
    /// sidebar row never moved, so the raw model index is wrong by one for every such session
    /// above it — the pane drifted down the grid, often to the end, while the row stayed put.
    /// The answer is the number of live sessions that precede it in the model.
    /// </remarks>
    public static int LiveInsertIndex(IReadOnlyList<string> modelOrder, IReadOnlyList<string> liveIds, string id)
    {
        var live = new HashSet<string>(liveIds);
        int count = 0;
        foreach (var modelId in modelOrder)
        {
            if (modelId == id) return count;
            if (live.Contains(modelId)) count++;
        }
        return -1;
    }
}

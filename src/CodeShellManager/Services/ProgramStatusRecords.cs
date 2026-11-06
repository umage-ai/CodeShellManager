using System;
using System.Collections.Generic;
using System.Linq;

namespace CodeShellManager.Services;

/// <summary>
/// The one state a session shows, derived from all of its OSC 7501 records.
/// <see cref="State"/> is null when there are no records (nothing to show).
/// </summary>
public sealed record ProgramStatusSummary(
    ProgramState? State,
    BlockedKind Kind = BlockedKind.None,
    int? Progress = null,
    string? Message = null,
    string? App = null)
{
    public static readonly ProgramStatusSummary Empty = new((ProgramState?)null);

    /// <summary>States that warrant the "needs attention" badge.</summary>
    public bool WantsAttention => State is ProgramState.Blocked or ProgramState.Done or ProgramState.Error;
}

/// <summary>A session just entered an attention state and the toast rate limit allows one.</summary>
public sealed record ProgramStatusNotice(string Message);

/// <summary>
/// Per-terminal store of OSC 7501 records, with the spec's replacement, <c>clear</c>,
/// eviction and lifetime rules, plus the aggregation into one <see cref="ProgramStatusSummary"/>.
/// WPF-free and single-threaded (the owner calls it on the UI thread); unit-tested in
/// <c>ProgramStatusRecordsTests</c>.
/// </summary>
public sealed class ProgramStatusRecords
{
    /// <summary>Spec cap: 256 records per terminal; least recently updated is evicted.</summary>
    public const int MaxRecords = 256;

    // Root record is keyed by "" — ids are validated non-empty, so it cannot collide.
    private const string RootKey = "";

    private sealed record Entry(ProgramStatusReport Report, long Seq);

    private readonly Dictionary<string, Entry> _records = new(StringComparer.Ordinal);
    private long _seq;

    public int Count => _records.Count;

    /// <summary>Applies one validated report. Returns true when the stored records changed.</summary>
    public bool Apply(ProgramStatusReport report)
    {
        if (report.State == ProgramState.Clear)
        {
            if (report.Id is null)
            {
                if (_records.Count == 0) return false;
                _records.Clear();
                return true;
            }
            return RemoveSubtree(report.Id);
        }

        string key = report.Id ?? RootKey;
        // Each report fully REPLACES its record — nothing carries over from the previous one.
        _records[key] = new Entry(report, ++_seq);

        if (_records.Count > MaxRecords)
        {
            string victim = _records.Where(kv => kv.Key != key)
                                    .MinBy(kv => kv.Value.Seq).Key;
            _records.Remove(victim);
        }
        return true;
    }

    /// <summary>
    /// The attached process exited, or a new shell prompt began (OSC 133 A). Per the spec,
    /// working and blocked records are dropped — there is no heartbeat, so nothing else would
    /// ever retire them. Idle records are dropped too (the spec allows it, and an idle record
    /// from a program that is gone describes nothing). Done and error persist: they are
    /// results the user has not looked at yet.
    /// </summary>
    public bool DropTransient() =>
        RemoveWhere(r => r.State is ProgramState.Working or ProgramState.Blocked or ProgramState.Idle);

    /// <summary>
    /// The user has seen the session (focused it, typed in it). Done and error mean "a result
    /// you have not seen yet", so they are retired; live states are left for the program to
    /// change. Returns true when anything was removed.
    /// </summary>
    public bool Acknowledge() =>
        RemoveWhere(r => r.State is ProgramState.Done or ProgramState.Error);

    /// <summary><c>app</c> for a record, inherited from the nearest ancestor that has one.</summary>
    public string? EffectiveApp(string? id)
    {
        string? current = id;
        while (true)
        {
            if (_records.TryGetValue(current ?? RootKey, out var e) && e.Report.App is { } app)
                return app;
            if (current is null) return null;
            int slash = current.LastIndexOf('/');
            current = slash < 0 ? null : current[..slash];
        }
    }

    /// <summary>
    /// Aggregates every record into one session state. Precedence:
    /// <b>blocked &gt; working &gt; error &gt; done &gt; idle</b>.
    /// <para>
    /// Blocked first because it is the only state that stops on the user. Working outranks
    /// error and done because those two persist until acknowledged (see the lifetime rules)
    /// while working is live — ranking a persisted result above it would let a stale child
    /// record hide work that is happening right now, forever. Ties go to the root record,
    /// then to the most recently updated one.
    /// </para>
    /// </summary>
    public ProgramStatusSummary Summarize()
    {
        if (_records.Count == 0) return ProgramStatusSummary.Empty;

        KeyValuePair<string, Entry>? best = null;
        foreach (var kv in _records)
        {
            if (best is null || Beats(kv, best.Value)) best = kv;
        }

        var (key, entry) = best!.Value;
        var r = entry.Report;
        return new ProgramStatusSummary(r.State, r.Kind, r.Progress, r.Message,
            EffectiveApp(key.Length == 0 ? null : key));
    }

    private static int Rank(ProgramState s) => s switch
    {
        ProgramState.Blocked => 4,
        ProgramState.Working => 3,
        ProgramState.Error => 2,
        ProgramState.Done => 1,
        _ => 0,
    };

    private static bool Beats(KeyValuePair<string, Entry> a, KeyValuePair<string, Entry> b)
    {
        int ra = Rank(a.Value.Report.State), rb = Rank(b.Value.Report.State);
        if (ra != rb) return ra > rb;
        bool aRoot = a.Key.Length == 0, bRoot = b.Key.Length == 0;
        if (aRoot != bRoot) return aRoot;
        return a.Value.Seq > b.Value.Seq;
    }

    private bool RemoveSubtree(string id)
    {
        string prefix = id + "/";
        var doomed = _records.Keys
            .Where(k => k == id || k.StartsWith(prefix, StringComparison.Ordinal))
            .ToList();
        foreach (var k in doomed) _records.Remove(k);
        return doomed.Count > 0;
    }

    private bool RemoveWhere(Func<ProgramStatusReport, bool> predicate)
    {
        var doomed = _records.Where(kv => predicate(kv.Value.Report)).Select(kv => kv.Key).ToList();
        foreach (var k in doomed) _records.Remove(k);
        return doomed.Count > 0;
    }
}

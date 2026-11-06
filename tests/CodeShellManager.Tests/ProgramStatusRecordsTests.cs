using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for <see cref="ProgramStatusRecords"/> — the OSC 7501 record tree: replacement,
/// <c>clear</c> semantics, eviction, lifetime and the aggregation precedence
/// (blocked &gt; working &gt; error &gt; done &gt; idle).
/// </summary>
public class ProgramStatusRecordsTests
{
    private static ProgramStatusReport R(ProgramState s, string? id = null, int? progress = null,
        BlockedKind kind = BlockedKind.None, string? app = null, string? msg = null) =>
        new(s, kind, progress, app, id, null, msg);

    [Fact]
    public void Empty_store_summarises_to_no_state()
    {
        var store = new ProgramStatusRecords();
        Assert.Null(store.Summarize().State);
        Assert.False(store.Summarize().WantsAttention);
    }

    [Fact]
    public void Each_report_fully_replaces_its_record()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Working, progress: 40, msg: "building"));
        store.Apply(R(ProgramState.Working));
        var s = store.Summarize();
        Assert.Equal(ProgramState.Working, s.State);
        Assert.Null(s.Progress);   // not carried over
        Assert.Null(s.Message);
        Assert.Equal(1, store.Count);
    }

    [Fact]
    public void Clear_without_id_removes_everything()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Working));
        store.Apply(R(ProgramState.Working, id: "a"));
        store.Apply(R(ProgramState.Done, id: "b/c"));
        Assert.True(store.Apply(R(ProgramState.Clear)));
        Assert.Equal(0, store.Count);
        Assert.False(store.Apply(R(ProgramState.Clear)));   // nothing left to change
    }

    [Fact]
    public void Clear_with_id_removes_the_record_and_its_children_only()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Idle));
        store.Apply(R(ProgramState.Working, id: "build"));
        store.Apply(R(ProgramState.Working, id: "build/test"));
        store.Apply(R(ProgramState.Working, id: "build/test/unit"));
        store.Apply(R(ProgramState.Working, id: "buildx"));   // sibling sharing a prefix
        store.Apply(R(ProgramState.Clear, id: "build"));
        Assert.Equal(2, store.Count);                         // root + buildx
        Assert.Equal(ProgramState.Working, store.Summarize().State);
    }

    [Fact]
    public void Clear_of_unknown_id_reports_no_change()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Working));
        Assert.False(store.Apply(R(ProgramState.Clear, id: "nope")));
    }

    [Fact]
    public void Store_caps_at_256_and_evicts_least_recently_updated()
    {
        var store = new ProgramStatusRecords();
        for (int i = 0; i < ProgramStatusRecords.MaxRecords; i++)
            store.Apply(R(ProgramState.Working, id: $"r{i}"));
        // Touch r0 so r1 becomes the oldest.
        store.Apply(R(ProgramState.Blocked, id: "r0"));
        store.Apply(R(ProgramState.Working, id: "new"));

        Assert.Equal(ProgramStatusRecords.MaxRecords, store.Count);
        // r1 was evicted: clearing it changes nothing. r0 survived.
        Assert.False(store.Apply(R(ProgramState.Clear, id: "r1")));
        Assert.True(store.Apply(R(ProgramState.Clear, id: "r0")));
    }

    [Theory]
    [InlineData(ProgramState.Blocked, ProgramState.Working, ProgramState.Blocked)]
    [InlineData(ProgramState.Working, ProgramState.Error, ProgramState.Working)]
    [InlineData(ProgramState.Working, ProgramState.Done, ProgramState.Working)]
    [InlineData(ProgramState.Error, ProgramState.Done, ProgramState.Error)]
    [InlineData(ProgramState.Done, ProgramState.Idle, ProgramState.Done)]
    [InlineData(ProgramState.Blocked, ProgramState.Error, ProgramState.Blocked)]
    public void Precedence_is_blocked_working_error_done_idle(ProgramState a, ProgramState b, ProgramState expected)
    {
        // Both orders of arrival, both root/child placements: precedence, not recency, decides.
        foreach (var (first, second) in new[] { (a, b), (b, a) })
        {
            var store = new ProgramStatusRecords();
            store.Apply(R(first, id: "x"));
            store.Apply(R(second, id: "y"));
            Assert.Equal(expected, store.Summarize().State);

            var rooted = new ProgramStatusRecords();
            rooted.Apply(R(first));
            rooted.Apply(R(second, id: "child"));
            Assert.Equal(expected, rooted.Summarize().State);
        }
    }

    [Fact]
    public void A_stale_done_child_never_hides_live_work()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Done, id: "old"));     // persists until acknowledged
        store.Apply(R(ProgramState.Working, progress: 10));
        Assert.Equal(ProgramState.Working, store.Summarize().State);
        Assert.Equal(10, store.Summarize().Progress);
    }

    [Fact]
    public void Ties_prefer_the_root_then_the_most_recent()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Working, progress: 10));
        store.Apply(R(ProgramState.Working, id: "a", progress: 20));
        Assert.Equal(10, store.Summarize().Progress);       // root wins the tie

        var children = new ProgramStatusRecords();
        children.Apply(R(ProgramState.Working, id: "a", progress: 20));
        children.Apply(R(ProgramState.Working, id: "b", progress: 30));
        Assert.Equal(30, children.Summarize().Progress);    // most recent child
    }

    [Fact]
    public void Summary_carries_kind_and_message_of_the_winner()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Working, msg: "root"));
        store.Apply(R(ProgramState.Blocked, id: "deploy/eu", kind: BlockedKind.Auth, msg: "login"));
        var s = store.Summarize();
        Assert.Equal(BlockedKind.Auth, s.Kind);
        Assert.Equal("login", s.Message);
        Assert.True(s.WantsAttention);
    }

    [Fact]
    public void App_is_inherited_from_the_nearest_ancestor()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Working, app: "cargo"));
        store.Apply(R(ProgramState.Working, id: "build", app: "rustc"));
        store.Apply(R(ProgramState.Working, id: "build/test/unit"));
        store.Apply(R(ProgramState.Working, id: "lint"));
        Assert.Equal("rustc", store.EffectiveApp("build/test/unit")); // skips missing build/test
        Assert.Equal("cargo", store.EffectiveApp("lint"));
        Assert.Equal("cargo", store.EffectiveApp(null));
    }

    [Fact]
    public void Process_exit_drops_live_records_and_keeps_results()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Working));
        store.Apply(R(ProgramState.Blocked, id: "b"));
        store.Apply(R(ProgramState.Idle, id: "i"));
        store.Apply(R(ProgramState.Done, id: "d"));
        store.Apply(R(ProgramState.Error, id: "e"));

        Assert.True(store.DropTransient());
        Assert.Equal(2, store.Count);
        Assert.Equal(ProgramState.Error, store.Summarize().State);
        Assert.False(store.DropTransient());   // idempotent
    }

    [Fact]
    public void EnteredAttention_is_judged_per_record()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Working, id: "bg"), out bool e1);
        Assert.False(e1);
        store.Apply(R(ProgramState.Working), out bool e2);
        Assert.False(e2);

        // Root finishes while a child keeps working: the aggregate stays "working", but the
        // root record entered done — that is the turn end the user must hear about.
        store.Apply(R(ProgramState.Done), out bool e3);
        Assert.True(e3);
        Assert.Equal(ProgramState.Working, store.Summarize().State);

        store.Apply(R(ProgramState.Done, msg: "again"), out bool e4);
        Assert.False(e4);                                   // same state restated

        store.Apply(R(ProgramState.Blocked, kind: BlockedKind.Question), out bool e5);
        Assert.True(e5);
        store.Apply(R(ProgramState.Blocked, kind: BlockedKind.Permission), out bool e6);
        Assert.True(e6);                                    // a new kind is a new ask
        store.Apply(R(ProgramState.Clear), out bool e7);
        Assert.False(e7);
    }

    [Fact]
    public void Attention_and_result_flags_track_the_records()
    {
        var store = new ProgramStatusRecords();
        Assert.False(store.HasAttentionRecord);
        store.Apply(R(ProgramState.Working));
        Assert.False(store.HasAttentionRecord);
        store.Apply(R(ProgramState.Blocked, id: "b"));
        Assert.True(store.HasAttentionRecord);
        Assert.False(store.HasUnacknowledgedResults);
        store.Apply(R(ProgramState.Error, id: "e"));
        Assert.True(store.HasUnacknowledgedResults);
        store.Acknowledge();
        Assert.False(store.HasUnacknowledgedResults);
        Assert.True(store.HasAttentionRecord);              // blocked is still live
    }

    [Fact]
    public void Acknowledge_retires_done_and_error_only()
    {
        var store = new ProgramStatusRecords();
        store.Apply(R(ProgramState.Done));
        store.Apply(R(ProgramState.Error, id: "e"));
        store.Apply(R(ProgramState.Blocked, id: "b", kind: BlockedKind.Permission));

        Assert.True(store.Acknowledge());
        Assert.Equal(1, store.Count);
        Assert.Equal(ProgramState.Blocked, store.Summarize().State);
        Assert.False(store.Acknowledge());
    }
}

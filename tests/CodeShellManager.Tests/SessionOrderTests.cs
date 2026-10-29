using System.Collections.ObjectModel;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class SessionOrderTests
{
    private static List<string> L(params string[] ids) => [.. ids];

    // ── MoveRelative ──────────────────────────────────────────────────────────

    [Fact]
    public void Moving_down_lands_exactly_before_the_anchor()
    {
        // The old remove-then-insert-at-old-index put 'a' AFTER 'c'.
        var list = L("a", "b", "c", "d");
        Assert.True(SessionOrder.MoveRelative(list, x => x, "a", "c", after: false));
        Assert.Equal(L("b", "a", "c", "d"), list);
    }

    [Fact]
    public void Moving_up_lands_exactly_before_the_anchor()
    {
        var list = L("a", "b", "c", "d");
        Assert.True(SessionOrder.MoveRelative(list, x => x, "d", "b", after: false));
        Assert.Equal(L("a", "d", "b", "c"), list);
    }

    [Fact]
    public void After_places_directly_behind_the_anchor()
    {
        var list = L("a", "b", "c", "d");
        Assert.True(SessionOrder.MoveRelative(list, x => x, "a", "c", after: true));
        Assert.Equal(L("b", "c", "a", "d"), list);
    }

    [Theory]
    [InlineData("b", "b", false)]   // relative to itself
    [InlineData("b", "c", false)]   // already directly before c
    [InlineData("b", "a", true)]    // already directly after a
    [InlineData("zz", "a", false)]  // unknown session
    [InlineData("a", "zz", false)]  // unknown anchor
    public void No_op_moves_report_false_and_change_nothing(string id, string anchor, bool after)
    {
        var list = L("a", "b", "c");
        Assert.False(SessionOrder.MoveRelative(list, x => x, id, anchor, after));
        Assert.Equal(L("a", "b", "c"), list);
    }

    // ── AlignTo ───────────────────────────────────────────────────────────────

    [Fact]
    public void AlignTo_follows_the_reference_ignoring_ids_the_list_does_not_hold()
    {
        // Reference = SessionManager (with dormant 'zz'); list = the grid (live only).
        var grid = new ObservableCollection<string>(["c1", "p1", "c2"]);
        Assert.True(SessionOrder.AlignTo(grid, L("zz", "c1", "c2", "p1"), x => x));
        Assert.Equal(["c1", "c2", "p1"], grid);
    }

    [Fact]
    public void AlignTo_raises_only_moves_never_a_reset()
    {
        var grid = new ObservableCollection<string>(["b", "a", "c"]);
        var actions = new List<System.Collections.Specialized.NotifyCollectionChangedAction>();
        grid.CollectionChanged += (_, e) => actions.Add(e.Action);

        SessionOrder.AlignTo(grid, L("a", "b", "c"), x => x);

        Assert.NotEmpty(actions);
        Assert.All(actions, a => Assert.Equal(System.Collections.Specialized.NotifyCollectionChangedAction.Move, a));
    }

    [Fact]
    public void AlignTo_keeps_unknown_items_after_known_ones_in_their_own_order()
    {
        var grid = new ObservableCollection<string>(["x", "b", "y", "a"]);
        SessionOrder.AlignTo(grid, L("a", "b"), x => x);
        Assert.Equal(["a", "b", "x", "y"], grid);
    }

    [Fact]
    public void AlignTo_on_an_aligned_list_reports_false()
    {
        var grid = new ObservableCollection<string>(["a", "b"]);
        Assert.False(SessionOrder.AlignTo(grid, L("zz", "a", "b"), x => x));
    }

    // ── InsertionIndex ────────────────────────────────────────────────────────

    [Fact]
    public void A_woken_session_rejoins_the_grid_at_its_relative_position_not_its_manager_index()
    {
        // Manager: [zz1, zz2, a, w, b]  ->  'w' is at manager index 3, but in the grid
        // [a, b] it belongs at 1. The old code inserted at 3, i.e. appended.
        var grid = L("a", "b");
        Assert.Equal(1, SessionOrder.InsertionIndex(grid, L("zz1", "zz2", "a", "w", "b"), x => x, "w"));
    }

    [Fact]
    public void InsertionIndex_appends_when_nothing_follows_or_the_id_is_unknown()
    {
        var grid = L("a", "b");
        Assert.Equal(2, SessionOrder.InsertionIndex(grid, L("a", "b", "w"), x => x, "w"));
        Assert.Equal(2, SessionOrder.InsertionIndex(grid, L("a", "b"), x => x, "nope"));
    }

    // ── ClusterAnchorFor ──────────────────────────────────────────────────────

    private static SessionPlacement P(string id, string repo, string group = "", bool live = true) =>
        new(id, group, live, repo == "" ? null : repo);

    [Fact]
    public void A_new_ps_session_is_placed_after_its_claude_sibling()
    {
        var order = new[] { P("api", @"C:\src\api"), P("web", @"C:\src\web"), P("api (PS)", @"C:\src\api") };
        Assert.Equal("api", SessionOrder.ClusterAnchorFor(order, "api (PS)"));
    }

    [Fact]
    public void Repo_roots_compare_case_insensitively()
    {
        var order = new[] { P("api", @"C:\src\API"), P("web", ""), P("api (PS)", @"c:\src\api") };
        Assert.Equal("api", SessionOrder.ClusterAnchorFor(order, "api (PS)"));
    }

    [Fact]
    public void Already_next_to_a_sibling_needs_no_move()
    {
        var order = new[] { P("api", "R"), P("api (PS)", "R"), P("web", "W") };
        Assert.Null(SessionOrder.ClusterAnchorFor(order, "api (PS)"));
    }

    [Fact]
    public void A_dormant_session_between_siblings_does_not_count_as_separating_them()
    {
        // Dormant rows render after the live ones, so this is visually adjacent already.
        var order = new[] { P("api", "R"), P("zz", "", live: false), P("api (PS)", "R") };
        Assert.Null(SessionOrder.ClusterAnchorFor(order, "api (PS)"));
    }

    [Fact]
    public void A_sibling_in_another_group_is_not_an_anchor()
    {
        var order = new[] { P("api", "R", group: "claude"), P("web", "W", group: "ps"), P("api (PS)", "R", group: "ps") };
        Assert.Null(SessionOrder.ClusterAnchorFor(order, "api (PS)"));
    }

    [Fact]
    public void No_repo_root_yet_or_no_siblings_means_no_anchor()
    {
        Assert.Null(SessionOrder.ClusterAnchorFor([P("api", "R"), P("new", "")], "new"));
        Assert.Null(SessionOrder.ClusterAnchorFor([P("api", "R"), P("solo", "S")], "solo"));
    }

    [Fact]
    public void With_several_siblings_it_goes_after_the_last_one()
    {
        var order = new[] { P("a1", "R"), P("a2", "R"), P("web", "W"), P("a3", "R") };
        Assert.Equal("a2", SessionOrder.ClusterAnchorFor(order, "a3"));
    }
}

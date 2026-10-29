using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class SidebarDropResolverTests
{
    private const double H = 40;

    // Rows stacked top to bottom with no gaps, as layout slots.
    private static List<SidebarRow> Stack(params (SidebarRowKind kind, string? id, double height)[] rows)
    {
        var list = new List<SidebarRow>();
        double y = 0;
        foreach (var (kind, id, height) in rows)
        {
            list.Add(new SidebarRow(kind, id, y, height));
            y += height;
        }
        return list;
    }

    private static (SidebarRowKind, string?, double) Live(string id) => (SidebarRowKind.Session, id, H);
    private static (SidebarRowKind, string?, double) Asleep(string id) => (SidebarRowKind.Dormant, id, H);
    private static (SidebarRowKind, string?, double) Header(string? group) => (SidebarRowKind.GroupHeader, group, 26);
    private static (SidebarRowKind, string?, double) Cluster() => (SidebarRowKind.ClusterHeader, null, 22);

    [Fact]
    public void Empty_sidebar_has_no_target()
    {
        Assert.Null(SidebarDropResolver.Resolve([], 10));
    }

    [Fact]
    public void Top_half_of_a_row_means_before_it()
    {
        var rows = Stack(Live("a"), Live("b"), Live("c"));
        var t = SidebarDropResolver.Resolve(rows, rows[1].Top + 5)!.Value;
        Assert.Equal("b", t.BeforeSessionId);
        Assert.Null(t.AfterSessionId);
        Assert.Equal(rows[1].Top, t.IndicatorY);
    }

    [Fact]
    public void Bottom_half_of_a_row_means_before_the_next_one()
    {
        var rows = Stack(Live("a"), Live("b"), Live("c"));
        var t = SidebarDropResolver.Resolve(rows, rows[1].Bottom - 5)!.Value;
        Assert.Equal("c", t.BeforeSessionId);
    }

    [Fact]
    public void Bottom_half_of_the_last_row_means_after_it()
    {
        var rows = Stack(Live("a"), Live("b"));
        var t = SidebarDropResolver.Resolve(rows, rows[1].Bottom - 5)!.Value;
        Assert.Null(t.BeforeSessionId);
        Assert.Equal("b", t.AfterSessionId);
        Assert.Equal(rows[1].Bottom, t.IndicatorY);
    }

    [Fact]
    public void Flat_mode_never_assigns_a_group()
    {
        var rows = Stack(Live("a"), Live("b"));
        Assert.False(SidebarDropResolver.Resolve(rows, 5)!.Value.AssignsGroup);
    }

    [Fact]
    public void A_cluster_header_is_not_counted_as_a_row()
    {
        // ⎇ header above api: aiming at api's top half means "before api", and the line is
        // drawn above the header, because that is where the dropped row will render.
        var rows = Stack(Live("web"), Cluster(), Live("api"), Live("api (PS)"), Live("docs"));
        var t = SidebarDropResolver.Resolve(rows, rows[2].Top + 5)!.Value;
        Assert.Equal("api", t.BeforeSessionId);
        Assert.Equal(rows[1].Top, t.IndicatorY);
    }

    [Fact]
    public void Dormant_rows_are_not_anchors_a_drop_among_them_lands_after_the_live_rows()
    {
        var rows = Stack(Live("a"), Live("b"), Asleep("zz1"), Asleep("zz2"));
        var t = SidebarDropResolver.Resolve(rows, rows[2].Top + 5)!.Value;
        Assert.Null(t.BeforeSessionId);
        Assert.Equal("b", t.AfterSessionId);
    }

    [Fact]
    public void Inline_the_section_under_the_cursor_decides_the_group()
    {
        var rows = Stack(Header(null), Live("u1"), Header("claude"), Live("c1"), Live("c2"), Header("ps"), Live("p1"));
        var t = SidebarDropResolver.Resolve(rows, rows[4].Top + 5)!.Value;
        Assert.True(t.AssignsGroup);
        Assert.Equal("claude", t.GroupId);
        Assert.Equal("c2", t.BeforeSessionId);
    }

    [Fact]
    public void Inline_the_ungrouped_section_is_group_null()
    {
        var rows = Stack(Header(null), Live("u1"), Header("claude"), Live("c1"));
        var t = SidebarDropResolver.Resolve(rows, rows[1].Bottom - 5)!.Value;
        Assert.True(t.AssignsGroup);
        Assert.Null(t.GroupId);
        Assert.Equal("u1", t.AfterSessionId);
    }

    [Fact]
    public void Inline_below_the_last_live_row_of_a_section_stays_in_that_section()
    {
        // Must not slide into the next group's first slot.
        var rows = Stack(Header("claude"), Live("c1"), Live("c2"), Header("ps"), Live("p1"));
        var t = SidebarDropResolver.Resolve(rows, rows[2].Bottom - 5)!.Value;
        Assert.Equal("claude", t.GroupId);
        Assert.Equal("c2", t.AfterSessionId);
    }

    [Fact]
    public void Inline_a_section_with_no_live_rows_assigns_the_group_and_keeps_the_position()
    {
        var rows = Stack(Header("claude"), Live("c1"), Header("ps"), Asleep("zz"), Header("misc"));
        var t = SidebarDropResolver.Resolve(rows, rows[3].Top + 5)!.Value;
        Assert.Equal("ps", t.GroupId);
        Assert.Null(t.BeforeSessionId);
        Assert.Null(t.AfterSessionId);
        Assert.Equal(rows[2].Bottom, t.IndicatorY);
    }

    [Fact]
    public void Inline_above_the_first_header_belongs_to_the_first_section()
    {
        var rows = Stack(Header("claude"), Live("c1"));
        var t = SidebarDropResolver.Resolve(rows, -3)!.Value;
        Assert.Equal("claude", t.GroupId);
        Assert.Equal("c1", t.BeforeSessionId);
    }
}

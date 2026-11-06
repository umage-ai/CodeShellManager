using System.Collections.ObjectModel;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// End-to-end drag scenarios: a real <see cref="SessionManager"/>, rows rendered the way
/// <c>MainWindow.RebuildSidebarOrder</c> renders them, the drop resolved by
/// <see cref="SidebarDropResolver"/>, and the grid re-aligned with
/// <see cref="SessionOrder.AlignTo{T}"/> — the same path <c>ApplySessionDrop</c> takes.
///
/// Scenarios 1-5 each reproduced a misplaced drop against the previous index-based code
/// (simulated faithfully before the fix). The cursor is always aimed at the TOP QUARTER of
/// the row the user means to drop above — no edge-of-midline cases.
/// </summary>
public class SidebarReorderScenarioTests
{
    private const double RowH = 40, ClusterH = 22, HeaderH = 26;

    private sealed class World
    {
        public SessionManager Manager { get; } = new();
        public ObservableCollection<string> Grid { get; } = [];          // _vm.Sessions ids
        public Dictionary<string, string> Repo { get; } = [];             // SessionViewModel.RepoRoot
        public string Mode = "None";                                      // None | FilterStrip | Inline
        public string? ActiveGroup;
        public List<string> Groups = [];

        public World Add(string id, string group = "", bool dormant = false, string? repo = null)
        {
            var s = Manager.CreateSession(id, @"C:\src\" + id, "claude", "", group);
            s.Id = id;
            s.IsDormant = dormant;
            if (!dormant) Grid.Add(id);
            if (repo != null) Repo[id] = repo;
            return this;
        }

        private ShellSession S(string id) => Manager.Sessions.First(s => s.Id == id);

        // Mirrors RebuildSidebarOrder (incl. dormant rows obeying the group filter, #142).
        public List<SidebarRow> Render()
        {
            var rows = new List<(SidebarRowKind kind, string? id)>();

            void Live(IEnumerable<ShellSession> live)
            {
                var l = live.ToList();
                for (int i = 0; i < l.Count;)
                {
                    int j = i;
                    Repo.TryGetValue(l[i].Id, out var root);
                    while (root != null && j + 1 < l.Count && Repo.GetValueOrDefault(l[j + 1].Id) == root) j++;
                    if (j > i) rows.Add((SidebarRowKind.ClusterHeader, null));
                    for (int k = i; k <= j; k++) rows.Add((SidebarRowKind.Session, l[k].Id));
                    i = j + 1;
                }
            }

            if (Mode == "Inline" && Groups.Count > 0)
            {
                foreach (var g in new[] { "" }.Concat(Groups))
                {
                    var members = Manager.Sessions.Where(s => s.GroupId == g).ToList();
                    if (g == "" && members.Count == 0) continue;
                    rows.Add((SidebarRowKind.GroupHeader, g == "" ? null : g));
                    Live(members.Where(s => !s.IsDormant));
                    rows.AddRange(members.Where(s => s.IsDormant).Select(s => (SidebarRowKind.Dormant, (string?)s.Id)));
                }
            }
            else
            {
                bool Visible(ShellSession s) => Mode != "FilterStrip" || ActiveGroup == null || s.GroupId == ActiveGroup;
                Live(Manager.Sessions.Where(s => !s.IsDormant && Visible(s)));
                rows.AddRange(Manager.Sessions.Where(s => s.IsDormant && Visible(s))
                    .Select(s => (SidebarRowKind.Dormant, (string?)s.Id)));
            }

            var result = new List<SidebarRow>();
            double y = 0;
            foreach (var (kind, id) in rows)
            {
                double h = kind switch { SidebarRowKind.ClusterHeader => ClusterH, SidebarRowKind.GroupHeader => HeaderH, _ => RowH };
                result.Add(new SidebarRow(kind, id, y, h));
                y += h;
            }
            return result;
        }

        public double AboveRow(string id)
        {
            var r = Render().First(r => r.Kind == SidebarRowKind.Session && r.Id == id);
            return r.Top + r.Height * 0.25;
        }

        public double BelowEverything()
        {
            var last = Render().Last(r => r.Kind == SidebarRowKind.Session);
            return last.Bottom - 2;
        }

        // Mirrors MainWindow.ApplySessionDrop.
        public void Drop(string dragged, double y)
        {
            var t = SidebarDropResolver.Resolve(Render(), y)!.Value;
            if (t.BeforeSessionId is { } b) Manager.MoveSessionRelative(dragged, b, after: false);
            else if (t.AfterSessionId is { } a) Manager.MoveSessionRelative(dragged, a, after: true);
            if (t.AssignsGroup && S(dragged).GroupId != (t.GroupId ?? ""))
                Manager.SetSessionGroup(dragged, t.GroupId);
            SessionOrder.AlignTo(Grid, Manager.Sessions.Select(s => s.Id).ToList(), x => x);
        }

        public List<string> Sidebar() =>
            Render().Where(r => r.Kind is SidebarRowKind.Session or SidebarRowKind.Dormant).Select(r => r.Id!).ToList();

        public List<string> LiveIn(string group) =>
            Manager.Sessions.Where(s => s.GroupId == group && !s.IsDormant).Select(s => s.Id).ToList();
    }

    [Fact]
    public void Scenario1_moving_down_one_slot_does_not_overshoot()
    {
        var w = new World().Add("a").Add("b").Add("c");
        w.Drop("a", w.AboveRow("c"));
        Assert.Equal(["b", "a", "c"], w.Sidebar());          // was [b, c, a]
    }

    [Fact]
    public void Scenario2_a_worktree_cluster_header_does_not_shift_the_drop()
    {
        var w = new World().Add("web").Add("api", repo: "R").Add("api (PS)", repo: "R").Add("docs");
        w.Drop("docs", w.AboveRow("api"));
        Assert.Equal(["web", "docs", "api", "api (PS)"], w.Sidebar());   // was [web, api, docs, api (PS)]
    }

    [Fact]
    public void Scenario3_filter_strip_drop_lands_where_aimed_within_the_visible_group()
    {
        var w = new World { Mode = "FilterStrip", ActiveGroup = "claude", Groups = ["claude", "ps"] }
            .Add("p1", "ps").Add("c1", "claude").Add("p2", "ps").Add("c2", "claude").Add("c3", "claude");
        w.Drop("c3", w.AboveRow("c2"));
        Assert.Equal(["c1", "c3", "c2"], w.LiveIn("claude"));  // was [c3, c1, c2]
        Assert.Equal(["p1", "p2"], w.LiveIn("ps"));             // the hidden group is untouched
    }

    [Fact]
    public void Scenario4_inline_cross_group_drop_with_a_sleeping_session_lands_where_aimed()
    {
        var w = new World { Mode = "Inline", Groups = ["claude", "ps"] }
            .Add("zz", "claude", dormant: true).Add("c1", "claude").Add("c2", "claude").Add("p1", "ps");
        w.Drop("p1", w.AboveRow("c2"));
        Assert.Equal(["c1", "p1", "c2"], w.LiveIn("claude"));  // was [p1, c1, c2]
        Assert.Empty(w.LiveIn("ps"));
    }

    [Fact]
    public void Scenario5_the_pane_grid_follows_the_sidebar_after_a_drop()
    {
        var w = new World { Mode = "Inline", Groups = ["claude", "ps"] }
            .Add("zz", "claude", dormant: true).Add("c1", "claude").Add("c2", "claude").Add("p1", "ps");
        w.Drop("p1", w.AboveRow("c2"));
        var liveSidebarOrder = w.Manager.Sessions.Where(s => !s.IsDormant).Select(s => s.Id).ToList();
        Assert.Equal(liveSidebarOrder, w.Grid);                 // grid was [c1, p1, c2] vs sidebar [p1, c1, c2]
    }

    [Fact]
    public void Scenario6_a_ps_session_dragged_away_from_its_claude_session_stays_put()
    {
        // The drop itself was always right here; what undid it was the auto-cluster pass
        // re-sorting every sibling on the next Add/Remove. That pass now only ever places a
        // session created in this run — the dragged session is removed from that set by
        // ApplySessionDrop, and an unrelated new session has nothing to be placed next to.
        var w = new World().Add("api", repo: "R").Add("api (PS)", repo: "R").Add("web");
        w.Drop("api (PS)", w.BelowEverything());
        Assert.Equal(["api", "web", "api (PS)"], w.Sidebar());

        w.Add("new", repo: "N");
        var placements = w.Manager.Sessions
            .Select(s => new SessionPlacement(s.Id, s.GroupId, !s.IsDormant, w.Repo.GetValueOrDefault(s.Id)))
            .ToList();
        Assert.Null(SessionOrder.ClusterAnchorFor(placements, "new"));
        Assert.Equal(["api", "web", "api (PS)", "new"], w.Sidebar());
    }

    [Fact]
    public void Scenario6b_dropping_within_the_cluster_keeps_both_siblings_together()
    {
        var w = new World().Add("web").Add("api", repo: "R").Add("api (PS)", repo: "R");
        w.Drop("api (PS)", w.AboveRow("api"));
        Assert.Equal(["web", "api (PS)", "api"], w.Sidebar());
        Assert.Contains(w.Render(), r => r.Kind == SidebarRowKind.ClusterHeader);
    }
}

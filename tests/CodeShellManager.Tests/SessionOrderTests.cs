using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Where a (re)launched session's VM goes in <c>MainViewModel.Sessions</c> (live only),
/// given its position in <c>SessionManager.Sessions</c> (live + dormant, all groups).
/// Restart and wake both re-insert a VM whose sidebar row never moved; using the raw
/// model index against the live list pushed the pane one slot further down for every
/// dormant or not-yet-relaunched session above it.
/// </summary>
public class SessionOrderTests
{
    [Fact]
    public void Restart_with_a_dormant_session_above_lands_in_its_sidebar_position()
    {
        // Sidebar: A, D(dormant), B, C. Restarting B: the live list is [A, C] meanwhile.
        string[] model = ["A", "D", "B", "C"];
        string[] live = ["A", "C"];

        Assert.Equal(1, SessionOrder.LiveInsertIndex(model, live, "B"));
    }

    [Fact]
    public void Restart_of_the_first_session_goes_first()
    {
        string[] model = ["D", "A", "B"];
        string[] live = ["B"];

        Assert.Equal(0, SessionOrder.LiveInsertIndex(model, live, "A"));
    }

    [Fact]
    public void Restart_of_the_last_session_goes_last()
    {
        string[] model = ["A", "D1", "B", "D2", "C"];
        string[] live = ["A", "B"];

        Assert.Equal(2, SessionOrder.LiveInsertIndex(model, live, "C"));
    }

    [Fact]
    public void Several_dormant_sessions_above_do_not_accumulate_an_offset()
    {
        string[] model = ["D1", "D2", "D3", "A", "B", "C"];
        string[] live = ["A", "C"];

        Assert.Equal(1, SessionOrder.LiveInsertIndex(model, live, "B"));
    }

    [Fact]
    public void Sessions_still_launching_above_are_skipped()
    {
        // Restore loop: A and B are in the model but not yet live when C registers.
        string[] model = ["A", "B", "C", "E"];
        string[] live = ["E"];

        Assert.Equal(0, SessionOrder.LiveInsertIndex(model, live, "C"));
    }

    [Fact]
    public void With_nothing_dormant_it_matches_the_model_index()
    {
        string[] model = ["A", "B", "C"];
        string[] live = ["A", "C"];

        Assert.Equal(1, SessionOrder.LiveInsertIndex(model, live, "B"));
    }

    [Fact]
    public void A_session_missing_from_the_model_appends()
    {
        string[] model = ["A", "B"];
        string[] live = ["A", "B"];

        Assert.Equal(-1, SessionOrder.LiveInsertIndex(model, live, "X"));
    }
}

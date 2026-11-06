using System;
using System.Linq;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

public class RecentSessionsTests
{
    private static readonly DateTime Now = new(2026, 11, 6, 12, 0, 0, DateTimeKind.Utc);

    private static ShellSession S(string id, double minutesAgo, bool dormant = false)
        => new() { Id = id, Name = id, LastActivityAt = Now.AddMinutes(-minutesAgo), IsDormant = dormant };

    [Fact]
    public void NewestFirst_ActiveExcluded()
    {
        var list = new[] { S("a", 30), S("b", 1), S("c", 10), S("d", 0) };
        var ids = RecentSessions.Pick(list, activeId: "d").Select(s => s.Id);
        Assert.Equal(new[] { "b", "c", "a" }, ids);
    }

    [Fact]
    public void SleepingSessions_AreIncluded()
    {
        var list = new[] { S("live", 20), S("asleep", 5, dormant: true) };
        Assert.Equal("asleep", RecentSessions.Pick(list, null)[0].Id);
    }

    [Fact]
    public void CappedAtMax()
    {
        var list = Enumerable.Range(0, 25).Select(i => S($"s{i}", i)).ToArray();
        var picked = RecentSessions.Pick(list, null);
        Assert.Equal(RecentSessions.MaxItems, picked.Count);
        Assert.Equal("s0", picked[0].Id);
    }

    [Fact]
    public void Ties_KeepSidebarOrder()
    {
        var list = new[] { S("x", 60), S("y", 60), S("z", 60) };
        Assert.Equal(new[] { "x", "y", "z" }, RecentSessions.Pick(list, null).Select(s => s.Id));
    }

    [Fact]
    public void NoActive_AllCandidates()
        => Assert.Equal(2, RecentSessions.Pick(new[] { S("a", 1), S("b", 2) }, null).Count);

    [Theory]
    [InlineData(0.2, "now")]
    [InlineData(5, "5m")]
    [InlineData(59, "59m")]
    [InlineData(60, "1h")]
    [InlineData(60 * 23, "23h")]
    [InlineData(60 * 24 * 3, "3d")]
    [InlineData(60 * 24 * 30, "4w")]
    public void Ago_Compact(double minutesAgo, string expected)
        => Assert.Equal(expected, RecentSessions.Ago(Now.AddMinutes(-minutesAgo), Now));

    [Fact]
    public void Ago_Never_IsBlank() => Assert.Equal("", RecentSessions.Ago(default, Now));
}

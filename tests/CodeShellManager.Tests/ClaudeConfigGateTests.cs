using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for the adaptive Claude launch gate (issue #82). Clock and file-time reads
/// are injected, so these drive the state machine with a fake clock — no real files,
/// no real waiting.
/// </summary>
public class ClaudeConfigGateTests
{
    // ── config file resolution ──────────────────────────────────────────────

    [Fact]
    public void ResolveConfigFile_NoEnvVar_IsSiblingOfClaudeDir()
    {
        // Default layout puts .claude.json NEXT TO ~/.claude, not inside it.
        Assert.Equal(@"C:\Users\bob\.claude.json",
            ClaudeConfigGate.ResolveConfigFile(null, @"C:\Users\bob"));
    }

    [Fact]
    public void ResolveConfigFile_WithEnvVar_IsInsideThatDir()
    {
        Assert.Equal(@"C:\Users\bob\.claude-work\.claude.json",
            ClaudeConfigGate.ResolveConfigFile(@"C:\Users\bob\.claude-work", @"C:\Users\bob"));
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    public void ResolveConfigFile_BlankEnvVar_FallsBackToProfile(string configDir)
    {
        Assert.Equal(@"C:\Users\bob\.claude.json",
            ClaudeConfigGate.ResolveConfigFile(configDir, @"C:\Users\bob"));
    }

    // ── the wait state machine ──────────────────────────────────────────────

    /// <summary>Fake clock: every awaited delay advances virtual time instantly.</summary>
    private sealed class Clock
    {
        public DateTime Now = new(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc);
        public readonly List<int> Waits = new();
        public Task Delay(int ms) { Waits.Add(ms); Now = Now.AddMilliseconds(ms); return Task.CompletedTask; }
        public int TotalWaitedMs { get { int t = 0; foreach (var w in Waits) t += w; return t; } }
    }

    [Fact]
    public async Task Wait_WriteThenQuiet_ReturnsEarlyNotAtTheCap()
    {
        var clock = new Clock();
        var baseline = new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc);
        DateTime? current = baseline;
        int polls = 0;

        await ClaudeConfigGate.WaitForQuiesceAsync(
            baseline,
            () => { if (++polls == 2) current = baseline.AddSeconds(1); return current; },
            () => clock.Now,
            clock.Delay,
            cap: TimeSpan.FromMilliseconds(2000),
            quietFor: TimeSpan.FromMilliseconds(250),
            pollMs: 50);

        // Write seen on poll 2 (~100ms), then 250ms of quiet -> well under the 2000ms cap.
        Assert.True(clock.TotalWaitedMs < 2000,
            $"expected an early return, waited {clock.TotalWaitedMs}ms");
        Assert.True(clock.TotalWaitedMs >= 250,
            $"must observe the full quiet period, waited only {clock.TotalWaitedMs}ms");
    }

    [Fact]
    public async Task Wait_NoWriteEverObserved_WaitsTheFullCap()
    {
        // Deliberate: a machine where the file can't be watched must be no worse off
        // than the old fixed delay, never faster-and-racy.
        var clock = new Clock();
        var baseline = new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc);

        await ClaudeConfigGate.WaitForQuiesceAsync(
            baseline,
            () => baseline,
            () => clock.Now,
            clock.Delay,
            cap: TimeSpan.FromMilliseconds(2000),
            quietFor: TimeSpan.FromMilliseconds(250),
            pollMs: 50);

        Assert.True(clock.TotalWaitedMs >= 2000,
            $"expected the full cap, waited {clock.TotalWaitedMs}ms");
    }

    [Fact]
    public async Task Wait_FileKeepsChanging_StillStopsAtTheCap()
    {
        // A pathological writer must not extend shutdown/startup indefinitely.
        var clock = new Clock();
        var baseline = new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc);
        int n = 0;

        await ClaudeConfigGate.WaitForQuiesceAsync(
            baseline,
            () => baseline.AddMilliseconds(++n * 10),   // never settles
            () => clock.Now,
            clock.Delay,
            cap: TimeSpan.FromMilliseconds(1000),
            quietFor: TimeSpan.FromMilliseconds(250),
            pollMs: 50);

        Assert.InRange(clock.TotalWaitedMs, 1000, 1100);
    }

    [Fact]
    public async Task Wait_FileAppearsFromNothing_CountsAsAWrite()
    {
        // First-ever run: no config file at baseline, Claude creates one.
        var clock = new Clock();
        DateTime? current = null;
        int polls = 0;

        await ClaudeConfigGate.WaitForQuiesceAsync(
            baseline: null,
            () => { if (++polls == 2) current = clock.Now; return current; },
            () => clock.Now,
            clock.Delay,
            cap: TimeSpan.FromMilliseconds(2000),
            quietFor: TimeSpan.FromMilliseconds(250),
            pollMs: 50);

        Assert.True(clock.TotalWaitedMs < 2000,
            $"creation should count as a write, waited {clock.TotalWaitedMs}ms");
    }

    [Fact]
    public async Task Wait_NeverSleepsPastTheDeadline()
    {
        // Each individual sleep is clamped to the remaining budget, so the wait can't
        // overshoot by a whole poll interval at the tail either.
        var clock = new Clock();
        var baseline = new DateTime(2026, 1, 1, 0, 0, 0, DateTimeKind.Utc);

        await ClaudeConfigGate.WaitForQuiesceAsync(
            baseline,
            () => baseline,
            () => clock.Now,
            clock.Delay,
            cap: TimeSpan.FromMilliseconds(175),   // deliberately not a multiple of pollMs
            quietFor: TimeSpan.FromMilliseconds(250),
            pollMs: 50);

        Assert.True(clock.TotalWaitedMs <= 175,
            $"slept {clock.TotalWaitedMs}ms against a 175ms cap");
    }

    [Fact]
    public async Task Wait_ZeroCap_ReturnsImmediately()
    {
        // Mirrors the existing `staggerMs > 0` opt-out.
        var clock = new Clock();

        await ClaudeConfigGate.WaitForQuiesceAsync(
            null, () => null, () => clock.Now, clock.Delay,
            cap: TimeSpan.Zero, quietFor: TimeSpan.FromMilliseconds(250));

        Assert.Empty(clock.Waits);
    }
}

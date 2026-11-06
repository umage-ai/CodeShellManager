using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Restoring the main window's bounds under per-monitor DPI v2 (#181). All coordinates are
/// physical pixels. The standard rig: a 1920x1080 primary at 100% with a 48px taskbar at the
/// bottom, and a 3840x2160 secondary at 150% to its right.
/// </summary>
public class WindowPlacementMathTests
{
    private static readonly MonitorArea Primary = new(
        new PixelRect(0, 0, 1920, 1080), new PixelRect(0, 0, 1920, 1032), 96, IsPrimary: true);

    private static readonly MonitorArea Secondary = new(
        new PixelRect(1920, 0, 3840, 2160), new PixelRect(1920, 0, 3840, 2088), 144, IsPrimary: false);

    private static readonly MonitorArea[] Both = { Primary, Secondary };

    private static WindowPlacementState P(int l, int t, int w, int h, int dpi) =>
        new() { Left = l, Top = t, Width = w, Height = h, Dpi = dpi };

    [Fact]
    public void Nothing_saved_resolves_to_null()
    {
        Assert.Null(WindowPlacementMath.Resolve(null, null, Both));
        Assert.Null(WindowPlacementMath.Resolve(P(0, 0, 0, 0, 96), null, Both));
        Assert.Null(WindowPlacementMath.Resolve(null, new WindowBounds { Width = double.NaN, Height = 10 }, Both));
    }

    [Fact]
    public void No_monitors_resolves_to_null()
    {
        Assert.Null(WindowPlacementMath.Resolve(P(10, 10, 800, 600, 96), null, System.Array.Empty<MonitorArea>()));
    }

    [Fact]
    public void Placement_on_the_secondary_is_restored_exactly()
    {
        // The drift case: same pixels back, every run, on a monitor whose scale differs from
        // the primary's.
        var r = WindowPlacementMath.Resolve(P(2200, 300, 2400, 1500, 144), null, Both)!.Value;
        Assert.Equal(new PixelRect(2200, 300, 2400, 1500), r.Rect);
        Assert.Same(Secondary, r.Monitor);
    }

    // A rig where the workspace offsets differ per monitor: primary with its taskbar docked
    // at the TOP (workspace (0,0) = screen (0,48)), 150% secondary with a left taskbar.
    private static readonly MonitorArea TopBarPrimary = new(
        new PixelRect(0, 0, 1920, 1080), new PixelRect(0, 48, 1920, 1032), 96, IsPrimary: true);

    private static MonitorArea LeftBarSecondary(int dpi) => new(
        new PixelRect(1920, 0, 3840, 2160), new PixelRect(1992, 0, 3768, 2160), dpi, IsPrimary: false);

    /// <summary>
    /// One app run: restore what was saved, then save what is on screen the way
    /// MainWindow.CaptureWindowState does — GetWindowRect for a normal window, the workspace
    /// rcNormalPosition (as SetWindowPlacement was given it) for a maximized one.
    /// </summary>
    private static WindowPlacementState Run(WindowPlacementState saved, MonitorArea[] monitors, bool maximized)
    {
        var (rect, monitor) = WindowPlacementMath.Resolve(saved, null, monitors)!.Value;
        return maximized
            ? WindowPlacementMath.SaveFromWorkspace(WindowPlacementMath.ScreenToWorkspace(rect, monitor), monitors)!
            : WindowPlacementMath.SaveFromScreen(rect, monitors)!;
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void Round_trip_is_stable_across_runs_with_per_monitor_offsets(bool maximized)
    {
        var monitors = new[] { TopBarPrimary, LeftBarSecondary(144) };
        foreach (var start in new[] { P(2500, 400, 1800, 1200, 144), P(100, 200, 1000, 700, 96) })
        {
            var p = start;
            for (int run = 0; run < 10; run++) p = Run(p, monitors, maximized);
            Assert.Equal((start.Left, start.Top, start.Width, start.Height, start.Dpi),
                         (p.Left, p.Top, p.Width, p.Height, p.Dpi));
        }
    }

    [Fact]
    public void Monitor_scale_change_rescales_once_then_stays_put()
    {
        var p = Run(P(2500, 400, 1800, 1200, 144), new[] { TopBarPrimary, LeftBarSecondary(144) }, false);
        // The secondary goes 150% -> 200%: same apparent size, applied once.
        var after = new[] { TopBarPrimary, LeftBarSecondary(192) };
        p = Run(p, after, false);
        Assert.Equal((2500, 400, 2400, 1600, 192), (p.Left, p.Top, p.Width, p.Height, p.Dpi));
        for (int run = 0; run < 5; run++) p = Run(p, after, false);
        Assert.Equal((2500, 400, 2400, 1600, 192), (p.Left, p.Top, p.Width, p.Height, p.Dpi));
    }

    [Fact]
    public void Saved_normal_rect_takes_offset_and_DPI_from_the_monitor_it_is_on()
    {
        // A maximized window on the PRIMARY whose normal (pre-maximize / pre-snap) rect is on
        // the secondary: taking offset and DPI from the window's current monitor would give
        // top 348 at 96 dpi — the wrong place, and a Rescale that shrinks it next run.
        var monitors = new[] { TopBarPrimary, LeftBarSecondary(144) };
        var s = WindowPlacementMath.SaveFromWorkspace(new PixelRect(2128, 300, 1500, 900), monitors)!;
        Assert.Equal((2200, 300, 1500, 900, 144), (s.Left, s.Top, s.Width, s.Height, s.Dpi));

        // A rect on the primary picks up the primary's top-taskbar offset instead.
        s = WindowPlacementMath.SaveFromWorkspace(new PixelRect(100, 100, 800, 600), monitors)!;
        Assert.Equal((100, 148, 96), (s.Left, s.Top, s.Dpi));
    }

    [Fact]
    public void Saved_screen_rect_takes_DPI_from_the_monitor_holding_most_of_it()
    {
        var s = WindowPlacementMath.SaveFromScreen(new PixelRect(1500, 100, 1200, 800), Both)!;
        Assert.Equal(144, s.Dpi);
        s = WindowPlacementMath.SaveFromScreen(new PixelRect(1000, 100, 1200, 800), Both)!;
        Assert.Equal(96, s.Dpi);
        // Off every monitor: the nearest one's.
        s = WindowPlacementMath.SaveFromScreen(new PixelRect(9000, 100, 800, 600), Both)!;
        Assert.Equal(144, s.Dpi);
    }

    [Fact]
    public void A_window_spanning_both_monitors_is_left_alone()
    {
        // Mostly on the secondary, so that is the DPI it was saved at.
        var r = WindowPlacementMath.Resolve(P(1500, 100, 1200, 800, 144), null, Both)!.Value;
        Assert.Equal(new PixelRect(1500, 100, 1200, 800), r.Rect);
        Assert.Same(Secondary, r.Monitor);
    }

    [Fact]
    public void Monitor_scale_change_keeps_the_apparent_size()
    {
        // Saved at 100% on the secondary, which is now at 150%: 1.5x the pixels, same corner.
        var r = WindowPlacementMath.Resolve(P(2000, 100, 1000, 800, 96), null, Both)!.Value;
        Assert.Equal(new PixelRect(2000, 100, 1500, 1200), r.Rect);
    }

    [Fact]
    public void Unplugged_monitor_falls_back_to_a_visible_position()
    {
        // Saved on the secondary; now only the primary exists.
        var r = WindowPlacementMath.Resolve(P(2200, 300, 1500, 1000, 144), null, new[] { Primary })!.Value;
        Assert.Same(Primary, r.Monitor);
        // 1500x1000 @144 → 1000x667 @96, pushed to the nearest (right) edge of the work area.
        Assert.Equal(new PixelRect(920, 300, 1000, 667), r.Rect);
        Assert.True(WindowPlacementMath.IsCaptionVisible(r.Rect, 96, new[] { Primary }));
    }

    [Fact]
    public void Monitor_left_of_primary_gone_lands_on_the_left_edge()
    {
        var r = WindowPlacementMath.Resolve(P(-1700, 200, 1200, 800, 96), null, new[] { Primary })!.Value;
        Assert.Equal(new PixelRect(0, 200, 1200, 800), r.Rect);
    }

    [Fact]
    public void Caption_above_every_work_area_is_pulled_down()
    {
        // Body on screen, title bar not: unreachable with the mouse.
        var r = WindowPlacementMath.Resolve(P(100, -200, 1000, 700, 96), null, new[] { Primary })!.Value;
        Assert.Equal(new PixelRect(100, 0, 1000, 700), r.Rect);
    }

    [Fact]
    public void Caption_under_the_taskbar_is_pulled_up()
    {
        // Top edge inside the taskbar band (1032..1080): on the monitor, not on its work area.
        var r = WindowPlacementMath.Resolve(P(100, 1040, 800, 600, 96), null, new[] { Primary })!.Value;
        Assert.Equal(new PixelRect(100, 432, 800, 600), r.Rect);
    }

    [Fact]
    public void Slightly_off_edge_window_is_kept_where_it_is()
    {
        var r = WindowPlacementMath.Resolve(P(-8, -4, 1000, 700, 96), null, new[] { Primary })!.Value;
        Assert.Equal(new PixelRect(-8, -4, 1000, 700), r.Rect);
    }

    [Fact]
    public void Off_screen_window_larger_than_the_work_area_is_shrunk_to_fit()
    {
        var r = WindowPlacementMath.Resolve(P(5000, 5000, 3000, 2000, 96), null, new[] { Primary })!.Value;
        Assert.Equal(Primary.Work, r.Rect);
    }

    [Fact]
    public void Minimum_size_is_enforced_in_physical_pixels()
    {
        var r = WindowPlacementMath.Resolve(P(2000, 100, 300, 200, 144), null, Both, minWidthDip: 800, minHeightDip: 500)!.Value;
        Assert.Equal(new PixelRect(2000, 100, 1200, 750), r.Rect);
    }

    [Fact]
    public void Legacy_DIP_bounds_are_scaled_by_the_primary_DPI()
    {
        // Saved by a system-DPI build at 150% primary: DIPs × 1.5 = pixels.
        var primary150 = Primary with { Dpi = 144 };
        var r = WindowPlacementMath.Resolve(null,
            new WindowBounds { Left = 100, Top = 50, Width = 1000, Height = 600 }, new[] { primary150 })!.Value;
        Assert.Equal(new PixelRect(150, 75, 1500, 900), r.Rect);
    }

    [Fact]
    public void Legacy_v0_10_bounds_on_a_150_percent_secondary_land_back_on_it()
    {
        // v0.10.0 (per-monitor v2) saved DIPs = pixels × 96 / THAT monitor's DPI. A window at
        // pixels (2200, 300, 2400x1500) on the 150% secondary was saved as (1466.67, 200,
        // 1600x1000). Read at the primary's 100% it would land mostly on the primary at
        // two-thirds the size.
        var r = WindowPlacementMath.Resolve(null,
            new WindowBounds { Left = 2200 / 1.5, Top = 200, Width = 1600, Height = 1000 }, Both)!.Value;
        Assert.Equal(new PixelRect(2200, 300, 2400, 1500), r.Rect);
        Assert.Same(Secondary, r.Monitor);
    }

    [Fact]
    public void Legacy_bounds_on_the_primary_stay_on_the_primary()
    {
        var r = WindowPlacementMath.Resolve(null,
            new WindowBounds { Left = 100, Top = 100, Width = 1000, Height = 700 }, Both)!.Value;
        Assert.Equal(new PixelRect(100, 100, 1000, 700), r.Rect);
        Assert.Same(Primary, r.Monitor);
    }

    [Fact]
    public void Legacy_bounds_ambiguous_between_monitors_prefer_the_primary()
    {
        // DIP (1300, 100, 400x300) is consistent both as 100% pixels on the primary and as
        // 150% pixels (1950, 150, ...) on the secondary. Nothing in the file says which.
        var r = WindowPlacementMath.Resolve(null,
            new WindowBounds { Left = 1300, Top = 100, Width = 400, Height = 300 }, Both)!.Value;
        Assert.Same(Primary, r.Monitor);
        Assert.Equal(new PixelRect(1300, 100, 400, 300), r.Rect);
    }

    [Fact]
    public void Legacy_bounds_far_off_screen_still_produce_a_visible_window()
    {
        var r = WindowPlacementMath.Resolve(null,
            new WindowBounds { Left = 9000, Top = 9000, Width = 1280, Height = 800 }, new[] { Primary })!.Value;
        Assert.True(WindowPlacementMath.IsCaptionVisible(r.Rect, 96, new[] { Primary }));
        Assert.Equal(new PixelRect(640, 232, 1280, 800), r.Rect);
    }

    [Fact]
    public void Placement_wins_over_legacy_bounds()
    {
        var r = WindowPlacementMath.Resolve(P(2200, 300, 2400, 1500, 144),
            new WindowBounds { Left = 0, Top = 0, Width = 500, Height = 500 }, Both)!.Value;
        Assert.Equal(new PixelRect(2200, 300, 2400, 1500), r.Rect);
    }

    [Fact]
    public void Workspace_conversion_round_trips_and_follows_a_top_taskbar()
    {
        // Taskbar docked at the top: workspace (0,0) is screen (0,48).
        var topBar = new MonitorArea(new PixelRect(0, 0, 1920, 1080), new PixelRect(0, 48, 1920, 1032), 96, true);
        var screen = new PixelRect(100, 148, 800, 600);
        var ws = WindowPlacementMath.ScreenToWorkspace(screen, topBar);
        Assert.Equal(new PixelRect(100, 100, 800, 600), ws);
        Assert.Equal(screen, WindowPlacementMath.WorkspaceToScreen(ws, topBar));
    }

    [Theory]
    [InlineData(96, 144, 1000, 1500)]
    [InlineData(144, 96, 1500, 1000)]
    [InlineData(120, 120, 1000, 1000)]
    public void Rescale_scales_size_only(int from, int to, int w, int expectedW)
    {
        var r = WindowPlacementMath.Rescale(new PixelRect(7, 9, w, w), from, to);
        Assert.Equal(new PixelRect(7, 9, expectedW, expectedW), r);
    }
}

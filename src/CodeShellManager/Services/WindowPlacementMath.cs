using System;
using System.Collections.Generic;
using System.Linq;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

/// <summary>A rectangle in physical screen pixels.</summary>
public readonly record struct PixelRect(int Left, int Top, int Width, int Height)
{
    public int Right => Left + Width;
    public int Bottom => Top + Height;

    public PixelRect Intersect(PixelRect o)
    {
        int l = Math.Max(Left, o.Left), t = Math.Max(Top, o.Top);
        int r = Math.Min(Right, o.Right), b = Math.Min(Bottom, o.Bottom);
        return r > l && b > t ? new PixelRect(l, t, r - l, b - t) : default;
    }

    public long Area => (long)Width * Height;
}

/// <summary>One display: its full bounds and work area (physical pixels) and its DPI.</summary>
public sealed record MonitorArea(PixelRect Bounds, PixelRect Work, int Dpi, bool IsPrimary);

/// <summary>
/// WPF-free decisions behind restoring the main window's bounds (#181): which monitor the
/// saved rectangle belongs on, whether it is still reachable, and what to do when it isn't.
/// Everything is in physical pixels — the only coordinate space that means the same thing on
/// every monitor of a per-monitor-DPI-v2 process. <c>MainWindow</c> feeds it the live monitor
/// list and applies the answer with <c>SetWindowPlacement</c> before the window is shown.
/// </summary>
public static class WindowPlacementMath
{
    /// <summary>Height of the strip at the top of the window that must stay reachable (DIPs).</summary>
    public const int CaptionDip = 32;

    /// <summary>How much of that strip must be on a work area for the window to count as visible (DIPs).</summary>
    public const int MinVisibleWidthDip = 120;

    /// <summary>
    /// The normal rectangle to restore, and the monitor it lands on. Null when there is
    /// nothing usable saved (first run, or garbage) — the caller then keeps its defaults.
    /// </summary>
    /// <param name="placement">Physical-pixel bounds saved by this build. Preferred.</param>
    /// <param name="legacy">
    /// DIP bounds saved by an older build, in an unknown scale — see <see cref="FromLegacyDips"/>.
    /// A wrong guess only misplaces the window by the scale ratio, and the visibility rules
    /// below still guarantee it ends up reachable.
    /// </param>
    public static (PixelRect Rect, MonitorArea Monitor)? Resolve(
        WindowPlacementState? placement, WindowBounds? legacy,
        IReadOnlyList<MonitorArea> monitors, double minWidthDip = 0, double minHeightDip = 0)
    {
        if (monitors.Count == 0) return null;
        var primary = monitors.FirstOrDefault(m => m.IsPrimary) ?? monitors[0];

        PixelRect rect;
        int savedDpi;
        if (placement is { Width: > 0, Height: > 0, Dpi: > 0 })
        {
            rect = new PixelRect(placement.Left, placement.Top, placement.Width, placement.Height);
            savedDpi = placement.Dpi;
        }
        else if (legacy is not null && IsFinite(legacy.Left) && IsFinite(legacy.Top)
                 && IsFinite(legacy.Width) && IsFinite(legacy.Height)
                 && legacy.Width > 0 && legacy.Height > 0)
        {
            (rect, savedDpi) = FromLegacyDips(legacy, monitors, primary);
        }
        else return null;

        var home = Home(rect, monitors);

        var target = home ?? Nearest(rect, monitors) ?? primary;

        // Same apparent size if that monitor's scale changed since the save. The top-left
        // corner stays put: it is what the user placed.
        rect = Rescale(rect, savedDpi, target.Dpi);

        // Reachable: leave it exactly where the user put it — even hanging off an edge or
        // spanning two monitors. Restoring is not the place to second-guess a layout.
        if (home is not null && IsCaptionVisible(rect, target.Dpi, monitors))
            return (EnsureMinSize(rect, target, minWidthDip, minHeightDip), target);

        // Caption out of reach (monitor unplugged, rearranged, resolution dropped): fit it into
        // the nearest monitor's work area, moving it the shortest distance — to the nearest
        // edge rather than the centre, so it stays roughly where it was.
        rect = EnsureMinSize(rect, target, minWidthDip, minHeightDip);
        return (ClampInto(rect, target.Work), target);
    }

    /// <summary>
    /// Legacy DIP bounds → physical pixels. The scale they were saved at is unknown:
    /// v0.10.0 (per-monitor v2, the usual upgrade source) saved pixels ÷ the DPI of the
    /// monitor the window was on; older system-DPI builds saved pixels ÷ the primary's DPI at
    /// the time. So each monitor's DPI is tried as the scale, and a candidate is consistent
    /// when the scaled rect's own monitor (<see cref="Home"/>) is the monitor whose DPI was
    /// used — v0.10.0's rule, and also the system-DPI rule whenever the window was on the
    /// primary. Several can be consistent (a small window near a shared edge); the primary
    /// wins that tie, then the largest share of the rect on its monitor. If none is, the
    /// primary's scale is used and the visibility rules in <see cref="Resolve"/> take over.
    /// </summary>
    public static (PixelRect Rect, int Dpi) FromLegacyDips(
        WindowBounds legacy, IReadOnlyList<MonitorArea> monitors, MonitorArea primary)
    {
        PixelRect Scaled(int dpi)
        {
            double s = dpi / 96.0;
            return new PixelRect(Round(legacy.Left * s), Round(legacy.Top * s),
                                 Round(legacy.Width * s), Round(legacy.Height * s));
        }

        var best = monitors
            .Select(m => (m, r: Scaled(m.Dpi)))
            .Where(x => ReferenceEquals(Home(x.r, monitors), x.m))
            .OrderByDescending(x => x.m.IsPrimary)
            .ThenByDescending(x => (double)x.r.Intersect(x.m.Bounds).Area / Math.Max(1, x.r.Area))
            .Select(x => ((PixelRect, int)?)(x.r, x.m.Dpi))
            .FirstOrDefault();
        return best ?? (Scaled(primary.Dpi), primary.Dpi);
    }

    /// <summary>
    /// The monitor a screen rect is (mostly) on — largest overlap, as Windows picks it — or
    /// null when it overlaps none.
    /// </summary>
    public static MonitorArea? Home(PixelRect rect, IReadOnlyList<MonitorArea> monitors) =>
        monitors
            .Select(m => (m, area: rect.Intersect(m.Bounds).Area))
            .Where(x => x.area > 0)
            .OrderByDescending(x => x.area)
            .Select(x => x.m)
            .FirstOrDefault();

    /// <summary>
    /// What to save for a normal-state window: its screen rect (GetWindowRect) and the DPI of
    /// the monitor that RECT is on — not of whatever monitor the window reports, which is the
    /// question that goes wrong for a snapped or spanning window.
    /// </summary>
    public static WindowPlacementState? SaveFromScreen(PixelRect screen, IReadOnlyList<MonitorArea> monitors)
    {
        if (screen.Width <= 0 || screen.Height <= 0 || monitors.Count == 0) return null;
        var m = Home(screen, monitors) ?? Nearest(screen, monitors)!;
        return ToState(screen, m);
    }

    /// <summary>
    /// What to save for a maximized/minimized window: its normal rect from GetWindowPlacement
    /// (workspace coordinates). Each monitor's work-area offset is tried, and the one under
    /// which the rect lands most on that same monitor is taken — the rect's own monitor, which
    /// need not be the one the (maximized) window is on now. That monitor supplies the DPI.
    /// </summary>
    public static WindowPlacementState? SaveFromWorkspace(PixelRect workspace, IReadOnlyList<MonitorArea> monitors)
    {
        if (workspace.Width <= 0 || workspace.Height <= 0 || monitors.Count == 0) return null;
        var hit = monitors
            .Select(m => (m, s: WorkspaceToScreen(workspace, m)))
            .Select(x => (x.m, x.s, area: x.s.Intersect(x.m.Bounds).Area))
            .Where(x => x.area > 0)
            .OrderByDescending(x => x.area)
            .FirstOrDefault();
        if (hit.m is not null) return ToState(hit.s, hit.m);
        var m = Nearest(workspace, monitors)!;
        return ToState(WorkspaceToScreen(workspace, m), m);
    }

    private static WindowPlacementState ToState(PixelRect r, MonitorArea m) =>
        new() { Left = r.Left, Top = r.Top, Width = r.Width, Height = r.Height, Dpi = m.Dpi };

    /// <summary>
    /// True when the top strip of the window (where the caption is) overlaps some monitor's
    /// work area by enough to grab it with the mouse.
    /// </summary>
    public static bool IsCaptionVisible(PixelRect rect, int dpi, IReadOnlyList<MonitorArea> monitors)
    {
        int strip = Math.Min(rect.Height, Scale(CaptionDip, dpi));
        var caption = new PixelRect(rect.Left, rect.Top, rect.Width, strip);
        int needW = Math.Min(rect.Width, Scale(MinVisibleWidthDip, dpi));
        int needH = Math.Max(1, strip / 2);
        return monitors.Any(m =>
        {
            var i = caption.Intersect(m.Work);
            return i.Width >= needW && i.Height >= needH;
        });
    }

    /// <summary>Scales the size (not the position) from one DPI to another.</summary>
    public static PixelRect Rescale(PixelRect r, int fromDpi, int toDpi)
    {
        if (fromDpi <= 0 || toDpi <= 0 || fromDpi == toDpi) return r;
        double f = (double)toDpi / fromDpi;
        return r with { Width = Round(r.Width * f), Height = Round(r.Height * f) };
    }

    /// <summary>Moves <paramref name="r"/> the shortest distance that puts it inside <paramref name="area"/>.</summary>
    public static PixelRect ClampInto(PixelRect r, PixelRect area)
    {
        int w = Math.Min(r.Width, area.Width), h = Math.Min(r.Height, area.Height);
        int l = Math.Clamp(r.Left, area.Left, area.Right - w);
        int t = Math.Clamp(r.Top, area.Top, area.Bottom - h);
        return new PixelRect(l, t, w, h);
    }

    /// <summary>
    /// Physical (screen) ↔ workspace coordinates. <c>WINDOWPLACEMENT.rcNormalPosition</c> is
    /// in workspace coordinates — relative to the work area, so a taskbar docked top or left
    /// shifts them. Save and restore both convert through the window's monitor, so a round
    /// trip is exact whichever way Windows defines the offset on a secondary monitor.
    /// </summary>
    public static PixelRect ScreenToWorkspace(PixelRect r, MonitorArea m) =>
        r with { Left = r.Left - (m.Work.Left - m.Bounds.Left), Top = r.Top - (m.Work.Top - m.Bounds.Top) };

    /// <inheritdoc cref="ScreenToWorkspace"/>
    public static PixelRect WorkspaceToScreen(PixelRect r, MonitorArea m) =>
        r with { Left = r.Left + (m.Work.Left - m.Bounds.Left), Top = r.Top + (m.Work.Top - m.Bounds.Top) };

    /// <summary>Grows a too-small rect to the window's minimum size (capped at the work area).</summary>
    private static PixelRect EnsureMinSize(PixelRect r, MonitorArea m, double minWidthDip, double minHeightDip)
    {
        int minW = Math.Min(m.Work.Width, (int)Math.Ceiling(minWidthDip * m.Dpi / 96.0));
        int minH = Math.Min(m.Work.Height, (int)Math.Ceiling(minHeightDip * m.Dpi / 96.0));
        return r with { Width = Math.Max(r.Width, minW), Height = Math.Max(r.Height, minH) };
    }

    private static MonitorArea? Nearest(PixelRect r, IReadOnlyList<MonitorArea> monitors)
    {
        long cx = r.Left + r.Width / 2, cy = r.Top + r.Height / 2;
        return monitors
            .OrderBy(m =>
            {
                long dx = cx < m.Bounds.Left ? m.Bounds.Left - cx : cx > m.Bounds.Right ? cx - m.Bounds.Right : 0;
                long dy = cy < m.Bounds.Top ? m.Bounds.Top - cy : cy > m.Bounds.Bottom ? cy - m.Bounds.Bottom : 0;
                return dx * dx + dy * dy;
            })
            .FirstOrDefault();
    }

    private static int Scale(int dip, int dpi) => Round(dip * Math.Max(dpi, 96) / 96.0);
    private static int Round(double v) => (int)Math.Round(v, MidpointRounding.AwayFromZero);
    private static bool IsFinite(double v) => !double.IsNaN(v) && !double.IsInfinity(v);
}

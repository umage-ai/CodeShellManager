using System;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Animation;
using CodeShellManager.ViewModels;
using Color = System.Windows.Media.Color;

namespace CodeShellManager;

/// <summary>
/// The "working" indicator driven by OSC 7501 program status — a small pulsing
/// <c>⟳ working · 40%</c> in the sidebar row and on the terminal toolbar. It lives in that
/// chrome on purpose: WebView2 is an <c>HwndHost</c>, so nothing WPF draws can appear over a
/// terminal pane (see "Session Spinners" in CLAUDE.md).
/// </summary>
public partial class MainWindow
{
    // Catppuccin accent blue — the same colour as the restore rail ("something is in progress").
    private static readonly SolidColorBrush s_workingBrush = Freeze(new SolidColorBrush(Color.FromRgb(0x89, 0xb4, 0xfa)));

    // One frozen animation shared by every indicator. Capped at 10 fps: a slow opacity pulse
    // gains nothing from 60, and a fleet of working sessions must not cost a frame each.
    private static readonly DoubleAnimation s_workingPulse = CreateWorkingPulse();

    private static DoubleAnimation CreateWorkingPulse()
    {
        var anim = new DoubleAnimation
        {
            From = 1.0,
            To = 0.45,
            Duration = TimeSpan.FromMilliseconds(1100),
            AutoReverse = true,
            RepeatBehavior = RepeatBehavior.Forever
        };
        Timeline.SetDesiredFrameRate(anim, 10);
        anim.Freeze();
        return anim;
    }

    private static SolidColorBrush Freeze(SolidColorBrush b) { b.Freeze(); return b; }

    /// <summary>Builds a collapsed indicator; drive it with <see cref="UpdateWorkingIndicator"/>.</summary>
    private static TextBlock BuildWorkingIndicator(Thickness margin) => new()
    {
        Foreground = s_workingBrush,
        FontSize = 10,
        Margin = margin,
        VerticalAlignment = VerticalAlignment.Center,
        TextTrimming = TextTrimming.CharacterEllipsis,
        Visibility = Visibility.Collapsed
    };

    /// <summary>
    /// Shows/hides and relabels a working indicator from the VM. The pulse runs only while
    /// visible — a collapsed element with a running clock still ticks.
    /// </summary>
    private static void UpdateWorkingIndicator(TextBlock tb, SessionViewModel vm, bool compact)
    {
        if (!vm.IsWorking)
        {
            if (tb.Visibility != Visibility.Collapsed)
            {
                tb.BeginAnimation(UIElement.OpacityProperty, null);
                tb.Visibility = Visibility.Collapsed;
            }
            return;
        }

        string pct = vm.WorkProgress is int p ? $"{p}%" : "";
        tb.Text = compact
            ? (pct.Length > 0 ? $"⟳ {pct}" : "⟳ working")
            : (pct.Length > 0 ? $"⟳ working · {pct}" : "⟳ working");
        // msg is decoded, control-character-free and format-character-stripped by
        // ProgramStatusPayload; a ToolTip renders it as plain text.
        tb.ToolTip = string.IsNullOrEmpty(vm.ProgramStatusMessage)
            ? "Working (reported by the program)"
            : vm.ProgramStatusMessage;

        if (tb.Visibility != Visibility.Visible)
        {
            tb.Visibility = Visibility.Visible;
            tb.BeginAnimation(UIElement.OpacityProperty, s_workingPulse);
        }
    }
}

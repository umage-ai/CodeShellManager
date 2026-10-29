using System.Windows;
using System.Windows.Documents;
using System.Windows.Media;
using Brush = System.Windows.Media.Brush;
using Color = System.Windows.Media.Color;
using Pen = System.Windows.Media.Pen;
using Point = System.Windows.Point;

namespace CodeShellManager.Views;

/// <summary>
/// The "it will land here" line shown while dragging a session over the sidebar.
///
/// Drawn as an adorner, not as an element inserted into the list: inserting a row would
/// shift every row below it mid-drag, and the drop is resolved against row positions.
/// The sidebar is plain WPF (no <c>HwndHost</c>), so unlike a terminal pane an adorner
/// can draw over it.
/// </summary>
internal sealed class InsertionLineAdorner : Adorner
{
    private static readonly Brush Accent = Freeze(new SolidColorBrush(Color.FromRgb(0x89, 0xb4, 0xfa)));
    private static readonly Pen Line = Freeze(new Pen(Accent, 2));
    private double _y = double.NaN;

    public InsertionLineAdorner(UIElement adorned) : base(adorned)
    {
        IsHitTestVisible = false;   // must never intercept the drag it is describing
    }

    public void ShowAt(double y)
    {
        if (Visibility == Visibility.Visible && _y.Equals(y)) return;
        _y = y;
        Visibility = Visibility.Visible;
        InvalidateVisual();
    }

    public void Hide()
    {
        if (Visibility != Visibility.Collapsed) Visibility = Visibility.Collapsed;
    }

    protected override void OnRender(DrawingContext dc)
    {
        if (double.IsNaN(_y)) return;
        double width = AdornedElement.RenderSize.Width;
        double y = Math.Round(_y);
        dc.DrawEllipse(Accent, null, new Point(5, y), 3.5, 3.5);
        dc.DrawLine(Line, new Point(5, y), new Point(Math.Max(5, width - 2), y));
    }

    private static T Freeze<T>(T f) where T : Freezable { f.Freeze(); return f; }
}

using System.Collections.Generic;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using Color = System.Windows.Media.Color;
using ColorConverter = System.Windows.Media.ColorConverter;
using FontFamily = System.Windows.Media.FontFamily;

namespace CodeShellManager.Views;

/// <summary>
/// "N sessions are still running — close anyway?" Shown by <c>MainWindow.OnClosing</c> when
/// any live session is busy (<see cref="Services.SessionBusyRules"/>). Defaults to keeping
/// the app open: Enter and Esc both cancel, closing takes a deliberate click.
/// </summary>
public partial class BusySessionsDialog : Window
{
    public sealed record Row(string Name, string? AccentHex, string Reason);

    /// <summary>True when the user ticked "Don't warn me again" and then chose to close.</summary>
    public bool DontAskAgain { get; private set; }

    public BusySessionsDialog(IReadOnlyList<Row> rows)
    {
        InitializeComponent();
        HeadlineText.Text = rows.Count == 1
            ? "1 session is still running"
            : $"{rows.Count} sessions are still running";
        foreach (var r in rows) RowsPanel.Children.Add(BuildRow(r));
        Loaded += (_, _) => CancelBtn.Focus();
    }

    /// <summary>Modal helper. True = close the app.</summary>
    public static bool Confirm(Window owner, IReadOnlyList<Row> rows, out bool dontAskAgain)
    {
        var dlg = new BusySessionsDialog(rows) { Owner = owner };
        bool close = dlg.ShowDialog() == true;
        dontAskAgain = close && dlg.DontAskAgain;
        return close;
    }

    private static UIElement BuildRow(Row r)
    {
        var grid = new Grid { Margin = new Thickness(0, 2, 0, 2) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(3) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        Color accent;
        try { accent = (Color)ColorConverter.ConvertFromString(r.AccentHex ?? "#6c7086"); }
        catch { accent = Color.FromRgb(0x6c, 0x70, 0x86); }

        var stripe = new Border
        {
            Background = new SolidColorBrush(accent),
            CornerRadius = new CornerRadius(2),
            Height = 14
        };
        Grid.SetColumn(stripe, 0);

        var name = new TextBlock
        {
            Text = r.Name,
            Foreground = new SolidColorBrush(Color.FromRgb(0xcd, 0xd6, 0xf4)),
            FontSize = 12,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(8, 0, 10, 0)
        };
        Grid.SetColumn(name, 1);

        var reason = new TextBlock
        {
            Text = "⟳ " + r.Reason,
            Foreground = new SolidColorBrush(Color.FromRgb(0xfa, 0xb3, 0x87)),
            FontFamily = new FontFamily("Consolas"),
            FontSize = 10.5,
            VerticalAlignment = VerticalAlignment.Center
        };
        Grid.SetColumn(reason, 2);

        grid.Children.Add(stripe);
        grid.Children.Add(name);
        grid.Children.Add(reason);
        return grid;
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }

    private void CloseAnyway_Click(object sender, RoutedEventArgs e)
    {
        DontAskAgain = DontAskCheck.IsChecked == true;
        DialogResult = true;
        Close();
    }
}

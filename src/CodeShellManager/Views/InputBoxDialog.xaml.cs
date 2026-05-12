using System.Windows;

namespace CodeShellManager.Views;

public partial class InputBoxDialog : Window
{
    public string Value => ValueBox.Text;

    public InputBoxDialog(string title, string prompt, string initial)
    {
        InitializeComponent();
        Title = title;
        PromptText.Text = prompt;
        ValueBox.Text = initial;
        Loaded += (_, _) => { ValueBox.Focus(); ValueBox.SelectAll(); };
    }

    /// <summary>Modal helper. Returns the entered text trimmed, or null if the user cancelled / left it empty.</summary>
    public static string? Prompt(Window owner, string title, string prompt, string initial)
    {
        var dlg = new InputBoxDialog(title, prompt, initial) { Owner = owner };
        if (dlg.ShowDialog() != true) return null;
        var trimmed = dlg.Value.Trim();
        return string.IsNullOrEmpty(trimmed) ? null : trimmed;
    }

    private void Ok_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = true;
        Close();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }
}

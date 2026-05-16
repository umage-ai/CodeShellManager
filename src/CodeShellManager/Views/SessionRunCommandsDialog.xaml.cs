using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Windows;
using CodeShellManager.Models;

namespace CodeShellManager.Views;

public partial class SessionRunCommandsDialog : Window
{
    private readonly ObservableCollection<RunCommandRow> _rows = new();

    /// <summary>The new list to write back to ShellSession.RunCommands. Populated on Save.</summary>
    public List<RunCommandItem>? Result { get; private set; }

    public SessionRunCommandsDialog(string sessionName, IReadOnlyList<RunCommandItem> initial)
    {
        InitializeComponent();
        TitleText.Text = $"Run commands for \"{sessionName}\"";
        foreach (var item in initial)
        {
            _rows.Add(new RunCommandRow
            {
                Id = item.Id,
                Label = item.Label,
                CommandLine = item.CommandLine,
                IsDefault = item.IsDefault,
                ModeIndex = (int)item.Mode,
                PostRunUrl = item.PostRunUrl ?? "",
            });
        }
        RowsList.ItemsSource = _rows;
    }

    private void Add_Click(object sender, RoutedEventArgs e)
    {
        _rows.Add(new RunCommandRow
        {
            Id = Guid.NewGuid().ToString(),
            Label = "",
            CommandLine = "",
            IsDefault = _rows.Count == 0,
            ModeIndex = 0,
            PostRunUrl = "",
        });
    }

    private void Delete_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.Tag is RunCommandRow row)
            _rows.Remove(row);
    }

    private void MoveUp_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.Tag is RunCommandRow row)
        {
            int i = _rows.IndexOf(row);
            if (i > 0) _rows.Move(i, i - 1);
        }
    }

    private void MoveDown_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.Tag is RunCommandRow row)
        {
            int i = _rows.IndexOf(row);
            if (i >= 0 && i < _rows.Count - 1) _rows.Move(i, i + 1);
        }
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        // Validate: every row needs label + commandline.
        foreach (var r in _rows)
        {
            if (string.IsNullOrWhiteSpace(r.Label) || string.IsNullOrWhiteSpace(r.CommandLine))
            {
                System.Windows.MessageBox.Show("Every row needs both a label and a command line.",
                    "Validation", System.Windows.MessageBoxButton.OK, System.Windows.MessageBoxImage.Warning);
                return;
            }
        }

        var list = _rows.Select(r => new RunCommandItem
        {
            Id = r.Id,
            Label = r.Label.Trim(),
            CommandLine = r.CommandLine.Trim(),
            IsDefault = r.IsDefault,
            Mode = (RunMode)r.ModeIndex,
            PostRunUrl = string.IsNullOrWhiteSpace(r.PostRunUrl) ? null : r.PostRunUrl.Trim(),
        }).ToList();
        RunCommandItem.EnsureSingleDefault(list);

        Result = list;
        DialogResult = true;
        Close();
    }

    /// <summary>
    /// Backing class for the bound grid. Has to implement INotifyPropertyChanged so
    /// the RadioButton's TwoWay binding can clear sibling rows on selection.
    /// </summary>
    private class RunCommandRow : System.ComponentModel.INotifyPropertyChanged
    {
        public string Id { get; set; } = "";
        private string _label = "";
        private string _commandLine = "";
        private bool _isDefault;
        private int _modeIndex;
        private string _postRunUrl = "";
        public string Label
        {
            get => _label;
            set { _label = value; OnChanged(nameof(Label)); }
        }
        public string CommandLine
        {
            get => _commandLine;
            set { _commandLine = value; OnChanged(nameof(CommandLine)); }
        }
        public bool IsDefault
        {
            get => _isDefault;
            set { _isDefault = value; OnChanged(nameof(IsDefault)); }
        }
        public int ModeIndex
        {
            get => _modeIndex;
            set { _modeIndex = value; OnChanged(nameof(ModeIndex)); }
        }
        public string PostRunUrl
        {
            get => _postRunUrl;
            set { _postRunUrl = value; OnChanged(nameof(PostRunUrl)); }
        }

        public event System.ComponentModel.PropertyChangedEventHandler? PropertyChanged;
        private void OnChanged(string n) => PropertyChanged?.Invoke(this,
            new System.ComponentModel.PropertyChangedEventArgs(n));
    }
}

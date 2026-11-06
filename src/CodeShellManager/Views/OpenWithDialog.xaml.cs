using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Brush = System.Windows.Media.Brush;
using Color = System.Windows.Media.Color;

namespace CodeShellManager.Views;

/// <summary>
/// Editor for <see cref="AppSettings.OpenWith"/> (issue #178): add / remove / reorder /
/// default, label, program with Browse…, a template per session kind, and a preview of the
/// fully resolved command for each kind. The preview is the import-trust half of the design —
/// an entry that arrived in an imported backup shows exactly what a click would run.
/// </summary>
public partial class OpenWithDialog : Window
{
    private static readonly SolidColorBrush Muted = new(Color.FromRgb(0x6c, 0x70, 0x86));
    private static readonly SolidColorBrush Warn = new(Color.FromRgb(0xfa, 0xb3, 0x87));   // peach
    private static readonly SolidColorBrush Bad = new(Color.FromRgb(0xf3, 0x8b, 0xa8));    // pink

    /// <summary>Sample session values for the previews. Deliberately ordinary — the preview shows shape, not validation of a real folder.</summary>
    private static readonly OpenWithContext SampleLocal = new(SessionKind.Local, Folder: @"C:\repos\my project");
    private static readonly OpenWithContext SampleWsl = new(SessionKind.Wsl,
        Folder: @"\\wsl$\Ubuntu\home\you\project", Distro: "Ubuntu", LinuxPath: "/home/you/project");
    private static readonly OpenWithContext SampleSsh = new(SessionKind.Ssh,
        User: "you", Host: "build-box", Port: 22, RemoteFolder: "/srv/app");

    private readonly ObservableCollection<EntryRow> _rows = new();
    private bool _loading;
    private readonly System.Windows.Threading.DispatcherTimer _diskCheckTimer =
        new() { Interval = TimeSpan.FromMilliseconds(350) };
    private int _diskCheckSeq;

    private async void DiskCheck_Tick(object? sender, EventArgs e)
    {
        _diskCheckTimer.Stop();
        if (Selected is not { } row) return;
        string exe = row.Entry.Executable;
        int seq = ++_diskCheckSeq;
        var (exists, error) = await Task.Run(() =>
        {
            string? err = OpenWithService.ValidateExecutableOnDisk(exe, allowBatch: true);
            return (File.Exists(exe), err);
        });
        // Stale: the user kept typing or picked another entry while the check ran.
        if (seq != _diskCheckSeq || !ReferenceEquals(Selected, row) || row.Entry.Executable != exe) return;
        if (error != null) SetStatus(ExeStatus, error, Bad);
        else if (!exists) SetStatus(ExeStatus, "Not found on this machine — the entry will fail until the program is installed.", Warn);
        else SetStatus(ExeStatus, "", Muted);
    }

    /// <summary>The edited list, sanitized. Set on Save.</summary>
    public List<OpenWithEntry>? Result { get; private set; }

    public OpenWithDialog(IEnumerable<OpenWithEntry>? initial)
    {
        InitializeComponent();
        _diskCheckTimer.Tick += DiskCheck_Tick;
        Closed += (_, _) => _diskCheckTimer.Stop();
        foreach (var e in initial ?? []) _rows.Add(new EntryRow(e.Clone()));
        EntriesList.ItemsSource = _rows;
        if (_rows.Count > 0) EntriesList.SelectedIndex = 0;
        UpdateEmptyState();
    }

    private EntryRow? Selected => EntriesList.SelectedItem as EntryRow;

    private void UpdateEmptyState()
    {
        bool empty = _rows.Count == 0;
        DetailPanel.Visibility = empty ? Visibility.Collapsed : Visibility.Visible;
        EmptyHint.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
        if (!empty && Selected == null) EntriesList.SelectedIndex = 0;
    }

    private void EntriesList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var row = Selected;
        DetailPanel.IsEnabled = row != null;
        if (row == null) return;
        _loading = true;
        LabelBox.Text = row.Entry.Label;
        ExeBox.Text = row.Entry.Executable;
        LocalArgsBox.Text = row.Entry.LocalArgs ?? "";
        WslArgsBox.Text = row.Entry.WslArgs ?? "";
        SshArgsBox.Text = row.Entry.SshArgs ?? "";
        _loading = false;
        RefreshPreview(row);
    }

    private void Field_Changed(object sender, TextChangedEventArgs e)
    {
        if (_loading || Selected is not { } row) return;
        row.Entry.Label = LabelBox.Text;
        row.Entry.Executable = ExeBox.Text.Trim();
        row.Entry.LocalArgs = NullIfBlank(LocalArgsBox.Text);
        row.Entry.WslArgs = NullIfBlank(WslArgsBox.Text);
        row.Entry.SshArgs = NullIfBlank(SshArgsBox.Text);
        RefreshPreview(row);
    }

    private static string? NullIfBlank(string s) => string.IsNullOrWhiteSpace(s) ? null : s.Trim();

    private void RefreshPreview(EntryRow row)
    {
        var e = row.Entry;
        string? exeError = OpenWithService.ValidateExecutable(e.Executable, allowBatch: true);
        if (exeError != null)
            SetStatus(ExeStatus, exeError, Bad);
        else if (OpenWithService.IsBatch(e.Executable))
            SetStatus(ExeStatus,
                "Batch file — Windows runs it through cmd.exe. A folder or value containing any of " +
                OpenWithService.BatchMetaChars + " will be refused rather than passed. Prefer the program's .exe.", Warn);
        else
        {
            SetStatus(ExeStatus, "", Muted);
            // The on-disk checks (exists, real path, network drive, renamed shell) touch the
            // disk, so they run debounced on the pool rather than per keystroke on this thread.
            _diskCheckTimer.Stop();
            _diskCheckTimer.Start();
        }

        PreviewFor(LocalPreview, e, SampleLocal);
        PreviewFor(WslPreview, e, SampleWsl);
        PreviewFor(SshPreview, e, SampleSsh);

        row.HasProblem = Problem(e) != null;
        row.Notify();
    }

    private static void PreviewFor(TextBlock target, OpenWithEntry e, OpenWithContext ctx)
    {
        if (!e.AppliesTo(ctx.Kind))
        {
            SetStatus(target, $"Not offered for {OpenWithService.KindName(ctx.Kind)} sessions.", Muted);
            return;
        }
        var (cmd, error) = OpenWithService.Build(e, ctx);
        if (cmd != null) SetStatus(target, "▶ " + cmd.Display, Muted);
        else SetStatus(target, "⚠ " + error, Bad);
    }

    private static void SetStatus(TextBlock tb, string text, Brush brush)
    {
        tb.Text = text;
        tb.Foreground = brush;
        tb.Visibility = text.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>What blocks Save for this entry, or null.</summary>
    private static string? Problem(OpenWithEntry e)
    {
        if (string.IsNullOrWhiteSpace(e.Label)) return "needs a label";
        if (OpenWithService.ValidateExecutable(e.Executable, allowBatch: true) is { } exeError) return exeError;
        if (!e.AppliesTo(SessionKind.Local) && !e.AppliesTo(SessionKind.Wsl) && !e.AppliesTo(SessionKind.Ssh))
            return "needs arguments for at least one session kind (e.g. {folder} for local sessions)";
        foreach (var kind in new[] { SessionKind.Local, SessionKind.Wsl, SessionKind.Ssh })
            if (OpenWithService.ValidateTemplate(e.ArgsFor(kind), kind) is { } t)
                return $"{OpenWithService.KindName(kind)} arguments: {t}";
        return null;
    }

    private async void Browse_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is not { } row) return;
        var dlg = new Microsoft.Win32.OpenFileDialog
        {
            Title = "Choose the editor's program",
            Filter = "Programs (*.exe)|*.exe|Launcher scripts (*.cmd;*.bat)|*.cmd;*.bat",
        };
        // No existence check here (disk I/O on the UI thread) — the shell dialog copes with a
        // folder that isn't there. Only a local, fully qualified path is offered, never a share.
        try
        {
            if (OpenWithService.ValidateExecutable(row.Entry.Executable, allowBatch: true) == null)
                dlg.InitialDirectory = Path.GetDirectoryName(row.Entry.Executable);
        }
        catch { }
        if (dlg.ShowDialog(this) != true) return;

        string path = dlg.FileName;
        if (OpenWithService.IsBatch(path))
        {
            // Resolve a launcher shim (code.cmd, a Toolbox script) to the real program so the
            // entry never goes through cmd.exe at all. Reads the script — off the UI thread.
            string? exe = await Task.Run(() => OpenWithService.ResolveShim(path));
            if (exe == null)
            {
                System.Windows.MessageBox.Show(this,
                    $"Couldn't tell which program {Path.GetFileName(path)} starts (it names none, or more than one). " +
                    "Choose the editor's .exe directly — a batch file runs through cmd.exe.",
                    "Open with", MessageBoxButton.OK, MessageBoxImage.Warning);
                return;
            }
            System.Windows.MessageBox.Show(this,
                $"{Path.GetFileName(path)} is a launcher script. Using the program it starts instead:\n\n{exe}",
                "Open with", MessageBoxButton.OK, MessageBoxImage.Information);
            path = exe;
        }
        if (!ReferenceEquals(Selected, row)) return;
        ExeBox.Text = path;
        if (string.IsNullOrWhiteSpace(LabelBox.Text))
            LabelBox.Text = Path.GetFileNameWithoutExtension(path);
        if (string.IsNullOrWhiteSpace(LocalArgsBox.Text) && string.IsNullOrWhiteSpace(WslArgsBox.Text)
            && string.IsNullOrWhiteSpace(SshArgsBox.Text))
            LocalArgsBox.Text = "{folder}";
    }

    private void Add_Click(object sender, RoutedEventArgs e)
    {
        var row = new EntryRow(new OpenWithEntry { Label = "New editor", LocalArgs = "{folder}", IsDefault = _rows.Count == 0 });
        _rows.Add(row);
        EntriesList.SelectedItem = row;
        UpdateEmptyState();
        LabelBox.Focus();
        LabelBox.SelectAll();
    }

    private async void Detect_Click(object sender, RoutedEventArgs e)
    {
        DetectBtn.IsEnabled = false;
        try
        {
            // Disk + registry + a vswhere spawn: off the UI thread.
            var found = await Task.Run(OpenWithDetector.Detect);
            var have = new HashSet<string>(_rows.Select(r => Normalize(r.Entry.Executable)), StringComparer.OrdinalIgnoreCase);
            var added = new List<string>();
            foreach (var entry in found)
            {
                if (!have.Add(Normalize(entry.Executable))) continue;
                entry.IsDefault = _rows.Count == 0;
                _rows.Add(new EntryRow(entry));
                added.Add(entry.Label);
            }
            UpdateEmptyState();
            foreach (var r in _rows) { r.HasProblem = Problem(r.Entry) != null; r.Notify(); }
            System.Windows.MessageBox.Show(this,
                added.Count == 0 ? "Nothing new found." : "Added: " + string.Join(", ", added),
                "Detect installed editors", MessageBoxButton.OK, MessageBoxImage.Information);
        }
        finally { DetectBtn.IsEnabled = true; }
    }

    private static string Normalize(string path)
    {
        try { return Path.GetFullPath(path); } catch { return path; }
    }

    private void Delete_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is not { } row) return;
        int i = _rows.IndexOf(row);
        _rows.Remove(row);
        if (row.Entry.IsDefault && _rows.Count > 0) _rows[0].Entry.IsDefault = true;
        foreach (var r in _rows) r.Notify();
        if (_rows.Count > 0) EntriesList.SelectedIndex = Math.Min(i, _rows.Count - 1);
        UpdateEmptyState();
    }

    private void MoveUp_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is not { } row) return;
        int i = _rows.IndexOf(row);
        if (i > 0) { _rows.Move(i, i - 1); EntriesList.SelectedItem = row; }
    }

    private void MoveDown_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is not { } row) return;
        int i = _rows.IndexOf(row);
        if (i >= 0 && i < _rows.Count - 1) { _rows.Move(i, i + 1); EntriesList.SelectedItem = row; }
    }

    private void MakeDefault_Click(object sender, RoutedEventArgs e)
    {
        if (Selected is not { } row) return;
        foreach (var r in _rows) { r.Entry.IsDefault = ReferenceEquals(r, row); r.Notify(); }
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        foreach (var r in _rows)
        {
            if (Problem(r.Entry) is { } problem)
            {
                EntriesList.SelectedItem = r;
                System.Windows.MessageBox.Show(this, $"\"{r.Entry.Label}\": {problem}", "Open with",
                    MessageBoxButton.OK, MessageBoxImage.Warning);
                return;
            }
        }
        // Same sanitizer state.json goes through, so what is saved is what will load.
        Result = OpenWithService.Sanitize(_rows.Select(r => r.Entry), allowBatch: true);
        DialogResult = true;
        Close();
    }

    /// <summary>List row: wraps the entry being edited and raises change notifications for the template.</summary>
    private sealed class EntryRow : INotifyPropertyChanged
    {
        public EntryRow(OpenWithEntry entry)
        {
            Entry = entry;
            HasProblem = Problem(entry) != null;
        }

        public OpenWithEntry Entry { get; }
        public bool HasProblem { get; set; }
        public string DisplayLabel => string.IsNullOrWhiteSpace(Entry.Label) ? "(no label)" : Entry.Label;
        public Visibility StarVisibility => Entry.IsDefault ? Visibility.Visible : Visibility.Collapsed;
        public Visibility NoStarVisibility => Entry.IsDefault ? Visibility.Collapsed : Visibility.Visible;
        public Visibility ProblemVisibility => HasProblem ? Visibility.Visible : Visibility.Collapsed;

        public event PropertyChangedEventHandler? PropertyChanged;
        public void Notify() => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(null));
    }
}

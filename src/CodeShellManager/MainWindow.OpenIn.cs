using System;
using System.Diagnostics;
using System.IO;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Media;
using CodeShellManager.Services;
using CodeShellManager.ViewModels;
using WpfButton = System.Windows.Controls.Button;
using Brushes = System.Windows.Media.Brushes;
using Color = System.Windows.Media.Color;
using FontFamily = System.Windows.Media.FontFamily;

namespace CodeShellManager;

/// <summary>
/// "Open in VS Code" — on the terminal toolbar next to 📁 / >_, and in the sidebar right-click
/// menu next to "Open in Explorer" / "Open PowerShell here". It used to live in the git menu,
/// which was the wrong home: opening a folder in an editor has nothing to do with git, and
/// should work for a folder that isn't a repo. A configurable "Open with…" for other editors
/// (and VS Code installs this lookup misses) is tracked in #178.
/// </summary>
public partial class MainWindow
{
    /// <summary>
    /// Resolved once per process, off the UI thread (it walks PATH and touches the disk).
    /// Null result = not found; the button and menu item then simply don't appear.
    /// </summary>
    private static Task<string?>? _vsCodeLookup;

    private static Task<string?> VsCodeLookup() => _vsCodeLookup ??= Task.Run(FindVsCode);

    /// <summary>Only a session with a local folder can be opened — not SSH.</summary>
    private static bool CanOpenInEditor(SessionViewModel vm) =>
        !vm.Session.IsRemote && !string.IsNullOrEmpty(vm.WorkingFolder);

    /// <summary>
    /// The toolbar button. Starts collapsed and appears once the lookup finds VS Code, so a
    /// machine without it never shows a button that can't work.
    /// </summary>
    private WpfButton BuildVsCodeButton(SessionViewModel vm)
    {
        var btn = new WpfButton
        {
            Content = "{ }",
            ToolTip = "Open folder in VS Code",
            Background = Brushes.Transparent,
            BorderThickness = new Thickness(0),
            Foreground = new SolidColorBrush(Color.FromRgb(0xa6, 0xad, 0xc8)),
            FontSize = 11,
            FontFamily = new FontFamily("Consolas"),
            Cursor = System.Windows.Input.Cursors.Hand,
            Padding = new Thickness(5, 2, 5, 2),
            Margin = new Thickness(0, 0, 4, 0),
            Visibility = Visibility.Collapsed,
        };
        btn.Click += (_, _) => OpenInVsCode(vm.WorkingFolder);

        if (CanOpenInEditor(vm))
        {
            _ = VsCodeLookup().ContinueWith(t =>
            {
                if (t.IsCompletedSuccessfully && t.Result != null)
                    Dispatcher.BeginInvoke(() => btn.Visibility = Visibility.Visible);
            }, TaskScheduler.Default);
        }
        return btn;
    }

    /// <summary>True when VS Code has been found (the lookup has finished and succeeded).</summary>
    private static bool IsVsCodeAvailable() =>
        VsCodeLookup() is { IsCompletedSuccessfully: true, Result: not null };

    private void OpenInVsCode(string folder)
    {
        _ = Task.Run(async () =>
        {
            try
            {
                string? codeExe = await VsCodeLookup().ConfigureAwait(false);
                if (codeExe == null) return;
                // Code.exe directly with the folder as one argv element — not code.cmd, which
                // would put the folder through cmd.exe's parser (a folder named with & or %
                // could then be read as a command).
                var psi = new ProcessStartInfo(codeExe) { UseShellExecute = false };
                psi.ArgumentList.Add(folder);
                Process.Start(psi)?.Dispose();
            }
            catch (Exception ex)
            {
                _ = Dispatcher.BeginInvoke(() => ToastHelper.Show("Couldn't open VS Code", ex.Message));
            }
        });
    }

    /// <summary>
    /// Finds VS Code's Code.exe via the <c>code.cmd</c> launcher on PATH (it lives in
    /// <c>&lt;install&gt;\bin\</c>). File checks only — no process is spawned to find it.
    /// Misses installs not on PATH, Insiders, portable copies and other editors; see #178.
    /// </summary>
    private static string? FindVsCode()
    {
        try
        {
            string path = Environment.GetEnvironmentVariable("PATH") ?? "";
            foreach (string dir in path.Split(';', StringSplitOptions.RemoveEmptyEntries))
            {
                string d = dir.Trim().Trim('"');
                if (d.Length == 0 || d.StartsWith(@"\\", StringComparison.Ordinal)) continue; // skip network dirs
                string cmd = Path.Combine(d, "code.cmd");
                if (!File.Exists(cmd)) continue;
                string exe = Path.GetFullPath(Path.Combine(d, "..", "Code.exe"));
                if (File.Exists(exe)) return exe;
            }
        }
        catch { }
        return null;
    }
}

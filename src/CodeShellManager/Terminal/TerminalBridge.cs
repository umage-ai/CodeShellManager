using System;
using System.Text.Json;
using System.Threading.Tasks;
using CodeShellManager.Models;
using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.Wpf;
using WpfApplication    = System.Windows.Application;
using WpfClipboard      = System.Windows.Clipboard;

namespace CodeShellManager.Terminal;

/// <summary>
/// Bridges a WebView2 running xterm.js with a PseudoTerminal (ConPTY).
/// Also handles clipboard (Ctrl+Shift+C/V, right-click paste) and
/// file drag-and-drop (converts dropped files to their paths in the terminal).
/// </summary>
public sealed class TerminalBridge : IDisposable
{
    private readonly WebView2 _webView;
    private PseudoTerminal? _pty;
    private bool _ready;
    // Last terminal size reported by xterm.js — applied immediately on PTY attach
    // so the PTY starts at the right dimensions even if resize fired before AttachPty.
    private (int cols, int rows) _lastSize = (80, 24);

    // Output that arrived before the page finished loading is buffered here
    private readonly System.Text.StringBuilder _outputBuffer = new();

    public event Action<string>? RawOutputReceived;
    public event Action? UserInput;

    private static void Log(string msg)
    {
        try
        {
            string path = System.IO.Path.Combine(
                System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData),
                "CodeShellManager", "crash.log");
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
            System.IO.File.AppendAllText(path, $"[{DateTime.Now:HH:mm:ss.fff}] BRIDGE {msg}\n");
        }
        catch { }
    }

    public TerminalBridge(WebView2 webView)
    {
        _webView = webView;
    }

    /// <summary>
    /// Initializes WebView2, navigates to terminal.html and AWAITS full page load
    /// before returning. This ensures PTY output is never dropped.
    /// </summary>
    public async Task InitializeAsync(string htmlPath)
    {
        Log($"InitializeAsync: htmlPath={htmlPath}");

        // Use AppData for the WebView2 user-data folder so the app works when
        // installed under Program Files (which is not writable by the user process).
        string wv2DataDir = System.IO.Path.Combine(
            System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData),
            "CodeShellManager", "WebView2");
        System.IO.Directory.CreateDirectory(wv2DataDir);
        var env = await CoreWebView2Environment.CreateAsync(null, wv2DataDir);
        await _webView.EnsureCoreWebView2Async(env);
        Log("EnsureCoreWebView2Async done");

        var settings = _webView.CoreWebView2.Settings;
        settings.AreDevToolsEnabled = true;  // enable for debugging
        settings.AreDefaultContextMenusEnabled = false;
        settings.IsStatusBarEnabled = false;

        _webView.CoreWebView2.WebMessageReceived += OnWebMessageReceived;

        // Log JS console messages and process failures
        _webView.CoreWebView2.ProcessFailed += (_, e) =>
            Log($"WebView2 ProcessFailed: {e.ProcessFailedKind}");
        // Note: ConsoleMessageReceived needs AllowedOrigins — use WebResourceRequested for JS errors

        // Drag-and-drop is handled entirely in JS (terminal.html) via text/uri-list.
        // WPF OLE Drop events don't fire for WebView2 content — WebView2 registers its
        // own IDropTarget for its HWND and handles OLE drops before WPF sees them.

        // Navigate and WAIT for the page to finish loading
        var navDone = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

        void NavCompleted(object? s, CoreWebView2NavigationCompletedEventArgs e)
        {
            _webView.CoreWebView2.NavigationCompleted -= NavCompleted;
            Log($"NavigationCompleted: success={e.IsSuccess} httpStatus={e.HttpStatusCode} webErrorStatus={e.WebErrorStatus}");
            _ready = true;

            // Flush any PTY output that arrived during page load
            string buffered;
            lock (_outputBuffer)
            {
                buffered = _outputBuffer.ToString();
                _outputBuffer.Clear();
            }
            if (buffered.Length > 0)
            {
                string json = System.Text.Json.JsonSerializer.Serialize(
                    new { type = "output", data = buffered });
                WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
                {
                    try { _webView.CoreWebView2?.PostWebMessageAsString(json); }
                    catch { }
                });
            }

            navDone.TrySetResult(true);
        }

        _webView.CoreWebView2.NavigationCompleted += NavCompleted;
        _webView.CoreWebView2.Navigate(htmlPath);

        await navDone.Task;
    }

    // ── PTY data → xterm.js ──────────────────────────────────────────────────

    /// <summary>Last terminal size reported by xterm.js. Use this to start the PTY at the right size.</summary>
    public (int cols, int rows) TerminalSize => _lastSize;

    public void AttachPty(PseudoTerminal pty)
    {
        _pty = pty;
        _pty.DataReceived += OnPtyData;
        // Apply any resize that xterm.js reported before the PTY was attached
        _pty.Resize(_lastSize.cols, _lastSize.rows);
    }

    private void OnPtyData(string rawData)
    {
        RawOutputReceived?.Invoke(rawData);

        if (!_ready)
        {
            // Page not ready yet — buffer until NavigationCompleted flushes it
            lock (_outputBuffer) { _outputBuffer.Append(rawData); }
            return;
        }

        string json = JsonSerializer.Serialize(new { type = "output", data = rawData });
        WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
        {
            try { _webView.CoreWebView2?.PostWebMessageAsString(json); }
            catch { }
        });
    }

    // ── xterm.js messages → PTY / clipboard ─────────────────────────────────

    private void OnWebMessageReceived(object? sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        try
        {
            var json = e.TryGetWebMessageAsString();
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            string type = root.GetProperty("type").GetString() ?? "";

            switch (type)
            {
                case "input":
                    _pty?.Write(root.GetProperty("data").GetString() ?? "");
                    UserInput?.Invoke();
                    break;

                case "resize":
                {
                    int cols = root.GetProperty("cols").GetInt32();
                    int rows = root.GetProperty("rows").GetInt32();
                    _lastSize = (cols, rows);
                    _pty?.Resize(cols, rows);
                    break;
                }

                case "getClipboard":
                    // xterm.js wants to paste — return clipboard text on UI thread
                    WpfApplication.Current?.Dispatcher.Invoke(() =>
                    {
                        string text = WpfClipboard.ContainsText()
                            ? WpfClipboard.GetText()
                            : "";
                        if (!string.IsNullOrEmpty(text))
                            _pty?.Write(text);
                    });
                    break;

                case "setClipboard":
                    // xterm.js wants to copy selected text
                    string copy = root.GetProperty("text").GetString() ?? "";
                    if (!string.IsNullOrEmpty(copy))
                        WpfApplication.Current?.Dispatcher.Invoke(() =>
                            WpfClipboard.SetText(copy));
                    break;

                case "filesDropped":
                    // JS sends full paths via text/uri-list (file:// URIs from Explorer)
                    if (root.TryGetProperty("paths", out var pathsEl))
                    {
                        var pathsList = new System.Collections.Generic.List<string>();
                        foreach (var p in pathsEl.EnumerateArray())
                        {
                            string fp = p.GetString() ?? "";
                            if (!string.IsNullOrEmpty(fp))
                                pathsList.Add(fp.Contains(' ') ? $"\"{fp}\"" : fp);
                        }
                        if (pathsList.Count > 0)
                            _pty?.Write(string.Join(" ", pathsList));
                    }
                    break;
            }
        }
        catch { }
    }

    // ── Utilities ─────────────────────────────────────────────────────────────

    public void ApplyFontSettings(AppSettings settings)
    {
        if (!_ready) return;
        var opts = new
        {
            fontFamily    = settings.TerminalFontFamily,
            fontSize      = settings.TerminalFontSize,
            fontLigatures = settings.TerminalFontLigatures,
            fontWeight    = settings.TerminalFontWeight,
            letterSpacing = settings.TerminalLetterSpacing,
            lineHeight    = settings.TerminalLineHeight,
        };
        string json = JsonSerializer.Serialize(new { type = "setOptions", options = opts });
        WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
        {
            try { _webView.CoreWebView2?.PostWebMessageAsString(json); }
            catch { }
        });
    }

    public void SendToTerminal(string text) => _pty?.Write(text);

    public void FitTerminal()
    {
        if (!_ready) return;
        // Use Background priority so this fires after WPF's layout/render passes,
        // ensuring the WebView2 has been measured and sized before we ask xterm to fit.
        WpfApplication.Current?.Dispatcher.BeginInvoke(
            System.Windows.Threading.DispatcherPriority.Background,
            new Action(() =>
            {
                try { _webView.CoreWebView2?.PostWebMessageAsString("{\"type\":\"fit\"}"); }
                catch { }
            }));
    }

    public void FocusTerminal()
    {
        if (!_ready) return;
        WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
        {
            try { _webView.CoreWebView2?.PostWebMessageAsString("{\"type\":\"focus\"}"); }
            catch { }
        });
    }

    public void Dispose()
    {
        if (_pty != null) _pty.DataReceived -= OnPtyData;
        if (_webView.CoreWebView2 != null)
        {
            _webView.CoreWebView2.WebMessageReceived -= OnWebMessageReceived;
            // NavigationCompleted is a local handler that unsubscribes itself — no need to remove here
        }
    }
}

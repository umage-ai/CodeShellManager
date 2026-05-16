using System;
using System.Text.Json;
using System.Threading.Tasks;
using CodeShellManager.Models;
using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.Wpf;
using WpfApplication    = System.Windows.Application;
using WpfClipboard      = System.Windows.Clipboard;
using WpfKeyEventArgs   = System.Windows.Input.KeyEventArgs;

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

    // Boot overlay — set by MainWindow before InitializeAsync; posted as setBootState after
    // navigation completes, and hidden via bootDone on the first PTY byte (see OnPtyData).
    private string? _bootLabel;
    private string? _bootAccentHex;
#pragma warning disable CS0169 // Used in Task 4; pragma keeps the build clean in between.
    private int _bootDoneFlag; // 0 = overlay still visible, 1 = bootDone already posted
#pragma warning restore CS0169

    // Output that arrived before the page finished loading is buffered here
    private readonly System.Text.StringBuilder _outputBuffer = new();

    // Diagnostics — gated by AppSettings.DebugTerminalTrace. Zero cost when off.
    /// <summary>AppSettings reference whose DebugTerminalTrace flag gates [DEBUG-tt] logging.</summary>
    public AppSettings? DebugSettings { get; set; }
    /// <summary>Short session-id prefix included in [DEBUG-tt] lines so multi-session logs are readable.</summary>
    public string? DebugSessionId { get; set; }
    private long _lastOutputTickMs;

    public event Action<string>? RawOutputReceived;
    public event Action? UserInput;

    /// <summary>
    /// Fires when the user presses a keyboard accelerator (Ctrl-combo, F-key, etc.)
    /// while the WebView2 has focus. Subscribers set <c>e.Handled = true</c> to prevent
    /// the key from also reaching xterm.js. The WPF WebView2 wrapper forwards
    /// accelerator keys through standard WPF PreviewKeyDown events.
    /// </summary>
    public event EventHandler<WpfKeyEventArgs>? AcceleratorKeyPressed;

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

    private void Trace(string msg)
    {
        if (DebugSettings?.DebugTerminalTrace != true) return;
        try
        {
            string path = System.IO.Path.Combine(
                System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData),
                "CodeShellManager", "crash.log");
            System.IO.Directory.CreateDirectory(System.IO.Path.GetDirectoryName(path)!);
            System.IO.File.AppendAllText(path,
                $"[{DateTime.Now:HH:mm:ss.fff}] [DEBUG-tt] {DebugSessionId ?? "?"} {msg}\n");
        }
        catch { }
    }

    public TerminalBridge(WebView2 webView)
    {
        _webView = webView;
    }

    /// <summary>
    /// Sets the boot-overlay label and accent color. Must be called before
    /// <see cref="InitializeAsync"/> — the bridge posts a setBootState message to the
    /// page as soon as navigation completes.
    /// </summary>
    public void SetBootContext(string label, string accentHex)
    {
        _bootLabel = label;
        _bootAccentHex = accentHex;
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

        // Match the boot overlay background so the WebView2 init flicker (the gap between
        // the control becoming visible and terminal.html rendering) is invisible.
        try { _webView.DefaultBackgroundColor = System.Drawing.Color.FromArgb(0x1e, 0x1e, 0x2e); }
        catch { }

        var settings = _webView.CoreWebView2.Settings;
        settings.AreDevToolsEnabled = true;  // enable for debugging
        settings.AreDefaultContextMenusEnabled = false;
        settings.IsStatusBarEnabled = false;

        _webView.CoreWebView2.WebMessageReceived += OnWebMessageReceived;

        // Surface accelerator keys (Ctrl-combos, etc.) to WPF so global shortcuts
        // still work when a terminal has focus. The WPF WebView2 wrapper forwards
        // accelerator presses through standard PreviewKeyDown events.
        _webView.PreviewKeyDown += OnAcceleratorKeyPressed;

        // Log JS console messages and process failures
        _webView.CoreWebView2.ProcessFailed += (_, e) =>
            Log($"WebView2 ProcessFailed: {e.ProcessFailedKind}");
        // Note: ConsoleMessageReceived needs AllowedOrigins — use WebResourceRequested for JS errors

        // Prevent WebView2 from navigating to dropped files.
        // When image/media files are dragged from Explorer, WebView2 intercepts the drop
        // at the browser level and navigates to the file:// URL before JS sees the drop
        // event — opening the file in the OS default viewer and leaving the drag overlay
        // stuck. Cancelling these navigations lets JS handle the drop normally.
        _webView.CoreWebView2.NavigationStarting += (_, args) =>
        {
            if (args.Uri.StartsWith("file://", StringComparison.OrdinalIgnoreCase))
            {
                bool allowed = args.Uri.EndsWith("terminal.html", StringComparison.OrdinalIgnoreCase)
                    || args.Uri.EndsWith("terminal-transparent.html", StringComparison.OrdinalIgnoreCase);
                if (!allowed) args.Cancel = true;
            }
        };

        // Navigate and WAIT for the page to finish loading
        var navDone = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);

        void NavCompleted(object? s, CoreWebView2NavigationCompletedEventArgs e)
        {
            _webView.CoreWebView2.NavigationCompleted -= NavCompleted;
            Log($"NavigationCompleted: success={e.IsSuccess} httpStatus={e.HttpStatusCode} webErrorStatus={e.WebErrorStatus}");
            _ready = true;

            // Apply boot-overlay state if MainWindow called SetBootContext before init.
            if (_bootLabel != null && _bootAccentHex != null)
            {
                var bootJson = JsonSerializer.Serialize(new
                {
                    type = "setBootState",
                    label = _bootLabel,
                    accentHex = _bootAccentHex
                });
                try { _webView.CoreWebView2?.PostWebMessageAsString(bootJson); }
                catch { }
            }

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
        if (DebugSettings?.DebugTerminalTrace == true)
        {
            long now = Environment.TickCount64;
            long prev = System.Threading.Interlocked.Exchange(ref _lastOutputTickMs, now);
            long gap = prev == 0 ? 0 : now - prev;
            Trace($"OUTPUT recv len={rawData.Length} gap-since-prev={gap}ms");
        }

        RawOutputReceived?.Invoke(rawData);

        if (!_ready)
        {
            // Page not ready yet — buffer until NavigationCompleted flushes it
            lock (_outputBuffer) { _outputBuffer.Append(rawData); }
            return;
        }

        string json = JsonSerializer.Serialize(new { type = "output", data = rawData });
        long enqueueAt = DebugSettings?.DebugTerminalTrace == true ? Environment.TickCount64 : 0;
        int len = rawData.Length;
        WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
        {
            // Capture latency before any work so Trace's file I/O doesn't inflate the
            // measurement, then post the WebView2 message before tracing so the trace
            // overhead doesn't delay terminal rendering.
            long latencyMs = enqueueAt != 0 ? Environment.TickCount64 - enqueueAt : 0;
            try { _webView.CoreWebView2?.PostWebMessageAsString(json); }
            catch { }
            if (enqueueAt != 0)
                Trace($"OUTPUT post dispatcher-latency={latencyMs}ms len={len}");
        });
    }

    private void OnAcceleratorKeyPressed(object? sender, WpfKeyEventArgs e)
    {
        AcceleratorKeyPressed?.Invoke(this, e);
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
                {
                    string data = root.GetProperty("data").GetString() ?? "";
                    if (DebugSettings?.DebugTerminalTrace == true)
                    {
                        long t0 = Environment.TickCount64;
                        Trace($"INPUT len={data.Length}");
                        _pty?.Write(data);
                        Trace($"PTY-WROTE elapsed={Environment.TickCount64 - t0}ms");
                    }
                    else
                    {
                        _pty?.Write(data);
                    }
                    UserInput?.Invoke();
                    break;
                }

                case "resize":
                {
                    int cols = root.GetProperty("cols").GetInt32();
                    int rows = root.GetProperty("rows").GetInt32();
                    _lastSize = (cols, rows);
                    _pty?.Resize(cols, rows);
                    break;
                }

                case "getClipboard":
                    // xterm.js wants to paste — round-trip the text through term.paste() so
                    // bracketed paste mode (CSI ?2004h) is honored. Apps like Claude Code
                    // require the \e[200~ ... \e[201~ markers to treat multi-line input as
                    // a single paste rather than submitting on the first newline.
                    WpfApplication.Current?.Dispatcher.Invoke(() =>
                    {
                        if (!WpfClipboard.ContainsText()) return;
                        string text = WpfClipboard.GetText();
                        if (string.IsNullOrEmpty(text)) return;
                        string pasteJson = JsonSerializer.Serialize(new { type = "paste", data = text });
                        try { _webView.CoreWebView2?.PostWebMessageAsString(pasteJson); }
                        catch { }
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

    public void ApplyProfileOverrides(ShellSession session)
    {
        if (!_ready) return;
        if (!HasAnyOverride(session)) return;

        var opts = new System.Collections.Generic.Dictionary<string, object?>();
        if (session.ProfileFontFamily    != null) opts["fontFamily"]    = QuoteFontFamily(session.ProfileFontFamily);
        if (session.ProfileFontSize      != null) opts["fontSize"]      = session.ProfileFontSize;
        if (session.ProfileFontWeight    != null) opts["fontWeight"]    = session.ProfileFontWeight;
        if (session.ProfileFontLigatures != null) opts["fontLigatures"] = session.ProfileFontLigatures;
        if (session.ProfileCursorShape   != null) opts["cursorStyle"]   = session.ProfileCursorShape;
        if (session.ProfileCursorBlink   != null) opts["cursorBlink"]   = session.ProfileCursorBlink;
        if (session.ProfilePadding       != null) opts["padding"]       = session.ProfilePadding;
        if (session.ProfileRetroEffect   != null) opts["retro"]         = session.ProfileRetroEffect;
        if (!string.IsNullOrEmpty(session.ProfileColorSchemeJson))
            opts["theme"] = JsonSerializer.Deserialize<JsonElement>(session.ProfileColorSchemeJson);

        string json = JsonSerializer.Serialize(new { type = "setOptions", options = opts });
        WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
        {
            try { _webView.CoreWebView2?.PostWebMessageAsString(json); }
            catch { }
        });
    }

    // Older state.json entries may store an unquoted face like "0xProto Nerd Font".
    // CSS needs spaces-in-names quoted; quote + add a monospace fallback at apply
    // time so existing saved sessions render correctly without a re-import.
    private static string QuoteFontFamily(string face)
    {
        face = face.Trim();
        if (face.Length == 0) return face;
        if (face.StartsWith('\'') || face.StartsWith('"') || face.Contains(','))
            return face;
        return $"'{face}', monospace";
    }

    private static bool HasAnyOverride(ShellSession s) =>
        s.ProfileFontFamily != null || s.ProfileFontSize != null
        || s.ProfileFontWeight != null || s.ProfileFontLigatures != null
        || s.ProfileCursorShape != null || s.ProfileCursorBlink != null
        || s.ProfilePadding != null || s.ProfileRetroEffect != null
        || !string.IsNullOrEmpty(s.ProfileColorSchemeJson);

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
            try
            {
                // Move WPF keyboard focus onto the WebView2 host. Without this, focus
                // can stay on whichever WPF control was last clicked (e.g. a sidebar
                // item Border), so the JS term.focus() below has no effect at the
                // WPF level and typing goes nowhere.
                _webView.Focus();
                _webView.CoreWebView2?.PostWebMessageAsString("{\"type\":\"focus\"}");
            }
            catch { }
        });
    }

    public void Dispose()
    {
        if (_pty != null) _pty.DataReceived -= OnPtyData;
        if (_webView.CoreWebView2 != null)
        {
            _webView.CoreWebView2.WebMessageReceived -= OnWebMessageReceived;
            try { _webView.PreviewKeyDown -= OnAcceleratorKeyPressed; }
            catch { }
            // NavigationCompleted is a local handler that unsubscribes itself — no need to remove here
        }
    }
}

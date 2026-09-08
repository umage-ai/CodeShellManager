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
    // Fallback hides the overlay after BootDoneFallbackMs so silent sessions (e.g. a child
    // that prints nothing, an SSH connect that's mid-handshake) aren't locked out.
    private const int BootDoneFallbackMs = 8000;
    private string? _bootLabel;
    private string? _bootAccentHex;
    private int _bootDoneFlag; // 0 = overlay still visible, 1 = bootDone already posted

    // Output that arrived before the page finished loading is buffered here
    private readonly System.Text.StringBuilder _outputBuffer = new();

    // Coalesces PTY chunks into one dispatcher post per tick (issue #70).
    private readonly OutputCoalescer _coalescer;

    /// <summary>
    /// True when this session's pane is the active one. Foreground output posts at
    /// Normal priority; background sessions post at Background priority so a chatty
    /// off-screen session can't delay the pane the user is actually typing into.
    /// Set by MainWindow whenever MainViewModel.ActiveSession changes.
    /// </summary>
    public bool IsForeground { get; set; }

    // Diagnostics — gated by AppSettings.DebugTerminalTrace. Zero cost when off.
    /// <summary>AppSettings reference whose DebugTerminalTrace flag gates [DEBUG-tt] logging.</summary>
    public AppSettings? DebugSettings { get; set; }
    /// <summary>Short session-id prefix included in [DEBUG-tt] lines so multi-session logs are readable.</summary>
    public string? DebugSessionId { get; set; }
    private long _lastOutputTickMs;

    // Set when a coalesced flush is queued, read when it runs, so the gap between the two
    // is the dispatcher queue latency — the number that says whether the UI pump is the
    // bottleneck. OutputCoalescer guarantees at most one flush in flight per bridge, so a
    // single field is sufficient. Restored here after 71e1294 removed the original along
    // with the per-chunk post it was attached to (issue #70).
    private long _flushEnqueuedAtMs;
    private bool _flushQueuedAsForeground;
    private long _lastInputTickMs;

    public event Action<string>? RawOutputReceived;

    /// <summary>
    /// Any input travelling to the PTY — keystrokes, pastes, and mouse reports.
    /// Used for "the user interacted with this session" (alert clearing).
    /// </summary>
    public event Action? UserInput;

    /// <summary>
    /// A real key press in this pane. Raised from xterm's <c>onKey</c> (posted as a
    /// <c>userkey</c> message), throttled to at most one per 500ms.
    ///
    /// Anything that changes UI state off the back of input must use this rather than
    /// <see cref="UserInput"/>. onData carries everything xterm sends to the PTY,
    /// including replies the TERMINAL itself generates — device-attribute answers
    /// (<c>ESC[?1;2c</c>), cursor-position reports, OSC colour replies, and focus
    /// in/out (<c>ESC[I</c>/<c>ESC[O</c>) — plus mouse reports when the app enables
    /// tracking. An earlier attempt filtered those by inspecting the bytes; it could
    /// not work, because a device-attribute reply is not distinguishable from typing
    /// by shape. xterm knows which is which (triggerDataEvent's wasUserInput flag) but
    /// does not surface it on onData, so onKey is the only honest source.
    /// </summary>
    public event Action? KeyboardInput;

    /// <summary>
    /// The user clicked into this pane. Posted from the page's <c>mousedown</c>, because
    /// WebView2 is an <c>HwndHost</c>: mouse input landing on hosted native content never
    /// raises WPF routed events, so a <c>PreviewMouseLeftButtonDown</c> on the host Border
    /// only fires for the thin ring around the terminal — never for the terminal itself.
    /// That is why clicking a pane did not make it the active session.
    /// </summary>
    public event Action? PaneActivated;

    /// <summary>
    /// Fires when the running shell program emits OSC 9001 (CSM shell integration).
    /// Carries the parsed key=value fields it included (color, git-branch, git-dirty, title, …).
    /// </summary>
    public event Action<System.Collections.Generic.IReadOnlyDictionary<string, string>>? ShellIntegrationReceived;

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

    // Queues the line; the disk write happens on a background drain. This used to open,
    // append to and close crash.log inline on whichever thread was tracing — the PTY reader
    // for output, the UI thread for the flush. At the session count this issue reproduces
    // at, that made the tracer a cause of the latency it was measuring (issue #70).
    private void Trace(string msg)
    {
        if (DebugSettings?.DebugTerminalTrace != true) return;
        Diagnostics.DiagnosticTrace.Write("DEBUG-tt", DebugSessionId, msg);
    }

    // Posts a one-shot bootDone message to the WebView2. Safe to call from any thread.
    private void PostBootDoneIfNeeded()
    {
        if (System.Threading.Interlocked.CompareExchange(ref _bootDoneFlag, 1, 0) != 0) return;
        WpfApplication.Current?.Dispatcher.BeginInvoke(() =>
        {
            try { _webView.CoreWebView2?.PostWebMessageAsString("{\"type\":\"bootDone\"}"); }
            catch { }
        });
    }

    public TerminalBridge(WebView2 webView)
    {
        _webView = webView;
        _coalescer = new OutputCoalescer(ScheduleFlush, PostOutput);
    }

    // Queues one coalesced flush. Background sessions yield to the foreground pane so
    // their output can't sit ahead of the active session's rendering or its keystrokes.
    private void ScheduleFlush(Action flush)
    {
        var dispatcher = WpfApplication.Current?.Dispatcher;
        if (dispatcher == null) return;

        // Read once, so the priority recorded in the trace is provably the one used for
        // the post. IsForeground is written from the UI thread while this runs on the PTY
        // thread, and a stale read here is itself a candidate explanation for the stall.
        bool foreground = IsForeground;

        if (DebugSettings?.DebugTerminalTrace == true)
        {
            System.Threading.Volatile.Write(ref _flushEnqueuedAtMs, Environment.TickCount64);
            _flushQueuedAsForeground = foreground;
        }

        dispatcher.BeginInvoke(
            foreground
                ? System.Windows.Threading.DispatcherPriority.Normal
                : System.Windows.Threading.DispatcherPriority.Background,
            flush);
    }

    // Runs on the UI thread. One WebView2 post per coalesced batch.
    private void PostOutput(string data)
    {
        bool tracing = DebugSettings?.DebugTerminalTrace == true;
        long enqueuedAt = tracing
            ? System.Threading.Interlocked.Exchange(ref _flushEnqueuedAtMs, 0)
            : 0;

        string json = JsonSerializer.Serialize(new { type = "output", data });
        try { _webView.CoreWebView2?.PostWebMessageAsString(json); }
        catch { }

        if (!tracing) return;

        long now = Environment.TickCount64;
        long lastInput = System.Threading.Volatile.Read(ref _lastInputTickMs);

        // dispatcher-latency: queued on the PTY thread -> ran on the UI thread. Large values
        //   mean the UI pump is the bottleneck. Read it together with prio: for a bg batch a
        //   large value may just be Background priority yielding correctly, which is why the
        //   UI-STALL heartbeat is logged separately and unattributed.
        // since-input: how long before this batch the user last typed. When a stall is
        //   reported, this is what ties a late flush to the keystroke it failed to echo.
        Trace($"OUTPUT flush len={data.Length} " +
              $"dispatcher-latency={(enqueuedAt == 0 ? -1 : now - enqueuedAt)}ms " +
              $"prio={(_flushQueuedAsForeground ? "fg" : "bg")} " +
              $"since-input={(lastInput == 0 ? -1 : now - lastInput)}ms");
    }

    /// <summary>
    /// Turns page-side timing probes on or off for a pane that is already running.
    /// Without this, toggling the trace setting mid-session would enable the host-side
    /// numbers while the page half stayed dark — and the renderer is exactly the component
    /// the host cannot see (issue #70).
    /// </summary>
    public void SetPageDiagnostics(bool on)
    {
        if (!_ready) return; // NavigationCompleted posts the initial state itself
        try
        {
            _webView.CoreWebView2?.PostWebMessageAsString(
                JsonSerializer.Serialize(new { type = "setDiag", on }));
        }
        catch { }
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

            // Turn on page-side timing when tracing is enabled. The host cannot see past
            // PostWebMessageAsString: if the renderer process is the thing that's starved,
            // every host-side number looks healthy and the stall is still real (issue #70).
            if (DebugSettings?.DebugTerminalTrace == true)
            {
                try
                {
                    _webView.CoreWebView2?.PostWebMessageAsString(
                        JsonSerializer.Serialize(new { type = "setDiag", on = true }));
                }
                catch { }
            }

            // Silent-session fallback: if the child writes nothing (or exits without
            // any output) the overlay would otherwise block the terminal indefinitely.
            // PostBootDoneIfNeeded is idempotent via Interlocked, so this is a no-op
            // when the first PTY byte beat the timer.
            _ = Task.Delay(BootDoneFallbackMs).ContinueWith(
                _ => PostBootDoneIfNeeded(), TaskScheduler.Default);

            // Flush any PTY output that arrived during page load
            string buffered;
            lock (_outputBuffer)
            {
                buffered = _outputBuffer.ToString();
                _outputBuffer.Clear();
            }
            // Route through the coalescer rather than posting directly: chunks that land
            // between `_ready = true` above and this drain go to the coalescer, so sharing
            // one buffer keeps load-time output in arrival order.
            if (buffered.Length > 0) _coalescer.Append(buffered);

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
        PostBootDoneIfNeeded();

        if (!_ready)
        {
            // Page not ready yet — buffer until NavigationCompleted flushes it
            lock (_outputBuffer) { _outputBuffer.Append(rawData); }
            return;
        }

        // Buffer instead of posting per chunk. Many chunks arriving before the dispatcher
        // gets a turn collapse into a single post, so background sessions can no longer
        // flood the shared UI queue and starve the foreground pane (issue #70).
        _coalescer.Append(rawData);
    }

    /// <summary>
    /// Turns dropped file paths into the text written to the PTY. Extracted so the filtering
    /// is testable without a WebView2 — see <c>DroppedPathsTests</c>.
    ///
    /// Paths from a drop are UNTRUSTED: the page derives them from the drag payload's
    /// text/uri-list with <c>decodeURIComponent</c>, so a drag source that controls that
    /// payload (a hostile page's dragstart, a crafted .url, another local app) can put any
    /// character in them percent-escaped. <c>%0A</c> decodes to a newline, and a newline
    /// written to a PTY is the user pressing Enter — one drop would have run a command in
    /// the focused session with no keystroke and no confirmation.
    ///
    /// Control characters are rejected rather than escaped: Win32 forbids them in filenames,
    /// so nothing legitimate is lost, and rejection has no escaping bug to get wrong later.
    /// </summary>
    internal static string BuildDroppedPathsPayload(IEnumerable<string> paths)
    {
        var quoted = new List<string>();
        foreach (string fp in paths)
        {
            if (string.IsNullOrEmpty(fp)) continue;

            // Control characters are rejected, not escaped. Win32 forbids them in filenames,
            // so nothing legitimate is lost, and rejection has no escaping bug to get wrong.
            if (fp.Any(char.IsControl)) continue;

            // A `"` cannot appear in a real Windows path either, and there is no quoting
            // that is simultaneously correct for cmd.exe, PowerShell and POSIX shells — the
            // session could be any of them. Rejecting is the only answer that is right in
            // all three.
            if (fp.Contains('"')) continue;

            quoted.Add(NeedsQuoting(fp) ? "\"" + fp + "\"" : fp);
        }
        return string.Join(" ", quoted);
    }

    /// <summary>
    /// True when a path must be wrapped in quotes before being typed into a shell.
    ///
    /// Not just spaces. The pane may be running cmd.exe, PowerShell, bash or a TUI, and each
    /// treats a different set of characters as syntax. A perfectly legal Windows filename
    /// like <c>a&amp;calc.txt</c> contains no space, so quoting only on space handed cmd.exe a
    /// bare <c>&amp;</c> — a command separator. Quoting on any shell metacharacter of any of
    /// them is the conservative union; over-quoting a path is harmless in all of them.
    /// </summary>
    private static bool NeedsQuoting(string path)
    {
        foreach (char c in path)
        {
            if (char.IsWhiteSpace(c)) return true;
            if ("&|<>^();,=!%$`'{}[]".IndexOf(c) >= 0) return true;
        }
        return false;
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
                        System.Threading.Volatile.Write(ref _lastInputTickMs, t0);
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

                // Posted from xterm's onKey — a real key event, never a terminal reply.
                // See terminal-init.js for why onData can't be used for this.
                case "userkey":
                    KeyboardInput?.Invoke();
                    break;

                // Posted from the page on mousedown. WebView2 is an HwndHost, so a click
                // in the terminal never reaches WPF as a routed event — this is the only
                // way the host learns the user clicked into this pane.
                case "activate":
                    PaneActivated?.Invoke();
                    break;

                // Page-side timing, only sent while setDiag is on and only when a threshold
                // is crossed — see terminal-init.js. Reports what happens after the host's
                // last visibility point: how long term.write blocked, and how long the
                // renderer then took to produce a frame.
                case "diag":
                {
                    if (DebugSettings?.DebugTerminalTrace != true) break;
                    string what = root.TryGetProperty("what", out var w) ? w.GetString() ?? "?" : "?";
                    double ms = root.TryGetProperty("ms", out var m) ? m.GetDouble() : -1;
                    int len = root.TryGetProperty("len", out var l) ? l.GetInt32() : -1;
                    Trace($"PAGE {what}={ms:0}ms len={len}");
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

                case "shellIntegration":
                    if (root.TryGetProperty("fields", out var fieldsEl)
                        && fieldsEl.ValueKind == JsonValueKind.Object)
                    {
                        var dict = new System.Collections.Generic.Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
                        foreach (var prop in fieldsEl.EnumerateObject())
                        {
                            if (prop.Value.ValueKind == JsonValueKind.String)
                                dict[prop.Name] = prop.Value.GetString() ?? "";
                        }
                        if (dict.Count > 0)
                            ShellIntegrationReceived?.Invoke(dict);
                    }
                    break;

                case "filesDropped":
                    // JS sends full paths via text/uri-list (file:// URIs from Explorer).
                    //
                    // These are UNTRUSTED. The page derives them from the drag payload with
                    // decodeURIComponent, so a drag source that controls text/uri-list — a
                    // hostile page's dragstart, a crafted .url shortcut, another local app —
                    // can put ANY character in them, percent-escaped. `%0A` decodes to a
                    // newline, and a newline written to a PTY is the user pressing Enter:
                    // one drop would have run a command in the focused session with no
                    // keystroke and no confirmation.
                    //
                    // Control characters are rejected outright rather than escaped. No real
                    // Windows path contains one (the Win32 API forbids them in filenames),
                    // so nothing legitimate is lost, and "reject" has no escaping bug to get
                    // wrong later. Embedded quotes are escaped so the quoting below can't be
                    // broken out of either.
                    if (root.TryGetProperty("paths", out var pathsEl))
                    {
                        var raw = new System.Collections.Generic.List<string>();
                        foreach (var p in pathsEl.EnumerateArray())
                            raw.Add(p.GetString() ?? "");

                        string payload = BuildDroppedPathsPayload(raw);
                        if (payload.Length > 0) _pty?.Write(payload);
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
        // Malformed JSON here must not take the session down. This value is normally
        // produced by SchemeMapper, but ImportExportService will deserialize a whole
        // AppState from any file the user opens, so it can be arbitrary — and an
        // unhandled throw on this path aborts the launch of an otherwise fine session.
        // Dropping the theme degrades to the default palette, which is survivable.
        if (!string.IsNullOrEmpty(session.ProfileColorSchemeJson))
        {
            try { opts["theme"] = JsonSerializer.Deserialize<JsonElement>(session.ProfileColorSchemeJson); }
            catch (JsonException ex)
            {
                Log($"ignoring malformed ProfileColorSchemeJson for '{session.Name}': {ex.Message}");
            }
        }

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
        PostBootDoneIfNeeded();
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

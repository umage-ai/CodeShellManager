using System.Text.Json;
using Avalonia.Controls;
using Avalonia.Platform;
using Avalonia.Threading;

namespace CrossPlatformSpike;

/// <summary>
/// Routes messages between the xterm.js page and the PTY.
/// JS → C#: invokeCSharpAction(json) raises WebMessageReceived; messages are
///   {type:'ready',cols,rows} | {type:'input',data} | {type:'resize',cols,rows}.
/// C# → JS: PTY bytes are base64-encoded and pushed via InvokeScript("window.ptyData('…')").
/// </summary>
public sealed class SpikeBridge : IDisposable
{
    private readonly NativeWebView _webView;
    private readonly SpikePty _pty = new();
    private readonly Action<string> _setStatus;

    public SpikeBridge(NativeWebView webView, Action<string> setStatus)
    {
        _webView = webView;
        _setStatus = setStatus;
        // Ubuntu 24.04 ships no WPE WebKit packages, so prefer the WebKitGTK
        // adapter (libwebkitgtk-6.0) on Linux. WPE-capable distros would work
        // without this, but the spike validates the path that stock Ubuntu needs.
        _webView.EnvironmentRequested += (_, args) =>
        {
            if (args is LinuxWpeWebViewEnvironmentRequestedEventArgs wpe)
                wpe.PreferWebKitGtkInstead = true;
        };
        _webView.WebMessageReceived += OnWebMessage;
        _pty.DataReceived += OnPtyData;
        _pty.Exited += code => Dispatcher.UIThread.Post(() =>
            _setStatus($"PTY: exited with code {code} ({_pty.ShellPath})"));
    }

    public void NavigateToTerminal()
    {
        var html = Path.Combine(AppContext.BaseDirectory, "Assets", "terminal.html");
        _webView.Navigate(new Uri(html));
    }

    private async void OnWebMessage(object? sender, WebMessageReceivedEventArgs e)
    {
        if (e.Body is null) return;
        try
        {
            using var doc = JsonDocument.Parse(e.Body);
            var msg = doc.RootElement;
            switch (msg.GetProperty("type").GetString())
            {
                case "ready":
                    int cols = msg.GetProperty("cols").GetInt32();
                    int rows = msg.GetProperty("rows").GetInt32();
                    // Diagnostic probe: proves the C#→JS direction independently of
                    // PTY traffic and reports the size the page actually sees.
                    var probe = await _webView.InvokeScript(
                        "`innerSize=${innerWidth}x${innerHeight} ptyData=${typeof window.ptyData}`");
                    Console.WriteLine($"[spike] ready {cols}x{rows}; InvokeScript probe => {probe ?? "null"}");
                    await _pty.StartAsync(cols, rows);
                    _setStatus($"PTY: running {_pty.ShellPath} (pid {_pty.Pid}, {cols}x{rows})");
                    _ = RunDiagnosticsAsync();
                    break;
                case "input":
                    await _pty.WriteAsync(msg.GetProperty("data").GetString() ?? "");
                    break;
                case "resize":
                    _pty.Resize(msg.GetProperty("cols").GetInt32(), msg.GetProperty("rows").GetInt32());
                    break;
            }
        }
        catch (Exception ex)
        {
            _setStatus($"Bridge error: {ex.Message}");
        }
    }

    private void OnPtyData(byte[] chunk)
    {
        var b64 = Convert.ToBase64String(chunk);
        Dispatcher.UIThread.Post(async () =>
        {
            try { await _webView.InvokeScript($"window.ptyData('{b64}')"); }
            catch (Exception ex)
            {
                // Spike diagnostic: a broken C#→JS path must be visible, not silent.
                Console.WriteLine($"[spike] ptyData InvokeScript failed: {ex.GetType().Name}: {ex.Message}");
            }
        });
    }

    /// <summary>
    /// Headless spike diagnostics: reads xterm's buffer back through InvokeScript
    /// (proves PTY output reached the page without needing to see the screen) and
    /// injects a shell command through the page's own send() (proves the full
    /// input path: JS → C# → PTY → shell). Results go to stdout / the run log.
    /// </summary>
    private async Task RunDiagnosticsAsync()
    {
        await Task.Delay(3000);
        Console.WriteLine($"[spike] diag line0 => {await JsAsync("term.buffer.active.getLine(0)?.translateToString(true) ?? 'null'")}");
        await JsAsync("send({type:'input',data:'touch /tmp/spike-probe; echo PROBE_OK\\n'})");
        await Task.Delay(2500);
        Console.WriteLine($"[spike] diag tail => {await JsAsync(
            "Array.from({length:term.buffer.active.length},(_,i)=>term.buffer.active.getLine(i)?.translateToString(true))" +
            ".filter(l=>l&&l.trim()).slice(-4).join(' || ')")}");
    }

    private Task<string?> JsAsync(string script) =>
        Dispatcher.UIThread.InvokeAsync(async () =>
        {
            try { return await _webView.InvokeScript(script); }
            catch (Exception ex) { return "ERR: " + ex.Message; }
        });

    public void Dispose()
    {
        _webView.WebMessageReceived -= OnWebMessage;
        _pty.Dispose();
    }
}

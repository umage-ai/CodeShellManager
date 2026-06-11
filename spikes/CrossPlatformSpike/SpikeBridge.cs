using System.Text.Json;
using Avalonia.Controls;
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
                    await _pty.StartAsync(cols, rows);
                    _setStatus($"PTY: running {_pty.ShellPath} (pid {_pty.Pid}, {cols}x{rows})");
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
            catch { /* page navigating or torn down; drop the chunk */ }
        });
    }

    public void Dispose()
    {
        _webView.WebMessageReceived -= OnWebMessage;
        _pty.Dispose();
    }
}

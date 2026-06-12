using Avalonia.Controls;

namespace CrossPlatformSpike;

public partial class MainWindow : Window
{
    private SpikeBridge? _bridge;

    public MainWindow()
    {
        InitializeComponent();
        Opened += OnOpened;
        Closed += (_, _) => _bridge?.Dispose();
    }

    private void OnOpened(object? sender, EventArgs e)
    {
        _bridge = new SpikeBridge(WebView, s => StatusText.Text = s);
        StatusText.Text = "PTY: waiting for terminal page…";
        _bridge.NavigateToTerminal();
    }
}

# Cross-Platform Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove that Avalonia 12 + NativeWebView (xterm.js) + Porta.Pty gives a working interactive terminal, validated on Windows and WSLg with a build-only CI matrix on all three OSes.

**Architecture:** A standalone throwaway app in `spikes/CrossPlatformSpike/` (not in the main solution). One window hosts a `NativeWebView` loading a stripped-down xterm.js page; `SpikePty` wraps Porta.Pty; `SpikeBridge` routes JSON messages between them (JS→C# via `invokeCSharpAction`/`WebMessageReceived`, C#→JS via `InvokeScript` with base64-encoded PTY bytes).

**Tech Stack:** .NET 10, Avalonia 12.x (MIT), Avalonia.Controls.WebView 12.x (MIT), Porta.Pty 1.0.x (MIT), xterm.js (MIT, vendored from `src/CodeShellManager/Assets`).

**Spec:** `docs/superpowers/specs/2026-06-10-cross-platform-spike-design.md`

**Testing note:** This is a spike — throwaway code whose deliverable is a findings log, per the spec. There is no test project and no TDD loop; every task instead ends with a run-and-observe verification step tied to the spec's success criteria. Do not skip those steps: the observations ARE the spike's output. Record every surprise (API differences, broken assumptions, workarounds) in `spikes/CrossPlatformSpike/README.md` under "Findings" as you hit them, not at the end.

**Licensing note (hard constraint):** Any package added beyond the four listed above must have an OSI-approved license, verified on nuget.org before adding.

---

### Task 1: Avalonia app scaffold

**Files:**
- Create: `spikes/CrossPlatformSpike/CrossPlatformSpike.csproj`
- Create: `spikes/CrossPlatformSpike/Program.cs`
- Create: `spikes/CrossPlatformSpike/App.axaml`
- Create: `spikes/CrossPlatformSpike/App.axaml.cs`
- Create: `spikes/CrossPlatformSpike/MainWindow.axaml`
- Create: `spikes/CrossPlatformSpike/MainWindow.axaml.cs`

- [ ] **Step 1: Check the latest Avalonia 12.x patch versions**

Run: `dotnet package search Avalonia --exact-match --take 1` and `dotnet package search Avalonia.Controls.WebView --exact-match --take 1`

Use the latest 12.x versions in the csproj below (12.0.4 / 12.0.1 were current at planning time). If `dotnet package search` is unavailable, keep the pinned versions — `dotnet build` will say if they don't resolve.

- [ ] **Step 2: Create the project files**

`spikes/CrossPlatformSpike/CrossPlatformSpike.csproj`:

```xml
<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <OutputType>WinExe</OutputType>
    <TargetFramework>net10.0</TargetFramework>
    <Nullable>enable</Nullable>
    <ImplicitUsings>enable</ImplicitUsings>
    <RootNamespace>CrossPlatformSpike</RootNamespace>
    <AvaloniaUseCompiledBindingsByDefault>true</AvaloniaUseCompiledBindingsByDefault>
  </PropertyGroup>

  <ItemGroup>
    <PackageReference Include="Avalonia" Version="12.0.4" />
    <PackageReference Include="Avalonia.Desktop" Version="12.0.4" />
    <PackageReference Include="Avalonia.Themes.Fluent" Version="12.0.4" />
    <PackageReference Include="Avalonia.Controls.WebView" Version="12.0.1" />
    <PackageReference Include="Porta.Pty" Version="1.0.7" />
  </ItemGroup>

  <ItemGroup>
    <Content Include="Assets\**">
      <CopyToOutputDirectory>PreserveNewest</CopyToOutputDirectory>
    </Content>
  </ItemGroup>

</Project>
```

Notes: `net10.0` (NOT `net10.0-windows`) is the point of the spike. `WinExe` suppresses the console window on Windows and is equivalent to `Exe` on Unix. The `Assets\**` glob means later tasks add assets without touching the csproj.

`spikes/CrossPlatformSpike/Program.cs`:

```csharp
using Avalonia;

namespace CrossPlatformSpike;

internal static class Program
{
    [STAThread]
    public static void Main(string[] args) => BuildAvaloniaApp()
        .StartWithClassicDesktopLifetime(args);

    public static AppBuilder BuildAvaloniaApp()
        => AppBuilder.Configure<App>()
            .UsePlatformDetect()
            .LogToTrace();
}
```

`spikes/CrossPlatformSpike/App.axaml`:

```xml
<Application xmlns="https://github.com/avaloniaui"
             xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
             x:Class="CrossPlatformSpike.App">
  <Application.Styles>
    <FluentTheme />
  </Application.Styles>
</Application>
```

`spikes/CrossPlatformSpike/App.axaml.cs`:

```csharp
using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Markup.Xaml;

namespace CrossPlatformSpike;

public class App : Application
{
    public override void Initialize() => AvaloniaXamlLoader.Load(this);

    public override void OnFrameworkInitializationCompleted()
    {
        if (ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop)
            desktop.MainWindow = new MainWindow();
        base.OnFrameworkInitializationCompleted();
    }
}
```

`spikes/CrossPlatformSpike/MainWindow.axaml` (placeholder body; Task 3 replaces the TextBlock with the web view):

```xml
<Window xmlns="https://github.com/avaloniaui"
        xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
        x:Class="CrossPlatformSpike.MainWindow"
        Title="CrossPlatformSpike" Width="1000" Height="640"
        Background="#1e1e2e">
  <DockPanel>
    <Border DockPanel.Dock="Bottom" Background="#181825" Padding="8,4">
      <TextBlock x:Name="StatusText" Foreground="#cdd6f4" FontSize="12"
                 Text="PTY: not started" />
    </Border>
    <TextBlock Text="terminal goes here" Foreground="#6c7086"
               HorizontalAlignment="Center" VerticalAlignment="Center" />
  </DockPanel>
</Window>
```

`spikes/CrossPlatformSpike/MainWindow.axaml.cs`:

```csharp
using Avalonia.Controls;

namespace CrossPlatformSpike;

public partial class MainWindow : Window
{
    public MainWindow()
    {
        InitializeComponent();
    }
}
```

- [ ] **Step 3: Build and run**

Run: `dotnet run --project spikes/CrossPlatformSpike/CrossPlatformSpike.csproj`

Expected: a dark (#1e1e2e) window titled "CrossPlatformSpike" opens with "terminal goes here" centered and a status bar reading "PTY: not started". Close it.

If the Avalonia package versions fail to restore, fix the pins (Step 1) before proceeding.

- [ ] **Step 4: Commit**

```bash
git add spikes/CrossPlatformSpike/
git commit -m "spike(xplat): Avalonia 12 app scaffold targeting net10.0"
```

---

### Task 2: Terminal page assets (xterm.js)

**Files:**
- Create: `spikes/CrossPlatformSpike/Assets/terminal.html`
- Create: `spikes/CrossPlatformSpike/Assets/spike-init.js`
- Create (copies): `spikes/CrossPlatformSpike/Assets/xterm.js`, `xterm.css`, `xterm-addon-fit.js`

- [ ] **Step 1: Copy the vendored xterm files from the main app**

```powershell
Copy-Item src/CodeShellManager/Assets/xterm.js,src/CodeShellManager/Assets/xterm.css,src/CodeShellManager/Assets/xterm-addon-fit.js spikes/CrossPlatformSpike/Assets/
```

- [ ] **Step 2: Write the stripped-down host page**

`spikes/CrossPlatformSpike/Assets/terminal.html` — no boot overlay, no drop overlay, no retro mode (all out of scope per spec):

```html
<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8" />
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  html, body { width: 100%; height: 100%; background: #1e1e2e; overflow: hidden; }
  #terminal { width: 100%; height: 100%; }
</style>
<link rel="stylesheet" href="xterm.css" />
</head>
<body>
<div id="terminal"></div>
<script src="xterm.js"></script>
<script src="xterm-addon-fit.js"></script>
<script src="spike-init.js"></script>
</body>
</html>
```

- [ ] **Step 3: Write the page script**

`spikes/CrossPlatformSpike/Assets/spike-init.js`. Differences from the main app's `terminal-init.js`: the message channel is NativeWebView's `invokeCSharpAction` (not `window.chrome.webview.postMessage`), PTY output arrives via a C#-invoked `window.ptyData(base64)` global (not a message event), and the font list covers macOS/Linux fallbacks per the spec.

```js
const term = new Terminal({
  cursorBlink: true,
  fontSize: 14,
  fontFamily: "'Cascadia Code', Menlo, 'DejaVu Sans Mono', Consolas, monospace",
  theme: { background: '#1e1e2e', foreground: '#cdd6f4', cursor: '#cdd6f4' },
  scrollback: 5000,
  allowProposedApi: true,
  customGlyphs: true,
});

const fitAddon = new FitAddon.FitAddon();
term.loadAddon(fitAddon);
term.open(document.getElementById('terminal'));
fitAddon.fit();

// invokeCSharpAction is injected by Avalonia's NativeWebView and raises
// WebMessageReceived on the C# side. Guarded so the page also works when
// opened in a plain browser for debugging.
function send(msg) {
  if (window.invokeCSharpAction) window.invokeCSharpAction(JSON.stringify(msg));
}

// C# pushes PTY output here as base64-encoded raw bytes. xterm.js accepts a
// Uint8Array and does its own UTF-8 decoding, so multi-byte characters split
// across chunk boundaries render correctly.
window.ptyData = (b64) => {
  const raw = atob(b64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  term.write(bytes);
};

term.onData(data => send({ type: 'input', data }));
term.onResize(({ cols, rows }) => send({ type: 'resize', cols, rows }));

new ResizeObserver(() => { try { fitAddon.fit(); } catch {} })
  .observe(document.getElementById('terminal'));

send({ type: 'ready', cols: term.cols, rows: term.rows });
term.focus();
```

- [ ] **Step 4: Sanity-check the page in a browser**

Run: `start spikes/CrossPlatformSpike/Assets/terminal.html` (opens default browser)

Expected: a dark page with a blinking xterm.js cursor block in the top-left. No console errors other than (possibly) none at all — the `send` guard means no `invokeCSharpAction` errors. Typing does nothing (no PTY) — that's correct.

- [ ] **Step 5: Commit**

```bash
git add spikes/CrossPlatformSpike/Assets/
git commit -m "spike(xplat): xterm.js terminal page with NativeWebView message contract"
```

---

### Task 3: SpikePty + SpikeBridge + wiring (the heart of the spike)

**Files:**
- Create: `spikes/CrossPlatformSpike/SpikePty.cs`
- Create: `spikes/CrossPlatformSpike/SpikeBridge.cs`
- Modify: `spikes/CrossPlatformSpike/MainWindow.axaml` (placeholder TextBlock → NativeWebView)
- Modify: `spikes/CrossPlatformSpike/MainWindow.axaml.cs` (create/dispose the bridge)

- [ ] **Step 1: Write SpikePty**

`spikes/CrossPlatformSpike/SpikePty.cs` — mirrors the shape of the main app's `IPseudoTerminal` (`DataReceived`, `Exited`, start/write/resize) so Phase 2 can slot it behind that interface:

```csharp
using System.Runtime.InteropServices;
using System.Text;
using Porta.Pty;

namespace CrossPlatformSpike;

/// <summary>Thin wrapper over Porta.Pty, shaped like the main app's IPseudoTerminal.</summary>
public sealed class SpikePty : IDisposable
{
    private IPtyConnection? _pty;
    private CancellationTokenSource? _readCts;

    public event Action<byte[]>? DataReceived;
    public event Action<int>? Exited;

    public int Pid => _pty?.Pid ?? -1;
    public string ShellPath { get; private set; } = "";

    public static string GetDefaultShell()
    {
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
            return FindOnPath("pwsh.exe") ?? Path.Combine(Environment.SystemDirectory, "cmd.exe");
        var shell = Environment.GetEnvironmentVariable("SHELL");
        return string.IsNullOrEmpty(shell) ? "/bin/bash" : shell;
    }

    private static string? FindOnPath(string exe)
    {
        foreach (var dir in (Environment.GetEnvironmentVariable("PATH") ?? "").Split(Path.PathSeparator))
        {
            try
            {
                var candidate = Path.Combine(dir.Trim(), exe);
                if (File.Exists(candidate)) return candidate;
            }
            catch { /* malformed PATH entry */ }
        }
        return null;
    }

    public async Task StartAsync(int cols, int rows)
    {
        ShellPath = GetDefaultShell();
        var env = new Dictionary<string, string>();
        // Unix TUIs read TERM for capabilities; xterm-256color matches xterm.js.
        if (!RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
            env["TERM"] = "xterm-256color";

        _pty = await PtyProvider.SpawnAsync(new PtyOptions
        {
            Name = "spike",
            App = ShellPath,
            Cols = cols,
            Rows = rows,
            Cwd = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            CommandLine = Array.Empty<string>(),
            Environment = env,
        }, CancellationToken.None);

        _pty.ProcessExited += (_, e) => Exited?.Invoke(e.ExitCode);

        _readCts = new CancellationTokenSource();
        _ = Task.Run(() => ReadLoopAsync(_pty, _readCts.Token));
    }

    private async Task ReadLoopAsync(IPtyConnection pty, CancellationToken ct)
    {
        var buffer = new byte[4096];
        try
        {
            while (!ct.IsCancellationRequested)
            {
                int n = await pty.ReaderStream.ReadAsync(buffer, ct);
                if (n == 0) break;
                DataReceived?.Invoke(buffer[..n]);
            }
        }
        catch (OperationCanceledException) { }
        catch (ObjectDisposedException) { }
    }

    public async Task WriteAsync(string data)
    {
        if (_pty is null) return;
        var bytes = Encoding.UTF8.GetBytes(data);
        await _pty.WriterStream.WriteAsync(bytes);
        await _pty.WriterStream.FlushAsync();
    }

    public void Resize(int cols, int rows) => _pty?.Resize(cols, rows);

    public void Dispose()
    {
        _readCts?.Cancel();
        try { _pty?.Kill(); } catch { /* already dead */ }
        _pty?.Dispose();
    }
}
```

API caveat: member names (`PtyProvider.SpawnAsync`, `IPtyConnection.ReaderStream/WriterStream/Resize/Kill/Pid/ProcessExited`, `PtyOptions.App/Cols/Rows/Cwd/CommandLine/Environment`) come from the Porta.Pty README. If compilation reveals differences, follow IntelliSense on the actual package and record the corrections in the README findings log.

- [ ] **Step 2: Write SpikeBridge**

`spikes/CrossPlatformSpike/SpikeBridge.cs`:

```csharp
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
        try
        {
            var msg = JsonDocument.Parse(e.Body).RootElement;
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
```

API caveats (record corrections in the findings log):
- `NativeWebView`, `WebMessageReceivedEventArgs`, and `e.Body` come from the Avalonia WebView docs; the exact namespaces may differ (`Avalonia.Controls` vs `Avalonia.WebView`) — let the IDE/compiler resolve them.
- If `e.Body` is not a string property, adapt (docs show it carrying the raw JS string).
- The base64 string contains only `[A-Za-z0-9+/=]`, so single-quoting it into the script is injection-safe.
- If `InvokeScript`-per-chunk turns out to be too slow (criterion 6), batching chunks on the C# side is the first fix to try — note it either way.

- [ ] **Step 3: Replace the placeholder with the web view**

In `spikes/CrossPlatformSpike/MainWindow.axaml`, replace

```xml
    <TextBlock Text="terminal goes here" Foreground="#6c7086"
               HorizontalAlignment="Center" VerticalAlignment="Center" />
```

with

```xml
    <NativeWebView x:Name="WebView" />
```

(If the default Avalonia xmlns doesn't resolve `NativeWebView`, add `xmlns:wv="using:Avalonia.Controls"` — adjusting to the control's real namespace — and use `<wv:NativeWebView>`.)

Replace `spikes/CrossPlatformSpike/MainWindow.axaml.cs` with:

```csharp
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
```

- [ ] **Step 4: Build, fixing API drift**

Run: `dotnet build spikes/CrossPlatformSpike/CrossPlatformSpike.csproj`

Expected: success, possibly after correcting Porta.Pty / NativeWebView member names per the caveats above. Every correction goes in the findings log (Task 4 creates the README; keep notes until then).

- [ ] **Step 5: Run and validate success criteria 1–4 on Windows**

Run: `dotnet run --project spikes/CrossPlatformSpike/CrossPlatformSpike.csproj`

Walk the spec's checklist:
1. xterm.js renders; status bar shows `PTY: running …pwsh.exe (pid …)`.
2. Type `dir` + Enter — output appears. Arrow-up recalls history. Ctrl+C interrupts a running `ping localhost -t`.
3. Colors: run `git -c color.status=always status` in a repo, or any TUI (`claude`). Box-drawing renders.
4. Resize the window — run `$Host.UI.RawUI.WindowSize` (pwsh) before/after; cols change.
5. Type `exit` — status bar shows `PTY: exited with code 0`.

- [ ] **Step 6: Commit**

```bash
git add spikes/CrossPlatformSpike/
git commit -m "spike(xplat): wire Porta.Pty to xterm.js through NativeWebView bridge"
```

---

### Task 4: README with success-criteria checklist + findings log

**Files:**
- Create: `spikes/CrossPlatformSpike/README.md`

- [ ] **Step 1: Write the README**

`spikes/CrossPlatformSpike/README.md` (fill the Windows column and Findings from Task 3's observations — the checkmarks below are placeholders to be edited to reality):

```markdown
# Cross-Platform Spike

Throwaway proof for [issue #32](https://github.com/umage-ai/CodeShellManager/issues/32) Phase 1:
**Avalonia 12 + NativeWebView (xterm.js) + Porta.Pty** as the cross-platform stack.
Spec: `docs/superpowers/specs/2026-06-10-cross-platform-spike-design.md`.

Run: `dotnet run --project spikes/CrossPlatformSpike/CrossPlatformSpike.csproj`

Linux needs WPE WebKit runtime libs: `libwpewebkit-2.0`, `libwpe-1.0`, `libWPEBackend-fdo-1.0`
(Debian/Ubuntu: `sudo apt install libwpewebkit-2.0-1 libwpe-1.0-1 libwpebackend-fdo-1.0-1` — exact
package names vary by distro/version; record what actually worked below.)

## Success criteria

| # | Criterion | Windows | WSLg (Linux) | macOS |
|---|---|---|---|---|
| 1 | Window opens, xterm.js renders | ☐ | ☐ | deferred |
| 2 | Interactive shell (typing, arrows, Ctrl+C) | ☐ | ☐ | deferred |
| 3 | ANSI colors + cursor addressing (TUI) | ☐ | ☐ | deferred |
| 4 | Resize propagates to the PTY | ☐ | ☐ | deferred |
| 5 | CI build green | ☐ | ☐ | ☐ |
| 6 | Type-echo latency acceptable | ☐ | ☐ | deferred |

## Findings

<!-- Append dated entries as discoveries happen. API corrections, workarounds,
     perf notes, anything Phase 2 needs to know. -->

## Verdict

<!-- Go / no-go recommendation for Phase 2, written when validation is done.
     Mirror it as a comment on issue #32. -->
```

- [ ] **Step 2: Fill in the Windows column and any findings collected during Task 3, then commit**

```bash
git add spikes/CrossPlatformSpike/README.md
git commit -m "spike(xplat): README with success-criteria checklist and findings log"
```

---

### Task 5: CI build matrix

**Files:**
- Create: `.github/workflows/spike-crossplatform.yml`

- [ ] **Step 1: Check the .NET setup convention in the existing workflow**

Read `.github/workflows/build.yml` and reuse its `actions/setup-dotnet` version/`dotnet-version` values in the workflow below if they differ.

- [ ] **Step 2: Write the workflow**

`.github/workflows/spike-crossplatform.yml`:

```yaml
name: spike-crossplatform

on:
  workflow_dispatch:
  push:
    paths:
      - 'spikes/CrossPlatformSpike/**'
      - '.github/workflows/spike-crossplatform.yml'

jobs:
  build:
    strategy:
      fail-fast: false
      matrix:
        os: [windows-latest, macos-latest, ubuntu-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-dotnet@v4
        with:
          dotnet-version: '10.0.x'
      - run: dotnet build spikes/CrossPlatformSpike/CrossPlatformSpike.csproj -c Release
```

Build-only by design — no packaging, no signing, no artifacts (spec: CI section).

- [ ] **Step 3: Commit and push, then watch the matrix**

```bash
git add .github/workflows/spike-crossplatform.yml
git commit -m "ci(spike): build-only matrix for the cross-platform spike"
git push -u origin feat/cross-platform-spike
gh run watch
```

Expected: all three jobs green (criterion 5). If macOS or Linux fail to compile, that is a spike finding — diagnose, fix if cheap, otherwise record in the README findings log and consult the spec's fallback ladder.

- [ ] **Step 4: Record criterion-5 results in the README and commit**

```bash
git add spikes/CrossPlatformSpike/README.md
git commit -m "spike(xplat): record CI matrix results"
```

---

### Task 6: Linux validation via WSLg

**Files:**
- Modify: `spikes/CrossPlatformSpike/README.md` (WSLg column + findings + verdict)

- [ ] **Step 1: Build and run inside WSL**

In a WSL shell (Ubuntu assumed; .NET 10 SDK must be installed in WSL — `dotnet --version` to check, install via Microsoft's apt feed if missing):

```bash
cd /mnt/c/Github/umage/CodeShellManager
sudo apt install libwpewebkit-2.0-1 libwpe-1.0-1 libwpebackend-fdo-1.0-1
dotnet run --project spikes/CrossPlatformSpike/CrossPlatformSpike.csproj
```

Package-name drift is expected (`apt search wpewebkit` to find the real ones); record the working set in the README. If WPE WebKit can't be satisfied, the docs offer a WebKitGTK fallback adapter — that switch is itself a finding.

Building under `/mnt/c` can be slow; if painful, `git clone` the repo into the WSL filesystem instead and work from there.

- [ ] **Step 2: Walk criteria 1–4 + 6 on WSLg**

Same checks as Task 3 Step 5, but the shell is `$SHELL` (bash/zsh): `ls --color`, arrow history, Ctrl+C on `ping 127.0.0.1`, resize check via `tput cols`, and a TUI (`htop` or `claude`).

- [ ] **Step 3: Fill the WSLg column, write the Verdict section, and commit**

The verdict is the spike's deliverable: go/no-go for Phase 2 (full Avalonia port), which layers passed as-is, which needed fallbacks, and open risks (macOS untested locally — CI build only).

```bash
git add spikes/CrossPlatformSpike/README.md
git commit -m "spike(xplat): WSLg validation results and Phase 2 verdict"
```

- [ ] **Step 4: Post the verdict to issue #32**

```bash
gh issue comment 32 --repo umage-ai/CodeShellManager --body-file - <<'EOF'
[paste the README's Verdict section here, plus a link to the branch]
EOF
```

---

## Self-review notes

- **Spec coverage:** project structure (T1–T3), stripped assets (T2), SpikePty (T3), SpikeBridge + message channel discovery (T3), status bar (T1/T3), success criteria checklist (T4), CI matrix (T5), WSLg validation + findings + verdict + issue comment (T6). macOS manual validation: out of scope per spec. ✓
- **Known unknowns are flagged, not hidden:** Porta.Pty and NativeWebView member names are README-sourced; Tasks 3's caveat blocks tell the executor to follow the compiler and log corrections — that discovery is the spike's purpose, not a plan defect.
- **No TDD:** deliberate, justified in the header (spike; manual success criteria are the verification).

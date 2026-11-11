using System;
using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;
using CodeShellManager.Models;
using CodeShellManager.Services;
using CodeShellManager.Terminal;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// A run command's PostRunUrl is almost always a dev server's address, and a server never
/// exits — so opening the URL only on exit 0 meant it never opened. It now also opens once a
/// loopback URL starts accepting connections, and still opens on exit 0 for one-shot commands.
/// </summary>
public class PostRunUrlReadinessTests
{
    private sealed class FakePty : IPseudoTerminal
    {
        public int? ExitCode { get; private set; }
#pragma warning disable CS0067 // never raised: these tests don't need output
        public event Action<string>? DataReceived;
#pragma warning restore CS0067
        public event Action? Exited;
        public void Start(string command, string args, string workingDirectory,
            int cols = 220, int rows = 50, bool useJobObject = false) { }
        public void Exit(int code) { ExitCode = code; Exited?.Invoke(); }
        public void Dispose() { }
    }

    private static readonly TimeSpan Wait = TimeSpan.FromSeconds(5);
    private static readonly TimeSpan Short = TimeSpan.FromMilliseconds(250);

    private static (RunInstance run, FakePty pty, BlockingCollection<string> opened) Make(
        string? url, Func<Uri, CancellationToken, Task<bool>>? probe = null)
    {
        var pty = new FakePty();
        var run = new RunInstance(
            new RunCommandItem { Id = "r", Label = "Run", CommandLine = "npm run dev", PostRunUrl = url },
            () => pty);
        var opened = new BlockingCollection<string>();
        run.UrlLauncher = opened.Add;
        run.ReadyPollInterval = TimeSpan.FromMilliseconds(10);
        if (probe != null) run.ReadyProbe = probe;
        return (run, pty, opened);
    }

    private static ShellSession Local() => new() { WorkingFolder = Environment.CurrentDirectory };

    [Fact]
    public void Server_OpensWhenListening_WithoutExiting()
    {
        int calls = 0;
        var (run, _, opened) = Make("http://localhost:5173",
            (_, _) => Task.FromResult(Interlocked.Increment(ref calls) >= 3));
        run.Start(Local());

        Assert.True(opened.TryTake(out var url, Wait));
        Assert.Equal("http://localhost:5173/", url);
        Assert.Equal(RunState.Running, run.State);
        run.Dispose();
    }

    [Fact]
    public void Server_ThenExitZero_OpensOnlyOnce()
    {
        var (run, pty, opened) = Make("http://127.0.0.1:4377", (_, _) => Task.FromResult(true));
        run.Start(Local());
        Assert.True(opened.TryTake(out _, Wait));
        pty.Exit(0);
        Assert.False(opened.TryTake(out _, Short));
    }

    [Fact]
    public void OneShot_ExitZero_Opens()
    {
        var (run, pty, opened) = Make("http://localhost:8000", (_, _) => Task.FromResult(false));
        run.Start(Local());
        pty.Exit(0);
        Assert.True(opened.TryTake(out _, Wait));
    }

    [Fact]
    public void ExitNonZero_BeforeReady_DoesNotOpen()
    {
        var (run, pty, opened) = Make("http://localhost:8000", (_, _) => Task.FromResult(false));
        run.Start(Local());
        pty.Exit(1);
        Assert.False(opened.TryTake(out _, Short));
    }

    [Fact]
    public void Stop_BeforeReady_DoesNotOpenLater()
    {
        int ready = 0;
        var (run, _, opened) = Make("http://localhost:8000",
            (_, _) => Task.FromResult(Volatile.Read(ref ready) == 1));
        run.Start(Local());
        run.Stop();
        Volatile.Write(ref ready, 1);
        Assert.False(opened.TryTake(out _, Short));
    }

    [Fact]
    public void RemoteUrl_IsNotProbed_OpensOnlyOnExitZero()
    {
        int calls = 0;
        var (run, pty, opened) = Make("https://example.com/deployed",
            (_, _) => { Interlocked.Increment(ref calls); return Task.FromResult(true); });
        run.Start(Local());
        Assert.False(opened.TryTake(out _, Short));
        Assert.Equal(0, Volatile.Read(ref calls));
        pty.Exit(0);
        Assert.True(opened.TryTake(out _, Wait));
    }

    [Fact]
    public void SshParent_IsNotProbed()
    {
        int calls = 0;
        var (run, _, opened) = Make("http://localhost:5173",
            (_, _) => { Interlocked.Increment(ref calls); return Task.FromResult(true); });
        run.Start(new ShellSession { Kind = SessionKind.Ssh, SshHost = "box", SshUser = "u" });
        Assert.False(opened.TryTake(out _, Short));
        Assert.Equal(0, Volatile.Read(ref calls));
        run.Dispose();
    }

    [Fact]
    public void NonHttpUrl_NeverProbedOrOpened()
    {
        int calls = 0;
        var (run, pty, opened) = Make(@"C:\Windows\System32\calc.exe",
            (_, _) => { Interlocked.Increment(ref calls); return Task.FromResult(true); });
        run.Start(Local());
        pty.Exit(0);
        Assert.False(opened.TryTake(out _, Short));
        Assert.Equal(0, Volatile.Read(ref calls));
    }

    [Theory]
    [InlineData("http://localhost:5173", true)]
    [InlineData("http://127.0.0.1:4377", true)]
    [InlineData("http://127.5.0.1/", true)]
    [InlineData("http://[::1]:8080/", true)]
    [InlineData("http://0.0.0.0:8000", true)]
    [InlineData("http://192.168.1.10:8000", false)]
    [InlineData("https://example.com", false)]
    [InlineData("http://localhost.example.com", false)]
    public void IsLoopbackHost(string url, bool expected)
        => Assert.Equal(expected, RunInstance.IsLoopbackHost(new Uri(url)));

    [Theory]
    [InlineData("127.0.0.1", "http://localhost:{0}")]   // IPv4-only server, reached via "localhost"
    [InlineData("::1", "http://localhost:{0}")]         // Vite on Node 17+ binds ::1 only
    [InlineData("127.0.0.1", "http://0.0.0.0:{0}")]
    public void DefaultProbe_DetectsRealListener(string bindAddress, string urlFormat)
    {
        var listener = new TcpListener(IPAddress.Parse(bindAddress), 0);
        listener.Start();
        try
        {
            int port = ((IPEndPoint)listener.LocalEndpoint).Port;
            var pty = new FakePty();
            var run = new RunInstance(new RunCommandItem
            {
                Id = "r", Label = "Run", CommandLine = "x",
                PostRunUrl = string.Format(urlFormat, port),
            }, () => pty);
            var opened = new BlockingCollection<string>();
            run.UrlLauncher = opened.Add;
            run.Start(Local());
            Assert.True(opened.TryTake(out _, Wait));
            run.Dispose();
        }
        finally { listener.Stop(); }
    }

    [Fact]
    public void DefaultProbe_NothingListening_DoesNotOpen()
    {
        // Grab a free port, then release it so nothing listens there.
        var l = new TcpListener(IPAddress.Loopback, 0);
        l.Start();
        int port = ((IPEndPoint)l.LocalEndpoint).Port;
        l.Stop();

        var pty = new FakePty();
        var run = new RunInstance(new RunCommandItem
        {
            Id = "r", Label = "Run", CommandLine = "x", PostRunUrl = $"http://127.0.0.1:{port}",
        }, () => pty);
        var opened = new BlockingCollection<string>();
        run.UrlLauncher = opened.Add;
        run.ReadyPollInterval = TimeSpan.FromMilliseconds(20);
        run.Start(Local());
        Assert.False(opened.TryTake(out _, TimeSpan.FromMilliseconds(600)));
        run.Dispose();
    }
}

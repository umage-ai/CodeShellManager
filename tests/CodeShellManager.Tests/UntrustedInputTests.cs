using System.Linq;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Regression tests for the second round of v0.8.0 pre-release security findings — three
/// places where a value from outside the app reached a shell or a launcher unescaped.
/// </summary>
public class UntrustedInputTests
{
    // ── ssh: the remote folder was the one value not escaped ─────────────────────────────

    [Theory]
    [InlineData("/home/alice/proj")]
    [InlineData("/home/alice/my proj")]
    [InlineData("/x'; id; :'")]        // closes the quote and runs `id` on the remote host
    [InlineData("/x'")]
    [InlineData("/it's/here")]         // legitimate path that also used to break the command
    public void SshRemoteFolder_IsSingleQuoteEscaped(string folder)
    {
        var session = new ShellSession
        {
            Kind = SessionKind.Ssh,
            SshHost = "example.invalid",
            SshRemoteFolder = folder,
            Command = "bash",
        };

        string args = session.BuildSshArgs();

        // The builder must route the folder through the escaper rather than interpolating
        // it raw. PosixSingleQuote's own correctness is pinned separately below with exact
        // expected strings — a quote-counting heuristic would be wrong here, since the
        // correct escaping of `/x'` is `'/x'\'''`, which contains an odd number of quotes.
        Assert.Contains("cd " + ShellSession.PosixSingleQuote(folder), args);

        // The raw-interpolation form must be absent — but only meaningfully so when the
        // folder actually contains a quote; without one the escaped and raw forms are the
        // same string, and asserting they differ would just be false.
        if (folder.Contains('\''))
            Assert.DoesNotContain($"cd '{folder}' &&", args);
    }

    [Theory]
    // A `"` is the escape that PosixSingleQuote does NOT handle, so before the outer
    // QuoteForCmd it terminated the hand-written Windows wrapper and everything after it
    // became separate ssh arguments. ssh honours options after the host, and ProxyCommand
    // runs LOCALLY — the same escalation as the SshHost bug, via a different field.
    [InlineData("/t\" -oProxyCommand=calc \"x")]
    [InlineData("/t\" -oPermitLocalCommand=yes \"x")]
    [InlineData("/plain")]
    public void TheWholeRemoteCommandStaysOneWindowsArgument(string folder)
    {
        var session = new ShellSession
        {
            Kind = SessionKind.Ssh,
            SshHost = "example.invalid",
            SshRemoteFolder = folder,
            Command = "bash",
        };

        string[] argv = Win32CommandLineTests.Split(session.BuildSshArgs());

        // ssh <-t> <host> <one remote command>. Anything more means the value escaped into
        // ssh's own option parsing.
        Assert.Equal(3, argv.Length);
        Assert.Equal("-t", argv[0]);
        Assert.Equal("example.invalid", argv[1]);
        Assert.StartsWith("cd ", argv[2]);
        Assert.DoesNotContain(argv, a => a.StartsWith("-o"));
    }

    [Theory]
    [InlineData("h -oProxyCommand=calc")]
    [InlineData("h\" -oProxyCommand=calc \"x")]
    public void AHostileHostStaysOneWindowsArgument(string host)
    {
        var session = new ShellSession
        {
            Kind = SessionKind.Ssh,
            SshHost = host,
            Command = "bash",
        };

        string[] argv = Win32CommandLineTests.Split(session.BuildSshArgs());

        Assert.Equal(3, argv.Length);
        Assert.Equal(host, argv[1]);
        Assert.DoesNotContain(argv, a => a.StartsWith("-o"));
    }

    [Theory]
    // The two escaping layers nest: POSIX single-quoting inside the remote command, Windows
    // argv quoting around the whole of it. These values exercise both at once — a quote for
    // the inner layer, a double-quote and a trailing backslash for the outer one, which is
    // where MSVCRT's 2n/2n+1 backslash rules bite.
    [InlineData("/it's/here")]
    [InlineData("/a\"b")]
    [InlineData("/a'b\"c")]
    [InlineData("/trailing\\")]
    [InlineData("/a'b\"c\\")]
    [InlineData("/x'; id; :'")]
    public void BothEscapingLayersNestCorrectly(string folder)
    {
        var session = new ShellSession
        {
            Kind = SessionKind.Ssh,
            SshHost = "example.invalid",
            SshRemoteFolder = folder,
            Command = "bash",
        };

        string[] argv = Win32CommandLineTests.Split(session.BuildSshArgs());

        // Outer layer: exactly three arguments reach ssh, whatever the value contains.
        Assert.Equal(3, argv.Length);
        Assert.Equal("example.invalid", argv[1]);

        // Inner layer: the remote command ssh receives carries the POSIX-escaped folder
        // verbatim. Its correctness against a real /bin/sh is verified separately — every
        // case here was round-tripped through `sh -c 'printf %s …'` in a WSL distro.
        Assert.Equal($"cd {ShellSession.PosixSingleQuote(folder)} && bash", argv[2]);
    }

    [Fact]
    public void PosixSingleQuote_EscapesEveryQuote()
    {
        Assert.Equal(@"'a'\''b'", ShellSession.PosixSingleQuote("a'b"));
        Assert.Equal("''", ShellSession.PosixSingleQuote(""));
        Assert.Equal(@"'/x'\''; id; :'\'''", ShellSession.PosixSingleQuote("/x'; id; :'"));
    }

    // ── PostRunUrl: validate and launch the SAME string ──────────────────────────────────

    [Theory]
    [InlineData("http://localhost:5173")]
    [InlineData("https://example.com/path?q=1")]
    public void LaunchableUrl_AcceptsHttpAndHttps(string url)
    {
        Assert.True(RunInstance.TryGetLaunchableUrl(url, out string? safe));
        Assert.NotNull(safe);
    }

    [Theory]
    [InlineData("file:///C:/Windows/System32/calc.exe")]
    [InlineData(@"\\attacker\share\evil.exe")]
    [InlineData("ms-settings:")]
    [InlineData("javascript:alert(1)")]
    [InlineData("localhost:5173")]     // scheme-less: Uri reads "localhost" as the scheme
    [InlineData("")]
    [InlineData(null)]
    public void LaunchableUrl_RejectsEverythingElse(string? url)
    {
        Assert.False(RunInstance.TryGetLaunchableUrl(url, out string? safe));
        Assert.Null(safe);
    }

    [Fact]
    public void LaunchableUrl_ReturnsTheNormalizedForm_NotTheRawString()
    {
        // The bug class: Uri.TryCreate accepts and internally escapes characters that the
        // raw string still contains, so validating one string and handing ShellExecute a
        // different one leaves whatever the Uri parser normalised away still live.
        Assert.True(RunInstance.TryGetLaunchableUrl("http://example.com/a b\"c", out string? safe));

        Assert.NotNull(safe);
        Assert.DoesNotContain(" ", safe!);
        Assert.DoesNotContain("\"", safe!);
        Assert.StartsWith("http://example.com/", safe!);
    }
}

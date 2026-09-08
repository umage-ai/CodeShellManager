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

        // The escaped form is '…' with every inner ' rendered as '\'' — so after the opening
        // quote, no bare ' can appear that would end the string early.
        Assert.Contains(ShellSession.PosixSingleQuote(folder), args);
        Assert.DoesNotContain($"cd '{folder}'", args.Replace(ShellSession.PosixSingleQuote(folder), ""));
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

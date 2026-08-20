using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// PostRunUrl is handed to ShellExecute automatically when a run exits 0 — no
/// confirmation step. ShellExecute launches whatever the string resolves to, so
/// anything that isn't an http(s) URL must be rejected before it reaches
/// Process.Start. See RunInstance.IsLaunchableUrl.
/// </summary>
public class PostRunUrlTests
{
    [Theory]
    [InlineData("http://localhost:5173")]
    [InlineData("https://example.com")]
    [InlineData("http://127.0.0.1:5000/health")]
    [InlineData("https://example.com/path?q=1&r=2#frag")]
    [InlineData("HTTPS://EXAMPLE.COM")]      // scheme comparison is case-insensitive
    public void IsLaunchableUrl_HttpAndHttps_Accepted(string url)
    {
        Assert.True(RunInstance.IsLaunchableUrl(url));
    }

    [Theory]
    [InlineData(@"C:\Windows\System32\calc.exe")]   // parses as scheme "c"
    [InlineData(@"C:\scripts\deploy.ps1")]
    [InlineData(@"\\server\share\payload.exe")]     // UNC → file scheme
    [InlineData("file:///C:/Windows/System32/calc.exe")]
    [InlineData("ftp://example.com/x")]
    [InlineData("javascript:alert(1)")]
    [InlineData("ms-settings:")]                    // registered protocol handler
    [InlineData("steam://run/440")]                 // third-party handler
    public void IsLaunchableUrl_NonHttpSchemes_Rejected(string url)
    {
        Assert.False(RunInstance.IsLaunchableUrl(url));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("localhost:5173")]   // scheme-less: parses as scheme "localhost"
    [InlineData("example.com")]      // scheme-less: not an absolute URI at all
    [InlineData("/relative/path")]
    public void IsLaunchableUrl_EmptyOrSchemeless_Rejected(string? url)
    {
        Assert.False(RunInstance.IsLaunchableUrl(url));
    }
}

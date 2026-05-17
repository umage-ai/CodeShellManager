using System.Text.Json;
using CodeShellManager.Models;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// State-file migration coverage. Legacy state.json predates the <see cref="SessionKind"/>
/// enum and only carried <c>IsRemote</c>; the deserializer must still produce a session
/// with the right <see cref="ShellSession.Kind"/>.
/// </summary>
public class ShellSessionMigrationTests
{
    [Fact]
    public void Deserialize_LegacyIsRemoteTrue_PromotesKindToSsh()
    {
        // Hand-rolled to match what an older app version would have written —
        // no `Kind` key, only `IsRemote`.
        const string legacy = """
            {
              "IsRemote": true,
              "SshUser": "alice",
              "SshHost": "dev.example.com",
              "SshPort": 22
            }
            """;
        var s = JsonSerializer.Deserialize<ShellSession>(legacy)!;
        Assert.Equal(SessionKind.Ssh, s.Kind);
        Assert.True(s.IsRemote);
        Assert.Equal("alice", s.SshUser);
    }

    [Fact]
    public void Deserialize_LegacyIsRemoteFalse_KeepsKindLocal()
    {
        const string legacy = """{ "IsRemote": false, "WorkingFolder": "C:\\proj" }""";
        var s = JsonSerializer.Deserialize<ShellSession>(legacy)!;
        Assert.Equal(SessionKind.Local, s.Kind);
        Assert.False(s.IsRemote);
    }

    [Fact]
    public void Deserialize_NewFormatWithKindWsl_LeavesIsRemoteFalse()
    {
        // StateService doesn't configure JsonStringEnumConverter, so enums round-trip
        // as integers. SessionKind.Wsl == 2.
        const string current = """
            {
              "Kind": 2,
              "WslDistro": "Ubuntu",
              "WslWorkingFolder": "/home/alice/proj"
            }
            """;
        var s = JsonSerializer.Deserialize<ShellSession>(current)!;
        Assert.Equal(SessionKind.Wsl, s.Kind);
        Assert.False(s.IsRemote);
        Assert.Equal("Ubuntu", s.WslDistro);
    }

    [Fact]
    public void Deserialize_BothKindAndLegacyIsRemote_KindWinsWhenKindIsWsl()
    {
        // Defensive: a file written by new code carries both IsRemote (computed, so false
        // for Wsl) and Kind. Verify the setter never demotes a Wsl Kind back to Ssh.
        const string mixed = """
            {
              "Kind": 2,
              "IsRemote": false,
              "WslDistro": "Ubuntu"
            }
            """;
        var s = JsonSerializer.Deserialize<ShellSession>(mixed)!;
        Assert.Equal(SessionKind.Wsl, s.Kind);
    }

    [Fact]
    public void Roundtrip_NewFormat_PreservesKind()
    {
        var original = new ShellSession
        {
            Kind = SessionKind.Wsl,
            WslDistro = "Debian",
            WslUser = "bob",
            WslWorkingFolder = "/srv/app",
        };
        string json = JsonSerializer.Serialize(original);
        var revived = JsonSerializer.Deserialize<ShellSession>(json)!;
        Assert.Equal(SessionKind.Wsl, revived.Kind);
        Assert.Equal("Debian", revived.WslDistro);
        Assert.Equal("bob", revived.WslUser);
        Assert.Equal("/srv/app", revived.WslWorkingFolder);
    }
}

using System.Text.Json;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// State-file migration coverage. Legacy state.json predates the <see cref="SessionKind"/>
/// enum and only carried <c>IsRemote</c>. Migration happens in
/// <see cref="StateService.Normalize"/> — the loader — not in a property setter, so the
/// in-memory <see cref="ShellSession.IsRemote"/> setter can stay a plain two-way switch.
/// </summary>
public class ShellSessionMigrationTests
{
    private static AppState LoadState(string json) =>
        StateService.Normalize(JsonSerializer.Deserialize<AppState>(json)!);

    [Fact]
    public void Normalize_LegacyIsRemoteTrue_PromotesKindToSsh()
    {
        const string legacy = """
            { "Sessions": [ { "IsRemote": true, "SshUser": "alice", "SshHost": "dev.example.com" } ] }
            """;
        var s = LoadState(legacy).Sessions[0];
        Assert.Equal(SessionKind.Ssh, s.Kind);
        Assert.True(s.IsRemote);
        // LegacyIsRemote is now a computed getter (Fix 4) — it doesn't "clear" after
        // migration the way a plain nullable field did. Once Kind is Ssh it correctly
        // reads true again; what matters is that migration actually happened (Kind).
        Assert.True(s.LegacyIsRemote);
    }

    [Fact]
    public void Normalize_LegacyIsRemoteFalse_KeepsKindLocal()
    {
        const string legacy = """{ "Sessions": [ { "IsRemote": false, "WorkingFolder": "C:\\proj" } ] }""";
        var s = LoadState(legacy).Sessions[0];
        Assert.Equal(SessionKind.Local, s.Kind);
        Assert.False(s.IsRemote);
    }

    [Fact]
    public void Normalize_KindWslWithStrayLegacyFalse_StaysWsl()
    {
        const string mixed = """{ "Sessions": [ { "Kind": 2, "IsRemote": false, "WslDistro": "Ubuntu" } ] }""";
        var s = LoadState(mixed).Sessions[0];
        Assert.Equal(SessionKind.Wsl, s.Kind);
    }

    [Fact]
    public void Normalize_KindWslWithStrayLegacyTrue_KindWins()
    {
        // Kind is authoritative once present; a stale IsRemote must not clobber it.
        const string mixed = """{ "Sessions": [ { "Kind": 2, "IsRemote": true, "WslDistro": "Ubuntu" } ] }""";
        var s = LoadState(mixed).Sessions[0];
        Assert.Equal(SessionKind.Wsl, s.Kind);
    }

    [Fact]
    public void Normalize_RecentlyClosedLegacyIsRemote_PromotesToSsh()
    {
        const string legacy = """{ "RecentlyClosed": [ { "IsRemote": true, "SshHost": "h" } ] }""";
        var e = LoadState(legacy).RecentlyClosed[0];
        Assert.Equal(SessionKind.Ssh, e.Kind);
        // Same computed-getter caveat as above — true again post-migration, not null.
        Assert.True(e.LegacyIsRemote);
    }

    [Fact]
    public void Serialize_DoesNotWriteLegacyIsRemoteOrComputedProperties()
    {
        // Fix 4: origin/main persists only "IsRemote" and has no Kind at all — an older
        // build reading a state.json this one wrote must still see an SSH session as
        // remote, so "IsRemote":true IS written for Ssh. Local/Wsl omit the key entirely.
        var s = new ShellSession { Kind = SessionKind.Ssh, SshHost = "h" };
        string json = JsonSerializer.Serialize(s);
        Assert.Contains("\"IsRemote\":true", json);
        Assert.DoesNotContain("FullCommandLine", json);
        Assert.DoesNotContain("FolderShort", json);
        Assert.DoesNotContain("AccentKey", json);
        Assert.Contains("\"Kind\":1", json);

        var local = new ShellSession { Kind = SessionKind.Local };
        Assert.DoesNotContain("\"IsRemote\"", JsonSerializer.Serialize(local));

        var wsl = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Ubuntu" };
        Assert.DoesNotContain("\"IsRemote\"", JsonSerializer.Serialize(wsl));
    }

    [Fact]
    public void Serialize_IncompleteSshSession_DoesNotThrow()
    {
        // Regression: FullCommandLine used to be serialised and BuildSshArgs threw on a
        // blank host, which made every state.json save fail after a bad edit.
        var s = new ShellSession { Kind = SessionKind.Ssh, SshHost = "" };
        string json = JsonSerializer.Serialize(s);
        Assert.NotNull(json);
        Assert.Equal("ssh", s.FullCommandLine);
    }

    [Fact]
    public void IsRemoteSetter_FalseOnSsh_DemotesToLocal()
    {
        var s = new ShellSession { Kind = SessionKind.Ssh };
        s.IsRemote = false;
        Assert.Equal(SessionKind.Local, s.Kind);
    }

    [Fact]
    public void IsRemoteSetter_FalseOnWsl_LeavesWsl()
    {
        var s = new ShellSession { Kind = SessionKind.Wsl };
        s.IsRemote = false;
        Assert.Equal(SessionKind.Wsl, s.Kind);
    }

    [Fact]
    public void Roundtrip_SshSession_ThroughComputedLegacyIsRemote_KindStaysSsh()
    {
        // serialize -> deserialize -> Normalize -> same Kind, going through the
        // computed LegacyIsRemote getter/setter split from Fix 4.
        var original = new ShellSession { Kind = SessionKind.Ssh, SshHost = "dev.example.com" };
        string sessionJson = JsonSerializer.Serialize(original);
        Assert.Contains("\"IsRemote\":true", sessionJson);

        string wrapped = "{ \"Sessions\": [ " + sessionJson + " ] }";
        var revived = LoadState(wrapped).Sessions[0];

        Assert.Equal(SessionKind.Ssh, revived.Kind);
        Assert.True(revived.IsRemote);
        Assert.Equal("dev.example.com", revived.SshHost);
    }

    [Fact]
    public void Roundtrip_NewFormat_PreservesKind()
    {
        var original = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "Debian", WslUser = "bob", WslWorkingFolder = "/srv/app",
        };
        string json = JsonSerializer.Serialize(original);
        var revived = JsonSerializer.Deserialize<ShellSession>(json)!;
        Assert.Equal(SessionKind.Wsl, revived.Kind);
        Assert.Equal("Debian", revived.WslDistro);
        Assert.Equal("bob", revived.WslUser);
        Assert.Equal("/srv/app", revived.WslWorkingFolder);
    }
}

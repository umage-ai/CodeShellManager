# WSL Sessions Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land PR #65 (first-class WSL sessions) on top of current `main`, with every confirmed review finding fixed, the "Edit session" feature taught about WSL, and the legacy `IsRemote` migration moved out of the property setter into the state loader.

**Architecture:** `ShellSession.Kind` (`Local | Ssh | Wsl`) is the single authority. `IsRemote` becomes a `[JsonIgnore]`d convenience over `Kind`; a JSON-only `LegacyIsRemote` field is read from old `state.json` files and folded into `Kind` by `StateService.Normalize` (the same loader hook that already backfills null collections). Edit/duplicate/reopen flows carry `Kind` plus the three WSL fields through `SessionConfigDraft`, `RecentlyClosedEntry`, and a new `snapshot_json` column in `session_history`. WSL command lines go through one MSVCRT-correct quoting helper.

**Tech Stack:** .NET 10 / WPF, System.Text.Json, Microsoft.Data.Sqlite, xunit 2.9.

**Spec:** the review findings recorded in the PR #65 review (this session, 2026-09-06) and the merge decisions in the conversation. Summarised in "Global Constraints" below.

## Global Constraints

- Branch: `feat/wsl-sessions-v2` in worktree `C:\Github\umage\CodeShellManager\.claude\worktrees\agent-a9ddd8b8`. Base commit `d4e6fd4` = PR #65 head merged with `origin/main` (658cf00). Never `git stash`; never touch other worktrees.
- Build: `dotnet build src/CodeShellManager/CodeShellManager.csproj -nologo -v q` must print `Build succeeded` with **no new warnings**.
- Tests: `dotnet test tests/CodeShellManager.Tests/ -nologo -v q` must pass with **zero failures** after every task (baseline 400 passing).
- Colours: Catppuccin Mocha hex literals only (see CLAUDE.md "Color / Theme").
- All `state.json` and import-file content is **untrusted** (CLAUDE.md, `RunInstance.IsLaunchableUrl` precedent). Nothing read from it may throw on the launch or save path, and everything that reaches a command line must be quoted by the shared helper.
- Every task ends with a commit. Commit message trailer (exactly):
  ```
  Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu
  ```
- Files are CRLF, UTF-8 with no BOM. Preserve that.
- Run `dotnet test` from the worktree root; it takes ~50 s.
- No Python on this machine. Use PowerShell or the Edit tool for file surgery.

---

### Task 1: Legacy `IsRemote` migration moves into the state loader

**Why:** PR #65 made `IsRemote`'s setter promote-only (`false` is ignored) so old JSON migrates. That silently broke `SessionConfigEditor.Apply` (`s.IsRemote = false` on an SSH session is a no-op → the session keeps `Kind=Ssh` with a blank host → `FullCommandLine` throws inside `BuildSshArgs` during the next `state.json` serialisation, and every later save fails). Migration belongs in the loader; the property becomes plain API sugar.

**Files:**
- Modify: `src/CodeShellManager/Models/ShellSession.cs` (lines 45-68, 106-112)
- Modify: `src/CodeShellManager/Models/RecentlyClosedEntry.cs` (lines 24-36, 78)
- Modify: `src/CodeShellManager/Services/StateService.cs` (`Normalize`, ~line 90)
- Modify: `src/CodeShellManager/MainWindow.xaml.cs` (`ReopenClosedSessionAsync`, lines 749-753)
- Modify: `tests/CodeShellManager.Tests/ShellSessionMigrationTests.cs` (rewrite)
- Modify: `tests/CodeShellManager.Tests/ShellSessionTests.cs` (`IsRemote_SetTrue_PromotesKindToSsh`, ~line 95)

**Interfaces:**
- Produces: `ShellSession.IsRemote { get; set; }` — `[JsonIgnore]`; getter `Kind == Ssh`; setter `true → Kind = Ssh`, `false → Kind = Local` only when `Kind == Ssh` (a WSL session stays WSL).
- Produces: `ShellSession.LegacyIsRemote : bool?` — JSON name `IsRemote`, `[JsonIgnore(Condition = WhenWritingNull)]`, never written after migration.
- Produces: `ShellSession.MigrateLegacyFields()` and `RecentlyClosedEntry.MigrateLegacyFields()` — idempotent, called by `StateService.Normalize`.
- Produces: `[JsonIgnore]` on `IsWsl`, `FullCommandLine`, `FolderShort`, `DefaultDisplayName`, `AccentKey` (ShellSession) and `IsRemote`, `Subtitle` (RecentlyClosedEntry). `FullCommandLine` no longer throws on incomplete sessions.

- [ ] **Step 1: Write the failing tests**

Replace the body of `tests/CodeShellManager.Tests/ShellSessionMigrationTests.cs` with:

```csharp
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
        Assert.Null(s.LegacyIsRemote);
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
        Assert.Null(e.LegacyIsRemote);
    }

    [Fact]
    public void Serialize_DoesNotWriteLegacyIsRemoteOrComputedProperties()
    {
        var s = new ShellSession { Kind = SessionKind.Ssh, SshHost = "h" };
        string json = JsonSerializer.Serialize(s);
        Assert.DoesNotContain("\"IsRemote\"", json);
        Assert.DoesNotContain("FullCommandLine", json);
        Assert.DoesNotContain("FolderShort", json);
        Assert.DoesNotContain("AccentKey", json);
        Assert.Contains("\"Kind\":1", json);
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
```

In `tests/CodeShellManager.Tests/ShellSessionTests.cs` replace the test `IsRemote_SetTrue_PromotesKindToSsh` with:

```csharp
    [Fact]
    public void IsRemote_SetTrue_SetsKindSsh()
    {
        var s = new ShellSession { IsRemote = true };
        Assert.Equal(SessionKind.Ssh, s.Kind);
        Assert.True(s.IsRemote);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `dotnet test tests/CodeShellManager.Tests/ -nologo -v q --filter "FullyQualifiedName~ShellSessionMigrationTests"`
Expected: compile error (`LegacyIsRemote` does not exist) — that counts as red.

- [ ] **Step 3: Implement in `ShellSession.cs`**

Add `using System.Text.Json.Serialization;` at the top. Replace the block from the `Kind` doc-comment through `IsWsl` (lines 44-68) with:

```csharp
    /// <summary>
    /// Authoritative session kind. Everything that branches on session type reads this.
    /// Legacy state.json files (pre-Kind) only carried <c>IsRemote</c>; see
    /// <see cref="LegacyIsRemote"/> and <see cref="MigrateLegacyFields"/>.
    /// </summary>
    public SessionKind Kind { get; set; } = SessionKind.Local;

    /// <summary>
    /// Convenience view of <see cref="Kind"/> for the SSH case. Setting <c>true</c> makes
    /// the session SSH; setting <c>false</c> on an SSH session makes it Local. It is
    /// deliberately NOT persisted — <see cref="Kind"/> is — and it carries no migration
    /// logic. A WSL session is unaffected by <c>IsRemote = false</c>.
    /// </summary>
    [JsonIgnore]
    public bool IsRemote
    {
        get => Kind == SessionKind.Ssh;
        set
        {
            if (value) Kind = SessionKind.Ssh;
            else if (Kind == SessionKind.Ssh) Kind = SessionKind.Local;
        }
    }

    /// <summary>
    /// Read-only compatibility slot for the pre-<see cref="Kind"/> <c>"IsRemote"</c> JSON
    /// key. Populated only when an old file is deserialised; <see cref="MigrateLegacyFields"/>
    /// folds it into <see cref="Kind"/> and nulls it so it is never written back.
    /// </summary>
    [JsonPropertyName("IsRemote")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? LegacyIsRemote { get; set; }

    /// <summary>
    /// Folds legacy JSON fields into their current representation. Idempotent. Called by
    /// <c>StateService.Normalize</c> for every loaded or imported session — the loader is
    /// the one place that knows it is looking at possibly-old data.
    /// </summary>
    public void MigrateLegacyFields()
    {
        if (LegacyIsRemote == true && Kind == SessionKind.Local) Kind = SessionKind.Ssh;
        LegacyIsRemote = null;
    }

    /// <summary>True iff this session runs inside a WSL distro via wsl.exe.</summary>
    [JsonIgnore]
    public bool IsWsl => Kind == SessionKind.Wsl;
```

Replace `FullCommandLine` (lines 106-112) with a non-throwing version and mark it ignored:

```csharp
    /// <summary>
    /// Full command line for display. Never throws: an incomplete session (blank SSH host,
    /// blank WSL distro) shows just the executable — this string is used in error dialogs
    /// on exactly those paths.
    /// </summary>
    [JsonIgnore]
    public string FullCommandLine => Kind switch
    {
        SessionKind.Ssh => string.IsNullOrWhiteSpace(SshHost) ? "ssh" : $"ssh {BuildSshArgs()}",
        SessionKind.Wsl => string.IsNullOrWhiteSpace(WslDistro) ? "wsl.exe" : $"wsl.exe {BuildWslArgs()}",
        _ => string.IsNullOrWhiteSpace(Args) ? Command : $"{Command} {Args}",
    };
```

Add `[JsonIgnore]` immediately above `FolderShort`, `DefaultDisplayName`, and `AccentKey`.

- [ ] **Step 4: Implement in `RecentlyClosedEntry.cs`**

Add `using System.Text.Json.Serialization;`. Replace the `Kind`/`IsRemote` block (lines 24-36) with:

```csharp
    /// <summary>
    /// Kind of the closed session, so a reopened WSL or SSH session comes back as the same
    /// kind instead of Local at a UNC. Legacy entries carried only <c>IsRemote</c>; see
    /// <see cref="LegacyIsRemote"/>.
    /// </summary>
    public SessionKind Kind { get; set; } = SessionKind.Local;

    [JsonIgnore]
    public bool IsRemote => Kind == SessionKind.Ssh;

    /// <summary>Legacy <c>"IsRemote"</c> JSON slot — see <see cref="ShellSession.LegacyIsRemote"/>.</summary>
    [JsonPropertyName("IsRemote")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? LegacyIsRemote { get; set; }

    public void MigrateLegacyFields()
    {
        if (LegacyIsRemote == true && Kind == SessionKind.Local) Kind = SessionKind.Ssh;
        LegacyIsRemote = null;
    }
```

Delete the line `IsRemote = s.IsRemote,` inside `FromSession`. Add `[JsonIgnore]` above `Subtitle`. Fix `tests/CodeShellManager.Tests/RecentlyClosedEntryTests.cs` object initialisers that use `IsRemote = true` → `Kind = SessionKind.Ssh` (the property is now get-only on the entry; the assertion `Assert.True(e.IsRemote)` still works).

- [ ] **Step 5: Implement in `StateService.Normalize`**

```csharp
    internal static AppState Normalize(AppState s)
    {
        s.Sessions ??= [];
        s.Groups ??= [];
        s.RecentlyClosed ??= [];
        s.GroupLayouts ??= new();
        s.Settings ??= new();
        // Legacy-field migration lives here, in the loader, so the models stay free of
        // deserialisation-order tricks. Import goes through this too (ImportExportService).
        foreach (var session in s.Sessions) session.MigrateLegacyFields();
        foreach (var entry in s.RecentlyClosed) entry.MigrateLegacyFields();
        return s;
    }
```

- [ ] **Step 6: Remove the setter-based migration in `MainWindow.ReopenClosedSessionAsync`**

Replace lines 749-753 (the two comments and the `if (entry.Kind == Models.SessionKind.Local) session.IsRemote = entry.IsRemote;` line) with just:

```csharp
        session.Kind = entry.Kind;
```

- [ ] **Step 7: Build, run the full suite**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj -nologo -v q` → `Build succeeded`, no new warnings.
Run: `dotnet test tests/CodeShellManager.Tests/ -nologo -v q` → all pass (expect ~408).

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor(sessions): migrate legacy IsRemote in the state loader, not a setter

The promote-only setter from #65 ignored false, so SessionConfigEditor.Apply
could never flip an SSH session back to Local, and the still-serialised
FullCommandLine then threw on every save. Kind is the only persisted field;
IsRemote is a [JsonIgnore]d two-way convenience; the legacy JSON key lands in
LegacyIsRemote and StateService.Normalize folds it into Kind.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 2: `SessionConfigDraft` / `SessionConfigEditor` learn `Kind` and the WSL fields

**Files:**
- Modify: `src/CodeShellManager/Models/SessionConfigDraft.cs`
- Modify: `src/CodeShellManager/Services/SessionConfigEditor.cs`
- Modify: `src/CodeShellManager/Views/NewSessionDialog.xaml.cs` (`ToDraft`, minimal compile fix — Task 3 finishes the dialog)
- Modify: `tests/CodeShellManager.Tests/SessionConfigEditorTests.cs` (fixtures at lines 18-26, usages of `d.IsRemote` at 98, 221, 276)

**Interfaces:**
- Consumes: `ShellSession.Kind`, `WslDiscoveryService.ToUncPath(distro, linuxPath)`.
- Produces: `SessionConfigDraft.Kind : SessionKind`, `WslDistro`, `WslUser`, `WslWorkingFolder : string`. `IsRemote` stays on the draft as `=> Kind == SessionKind.Ssh` (read-only) so existing call sites compile.
- Produces: `SessionConfigChange.WorkingFolderChanged` is also true when a WSL session's distro or Linux folder changed (git info must be re-resolved).
- Produces: `SessionConfigEditor.Apply` writes `s.Kind = d.Kind`; for WSL it derives `s.WorkingFolder = WslDiscoveryService.ToUncPath(d.WslDistro, d.WslWorkingFolder)` — the UNC-mirror invariant every WSL code path relies on.

- [ ] **Step 1: Write the failing tests**

Change the fixture in `SessionConfigEditorTests.cs` so `RemoteSession()` uses `Kind = SessionKind.Ssh` instead of `IsRemote = true`, and add a third fixture plus these tests (append inside the class):

```csharp
    private static ShellSession WslSession() => new()
    {
        Name = "ubuntu proj",
        Kind = SessionKind.Wsl,
        WslDistro = "Ubuntu",
        WslUser = "alice",
        WslWorkingFolder = "/home/alice/proj",
        WorkingFolder = @"\\wsl$\Ubuntu\home\alice\proj",
        Command = "claude",
    };

    [Fact]
    public void Apply_SshToLocal_DemotesKind()
    {
        // Regression: with the promote-only IsRemote setter this silently left Kind=Ssh.
        var s = RemoteSession();
        var d = SessionConfigDraft.FromSession(s);
        d.Kind = SessionKind.Local;
        d.WorkingFolder = @"C:\src";

        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
        SessionConfigEditor.Apply(s, d);

        Assert.Equal(SessionKind.Local, s.Kind);
        Assert.False(s.IsRemote);
        Assert.Equal(@"C:\src", s.WorkingFolder);
    }

    [Fact]
    public void Diff_WslIdentical_NoChange()
    {
        var s = WslSession();
        Assert.False(SessionConfigEditor.Diff(s, SessionConfigDraft.FromSession(s)).AnyChange);
    }

    [Fact]
    public void Diff_WslDistroChanged_RequiresRelaunchAndFolderChanged()
    {
        var s = WslSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WslDistro = "Debian";
        var c = SessionConfigEditor.Diff(s, d);
        Assert.True(c.AnyChange);
        Assert.True(c.RequiresRelaunch);
        Assert.True(c.WorkingFolderChanged);
    }

    [Fact]
    public void Diff_WslUserChanged_RequiresRelaunchButFolderUnchanged()
    {
        var s = WslSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WslUser = "root";
        var c = SessionConfigEditor.Diff(s, d);
        Assert.True(c.RequiresRelaunch);
        Assert.False(c.WorkingFolderChanged);
    }

    [Fact]
    public void Diff_WslLinuxFolderTrailingSlash_IsNotAChange()
    {
        var s = WslSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WslWorkingFolder = "/home/alice/proj/";
        Assert.False(SessionConfigEditor.Diff(s, d).AnyChange);
    }

    [Fact]
    public void Apply_WslFolderChanged_ResyncsUncWorkingFolder()
    {
        var s = WslSession();
        var d = SessionConfigDraft.FromSession(s);
        d.WslWorkingFolder = "/srv/other";
        SessionConfigEditor.Apply(s, d);
        Assert.Equal("/srv/other", s.WslWorkingFolder);
        Assert.Equal(@"\\wsl$\Ubuntu\srv\other", s.WorkingFolder);
    }

    [Fact]
    public void Apply_LocalToWsl_SetsKindAndUnc()
    {
        var s = LocalSession();
        var d = SessionConfigDraft.FromSession(s);
        d.Kind = SessionKind.Wsl;
        d.WslDistro = "Ubuntu";
        d.WslWorkingFolder = "/home/alice";
        Assert.True(SessionConfigEditor.Diff(s, d).RequiresRelaunch);
        SessionConfigEditor.Apply(s, d);
        Assert.Equal(SessionKind.Wsl, s.Kind);
        Assert.Equal(@"\\wsl$\Ubuntu\home\alice", s.WorkingFolder);
    }

    [Fact]
    public void Diff_StaleWslFieldsOnLocalSession_DoNotCount()
    {
        var s = LocalSession();
        s.WslDistro = "leftover";
        var d = SessionConfigDraft.FromSession(s);
        d.WslDistro = "";
        Assert.False(SessionConfigEditor.Diff(s, d).AnyChange);
    }
```

Update the three existing usages: `d.IsRemote = true;` → `d.Kind = SessionKind.Ssh;`, and the object initialiser at ~line 221 `IsRemote = false,` → `Kind = SessionKind.Local,`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `dotnet test tests/CodeShellManager.Tests/ -nologo -v q --filter "FullyQualifiedName~SessionConfigEditorTests"`
Expected: compile error on `d.Kind` / `WslDistro`.

- [ ] **Step 3: Implement `SessionConfigDraft`**

Replace the `// Remote` block and `FromSession` in `SessionConfigDraft.cs`:

```csharp
    // Kind + kind-specific fields. Kind is authoritative; IsRemote is a read-only view.
    public SessionKind Kind { get; set; } = SessionKind.Local;
    public bool IsRemote => Kind == SessionKind.Ssh;
    public string SshUser { get; set; } = "";
    public string SshHost { get; set; } = "";
    public int SshPort { get; set; } = 22;
    public string SshRemoteFolder { get; set; } = "";
    public string WslDistro { get; set; } = "";
    public string WslUser { get; set; } = "";
    public string WslWorkingFolder { get; set; } = "";
```

and in `FromSession` replace `IsRemote = s.IsRemote,` with:

```csharp
        Kind = s.Kind,
        WslDistro = s.WslDistro,
        WslUser = s.WslUser,
        WslWorkingFolder = s.WslWorkingFolder,
```

- [ ] **Step 4: Implement `SessionConfigEditor.Diff` / `Apply`**

In `Diff`, replace the `modeChanged`, `folderChanged`, `sshChanged` block with:

```csharp
        bool modeChanged = d.Kind != s.Kind;

        // Kind-specific fields only count while the kind is unchanged — a kind flip already
        // forces a relaunch, and leftovers from a previous kind must not read as edits.
        bool sameKind = !modeChanged;
        bool folderChanged = sameKind && s.Kind == SessionKind.Local
            && !PathsEqual(d.WorkingFolder, s.WorkingFolder);

        bool sshChanged = sameKind && s.Kind == SessionKind.Ssh
            && (!Eq(d.SshUser, s.SshUser)
                || !Eq(d.SshHost, s.SshHost)
                || d.SshPort != s.SshPort
                || !Eq(d.SshRemoteFolder, s.SshRemoteFolder));

        bool wslFolderChanged = sameKind && s.Kind == SessionKind.Wsl
            && (!Eq(d.WslDistro, s.WslDistro)
                || !LinuxPathsEqual(d.WslWorkingFolder, s.WslWorkingFolder));
        bool wslChanged = wslFolderChanged
            || (sameKind && s.Kind == SessionKind.Wsl && !Eq(d.WslUser, s.WslUser));
```

then include them: `anyChange = modeChanged || folderChanged || sshChanged || wslChanged || launchChanged || appearanceChanged || !Eq(d.Name, s.Name);`, `requiresRelaunch = modeChanged || folderChanged || sshChanged || wslChanged || launchChanged || transparencyChanged || overridesCleared;` and return `WorkingFolderChanged: folderChanged || wslFolderChanged`.

Add the helper next to `PathsEqual`:

```csharp
    /// <summary>Linux path compare: exact, trailing-slash tolerant, case-sensitive (ext4 is).</summary>
    internal static bool LinuxPathsEqual(string a, string b) =>
        string.Equals((a ?? "").Trim().TrimEnd('/'), (b ?? "").Trim().TrimEnd('/'), StringComparison.Ordinal);
```

In `Apply`, replace `s.IsRemote = d.IsRemote; s.WorkingFolder = d.WorkingFolder;` with:

```csharp
        s.Kind = d.Kind;
        s.WslDistro = d.WslDistro;
        s.WslUser = d.WslUser;
        s.WslWorkingFolder = d.WslWorkingFolder.Trim();
        // WSL sessions keep WorkingFolder as the \\wsl$ UNC mirror of the Linux path so
        // Explorer, git polling and the sidebar need no special-casing (see CLAUDE.md
        // "WSL Sessions"). Derive it here so the two can never drift apart.
        s.WorkingFolder = d.Kind == SessionKind.Wsl
            ? WslDiscoveryService.ToUncPath(d.WslDistro, s.WslWorkingFolder)
            : d.WorkingFolder;
```

Update the `<param name="WorkingFolderChanged">` doc to say "local folder or WSL distro/Linux folder moved".

- [ ] **Step 5: Build + full tests**

Run: `dotnet build src/CodeShellManager/CodeShellManager.csproj -nologo -v q` — expect a compile error in `NewSessionDialog.ToDraft` (`IsRemote = IsRemote` on a get-only property). Fix minimally now so the build passes: in `NewSessionDialog.ToDraft()` replace `IsRemote = IsRemote,` with

```csharp
        Kind = IsWsl ? SessionKind.Wsl : IsRemote ? SessionKind.Ssh : SessionKind.Local,
        WslDistro = WslDistro,
        WslUser = WslUser,
        WslWorkingFolder = WslWorkingFolder,
```

(`IsWsl`, `IsRemote`, `WslDistro`… here are the dialog's own output properties, set in `Start_Click`). Add `using CodeShellManager.Models;` if missing.
Run: `dotnet test tests/CodeShellManager.Tests/ -nologo -v q` → all pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(edit-session): carry Kind and WSL fields through SessionConfigDraft

Diff treats distro/user/Linux-folder like the SSH target fields; Apply sets
Kind directly and re-derives the UNC WorkingFolder so it can't drift from
WslWorkingFolder. Adds the SSH->Local regression test.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 3: "Edit session…" works for WSL sessions (dialog + MainWindow)

**Files:**
- Modify: `src/CodeShellManager/Views/NewSessionDialog.xaml.cs` (`_preselectWslDistro` ~line 88, ctor `Loaded` ~line 174-180, `PopulateWslDistrosAsync` ~line 187, `ForEdit` ~line 216, `ApplyEditMode` ~line 237, `SetUpEditModeProfileCombo` ~line 286, `SessionType_Changed` ~line 460)
- Modify: `src/CodeShellManager/MainWindow.xaml.cs` (`EditSessionAsync` lines 4574-4585; boot label line ~1330)

**Interfaces:**
- Consumes: `SessionConfigDraft.Kind/WslDistro/WslUser/WslWorkingFolder` (Task 2).
- Produces: `NewSessionDialog.ForEdit(session, …)` pre-fills WSL mode (distro pre-selected once the async list loads, user + Linux folder boxes filled). Profile/appearance panel is shown for Local **and** WSL sessions (xterm appearance is independent of what runs inside), hidden only for SSH — same rule in create and edit mode.

- [ ] **Step 1: Make the preselect field assignable**

Change `private readonly string _preselectWslDistro = "";` to `private string _preselectWslDistro = "";`.

- [ ] **Step 2: `ForEdit` passes the local folder only for Local sessions**

```csharp
            defaultFolder: session.Kind == SessionKind.Local ? session.WorkingFolder : "",
```

- [ ] **Step 3: `ApplyEditMode` handles all three kinds**

Replace the `if (s.IsRemote) { … }` block with:

```csharp
        switch (s.Kind)
        {
            case SessionKind.Ssh:
                // Checking the radio runs SessionType_Changed, which swaps the panels. It no
                // longer blanks NameBox — that handler returns early in edit mode — so the
                // assignment below is the only thing setting the name, not a repair.
                RemoteRadio.IsChecked = true;
                SshHostBox.Text = string.IsNullOrWhiteSpace(s.SshUser)
                    ? s.SshHost
                    : $"{s.SshUser}@{s.SshHost}";
                SshPortBox.Text = s.SshPort.ToString();
                SshRemoteFolderBox.Text = s.SshRemoteFolder;
                break;
            case SessionKind.Wsl:
                WslRadio.IsChecked = true;
                WslUserBox.Text = s.WslUser;
                WslWorkingFolderBox.Text = s.WslWorkingFolder;
                // The distro combo is filled asynchronously on Loaded; PopulateWslDistrosAsync
                // selects this name once the list arrives.
                _preselectWslDistro = s.WslDistro;
                break;
        }
```

- [ ] **Step 4: `Loaded` populates distros in edit mode too**

Replace the `Loaded += …` lambda body with:

```csharp
        Loaded += async (_, _) =>
        {
            // The distro list is needed in every mode the WSL radio can be reached from,
            // including edit mode — otherwise editing a WSL session shows an empty combo.
            await PopulateWslDistrosAsync();
            // Sibling-worktree fan-out only makes sense when creating sessions.
            if (IsEditMode) return;
            if (IsLocalMode && !string.IsNullOrWhiteSpace(FolderBox.Text))
                await ProbeSiblingWorktreesAsync(FolderBox.Text.Trim());
        };
```

In `PopulateWslDistrosAsync`, after the list is built (including the `distros.Count == 0` case), if `_preselectWslDistro` is non-empty and no item matched it, add `new ComboBoxItem { Content = $"{_preselectWslDistro}  (not installed)", Tag = _preselectWslDistro }` and select it, so editing a session whose distro was removed does not silently wipe the distro on Save. Keep the "No WSL distros found" hint when the list was empty.

- [ ] **Step 5: Profile panel rule — hide only for SSH**

In `SessionType_Changed`: `ProfilePanel.Visibility = IsRemoteMode ? Visibility.Collapsed : Visibility.Visible;` and update the comment to "Appearance overrides apply to any xterm-hosted session, WSL included; SSH is excluded because the remote profile is out of our hands." In `SetUpEditModeProfileCombo`: `ProfilePanel.Visibility = s.Kind == SessionKind.Ssh ? Visibility.Collapsed : Visibility.Visible;`.

- [ ] **Step 6: MainWindow edit flow compares Kind; boot label per kind**

In `EditSessionAsync` replace `bool wasRemote = session.IsRemote;` with `var wasKind = session.Kind;` and the condition with `if (change.WorkingFolderChanged || session.Kind != wasKind)`.

At line ~1330 replace the `bootLabel` expression with:

```csharp
        string bootLabel = session.Kind switch
        {
            Models.SessionKind.Ssh => $"Connecting to {session.SshHost}…",
            Models.SessionKind.Wsl => $"Starting {session.WslDistro}…",
            _ => $"Starting {(string.IsNullOrWhiteSpace(session.Command) ? "session" : session.Command)}…",
        };
```

- [ ] **Step 7: Build + full tests**

Run build and tests as in Global Constraints. Expected: `Build succeeded`, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(edit-session): edit WSL sessions in the shared New Session form

ForEdit pre-selects the distro and fills user/Linux folder, the distro list is
populated in edit mode, and the appearance panel is available for WSL sessions
(only SSH hides it). Boot label names the distro.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 4: One MSVCRT-correct quoting helper; one `BuildWslArgs`

**Why:** `QuoteForCmd` and both `BuildWslArgs` bodies escape `"` as `\"` but never double the backslashes that precede a quote (or the closing quote). Verified against `CommandLineToArgvW`: `sed -i 's/\"//g' f.txt` splits into stray argv; a command ending in `\` swallows the closing quote.

**Files:**
- Modify: `src/CodeShellManager/Models/ShellSession.cs` (`BuildWslArgs`, `QuoteForCmd`, lines ~140-175)
- Modify: `src/CodeShellManager/Services/RunInstance.cs` (`BuildWslArgs`, lines 262-280; `Start`, lines 75-128)
- Create: `tests/CodeShellManager.Tests/Win32CommandLineTests.cs`
- Modify: `tests/CodeShellManager.Tests/ShellSessionTests.cs`, `tests/CodeShellManager.Tests/RunInstanceTests.cs` (only if an existing expectation encoded the buggy shape — the plain-quote cases keep their current expected strings)

**Interfaces:**
- Produces: `ShellSession.QuoteForCmd(string value, bool force = false)` — MSVCRT rules: 2n backslashes before `"` → n literal backslashes + escaped quote; trailing backslashes doubled before the closing quote; `force` always wraps in quotes.
- Produces: `ShellSession.BuildWslArgs(string? inner = null)` — `inner` replaces the `Command + Args` payload (used for run commands). Throws `InvalidOperationException` on blank `WslDistro` (both callers now agree).
- Produces: `RunInstance.BuildWslArgs(parent, commandLine)` delegates to `parent.BuildWslArgs(commandLine)`.

- [ ] **Step 1: Write the failing round-trip tests**

Create `tests/CodeShellManager.Tests/Win32CommandLineTests.cs`:

```csharp
using System;
using System.Runtime.InteropServices;
using CodeShellManager.Models;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Round-trips our quoting through the real Win32 tokenizer. wsl.exe is started via
/// CreateProcess with no outer shell, so what CommandLineToArgvW produces is exactly what
/// wsl.exe (and then bash -lc) receives. Hand-written expectations were how the original
/// backslash bug slipped through.
/// </summary>
public class Win32CommandLineTests
{
    [DllImport("shell32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern IntPtr CommandLineToArgvW(string lpCmdLine, out int pNumArgs);

    [DllImport("kernel32.dll")]
    private static extern IntPtr LocalFree(IntPtr hMem);

    internal static string[] Split(string commandLine)
    {
        // Prefix a dummy program name: CommandLineToArgvW parses argv[0] with different rules.
        IntPtr argv = CommandLineToArgvW("x.exe " + commandLine, out int argc);
        if (argv == IntPtr.Zero) throw new InvalidOperationException("CommandLineToArgvW failed");
        try
        {
            var result = new string[argc - 1];
            for (int i = 1; i < argc; i++)
                result[i - 1] = Marshal.PtrToStringUni(Marshal.ReadIntPtr(argv, i * IntPtr.Size))!;
            return result;
        }
        finally { LocalFree(argv); }
    }

    [Theory]
    [InlineData("plain")]
    [InlineData("has space")]
    [InlineData("say \"hi\"")]
    [InlineData("trailing\\")]
    [InlineData("back\\\"slash-quote")]
    [InlineData("two\\\\\"bs")]
    [InlineData("sed -i 's/\\\"//g' f.txt")]
    [InlineData("grep -r \"\\\"foo\\\"\" .")]
    [InlineData("cp -r /src /dst\\")]
    [InlineData("")]
    public void QuoteForCmd_RoundTripsThroughCommandLineToArgvW(string value)
    {
        string[] argv = Split(ShellSession.QuoteForCmd(value, force: true));
        Assert.Single(argv);
        Assert.Equal(value, argv[0]);
    }

    [Theory]
    [InlineData("cargo test")]
    [InlineData("echo \"hi\"")]
    [InlineData("sed -i 's/\\\"//g' f.txt")]
    [InlineData("cp -r /src /dst\\")]
    [InlineData("printf '%s\\n' \"$HOME\"")]
    public void RunInstanceBuildWslArgs_BashPayloadArrivesIntact(string commandLine)
    {
        var p = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Ubuntu", WslWorkingFolder = "/home/a b" };
        string[] argv = Split(RunInstance.BuildWslArgs(p, commandLine));
        Assert.Equal(new[] { "-d", "Ubuntu", "--cd", "/home/a b", "--", "bash", "-lc", commandLine }, argv);
    }

    [Fact]
    public void ShellSessionBuildWslArgs_DistroWithSpaceAndUser_Tokenizes()
    {
        var s = new ShellSession
        {
            Kind = SessionKind.Wsl, WslDistro = "My Distro", WslUser = "alice",
            WslWorkingFolder = "/home/alice", Command = "claude", Args = "--prompt \"fix the \\\"foo\\\" bug\"",
        };
        string[] argv = Split(s.BuildWslArgs());
        Assert.Equal(new[] { "-d", "My Distro", "-u", "alice", "--cd", "/home/alice", "--", "bash", "-lc",
            "claude --prompt \"fix the \\\"foo\\\" bug\"" }, argv);
    }

    [Fact]
    public void RunInstanceBuildWslArgs_BlankDistro_Throws()
    {
        var p = new ShellSession { Kind = SessionKind.Wsl, WslDistro = "" };
        Assert.Throws<InvalidOperationException>(() => RunInstance.BuildWslArgs(p, "ls"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `dotnet test tests/CodeShellManager.Tests/ -nologo -v q --filter "FullyQualifiedName~Win32CommandLineTests"`
Expected: compile error (`force` parameter missing); after adding only the parameter, the `trailing\` / `sed` cases fail.

- [ ] **Step 3: Implement `QuoteForCmd` and unify `BuildWslArgs` in `ShellSession.cs`**

```csharp
    /// <summary>
    /// Win32 (MSVCRT / CommandLineToArgvW) argument quoting. Space-free, quote-free values
    /// are returned unchanged unless <paramref name="force"/> is set. Inside quotes, a
    /// run of n backslashes followed by <c>"</c> becomes 2n+1 backslashes + quote, and a
    /// trailing run of n backslashes becomes 2n so it cannot eat the closing quote.
    /// Every value that reaches wsl.exe goes through here — no ad-hoc Replace.
    /// </summary>
    internal static string QuoteForCmd(string value, bool force = false)
    {
        value ??= "";
        if (!force && value.Length > 0 && value.IndexOfAny(new[] { ' ', '\t', '"' }) < 0)
            return value;

        var sb = new StringBuilder(value.Length + 2);
        sb.Append('"');
        int backslashes = 0;
        foreach (char c in value)
        {
            if (c == '\\') { backslashes++; continue; }
            if (c == '"')
            {
                sb.Append('\\', backslashes * 2 + 1).Append('"');
                backslashes = 0;
                continue;
            }
            sb.Append('\\', backslashes).Append(c);
            backslashes = 0;
        }
        sb.Append('\\', backslashes * 2);
        sb.Append('"');
        return sb.ToString();
    }

    /// <summary>
    /// Builds the argument string passed to wsl.exe:
    /// <c>-d &lt;distro&gt; [-u &lt;user&gt;] [--cd &lt;linux-folder&gt;] -- bash -lc "&lt;payload&gt;"</c>.
    /// The payload is <see cref="Command"/> + <see cref="Args"/>, or <paramref name="inner"/>
    /// when given (run commands). It is wrapped in <c>bash -lc</c> so PATH-resolved tools
    /// (nvm node, pyenv, …) behave as in a login shell; bash then interprets the payload as
    /// a shell command line, which is the intent. Throws when <see cref="WslDistro"/> is
    /// blank — callers validate first (<c>LaunchValidationError</c>, Task 5).
    /// </summary>
    internal string BuildWslArgs(string? inner = null)
    {
        if (string.IsNullOrWhiteSpace(WslDistro))
            throw new InvalidOperationException("WslDistro must be set for WSL sessions.");
        var sb = new StringBuilder();
        sb.Append("-d ").Append(QuoteForCmd(WslDistro));
        if (!string.IsNullOrWhiteSpace(WslUser))
            sb.Append(" -u ").Append(QuoteForCmd(WslUser));
        if (!string.IsNullOrWhiteSpace(WslWorkingFolder))
            sb.Append(" --cd ").Append(QuoteForCmd(WslWorkingFolder));
        if (inner is null)
        {
            var shell = string.IsNullOrWhiteSpace(Command) ? "bash" : Command;
            inner = string.IsNullOrWhiteSpace(Args) ? shell : $"{shell} {Args}";
        }
        sb.Append(" -- bash -lc ").Append(QuoteForCmd(inner, force: true));
        return sb.ToString();
    }
```

- [ ] **Step 4: `RunInstance.BuildWslArgs` delegates; `Start` fails gracefully**

Replace the whole `RunInstance.BuildWslArgs` method with:

```csharp
    /// <summary>
    /// wsl.exe args for a run inside the parent's distro. One implementation with the
    /// session launcher — see <see cref="ShellSession.BuildWslArgs"/> — so the two can't
    /// disagree about quoting or about a blank distro.
    /// </summary>
    internal static string BuildWslArgs(ShellSession parent, string commandLine)
        => parent.BuildWslArgs(commandLine);
```

In `RunInstance.Start`, wrap the `switch (parent.Kind) { … }` and `_pty.Start(…)` in a `try`. In the `catch (Exception ex)`: append `$"Cannot start: {ex.Message}\r\n"` to the ANSI-stripped buffer (reuse whatever method already appends to `_ansiStripped` under `_bufLock` and raises `OutputChanged` — search the file; add a small private `AppendText(string)` if there is only inline code), set `ExitCode = -1`, `EndedAt = DateTime.Now`, `State` to the failed member of `RunState` (check the enum), raise `StateChanged`, detach the two PTY handlers, `_pty.Dispose()`, `_pty = null`, and return. A run that cannot even build its command line must show as a failed chip, not throw out of the toolbar click.

- [ ] **Step 5: Reconcile existing tests**

Run the full suite. If any pre-existing `BuildWslArgs`/`QuoteForCmd` expectations differ **only** in the buggy escaping shape, update them to the round-tripped output. Expectations that assert plain `"..."` wrapping (`-d Debian -- bash -lc "ls"`) still hold.

- [ ] **Step 6: Build + full tests → commit**

```bash
git add -A
git commit -m "fix(wsl): MSVCRT-correct quoting, one BuildWslArgs for sessions and runs

QuoteForCmd now doubles backslashes before quotes and at the end of a quoted
value; verified by round-tripping through CommandLineToArgvW. RunInstance
delegates to ShellSession.BuildWslArgs and reports a start failure in the run
output instead of throwing from the toolbar click.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 5: Validate a session before creating any UI for it

**Why:** `session.BuildWslArgs()` (and `BuildSshArgs()`) run at MainWindow.xaml.cs:1370-1379, *after* the WebView2 and terminal wrapper exist and *outside* the `try` at 1419. A `state.json`/import entry with `Kind: 2, WslDistro: ""` leaks a pane and a stuck launching placeholder on every restore.

**Files:**
- Modify: `src/CodeShellManager/Models/ShellSession.cs` (add `LaunchValidationError`)
- Modify: `src/CodeShellManager/MainWindow.xaml.cs` (`LaunchSessionAsync` top, ~line 1261)
- Modify: `tests/CodeShellManager.Tests/ShellSessionTests.cs`

**Interfaces:**
- Produces: `ShellSession.LaunchValidationError : string?` — `[JsonIgnore]`; null when launchable; otherwise a user-facing sentence.

- [ ] **Step 1: Tests**

Append to `ShellSessionTests`:

```csharp
    [Fact]
    public void LaunchValidationError_Local_IsNull() =>
        Assert.Null(new ShellSession { Kind = SessionKind.Local, Command = "claude" }.LaunchValidationError);

    [Fact]
    public void LaunchValidationError_SshBlankHost_Reports() =>
        Assert.Contains("host", new ShellSession { Kind = SessionKind.Ssh }.LaunchValidationError!, StringComparison.OrdinalIgnoreCase);

    [Fact]
    public void LaunchValidationError_WslBlankDistro_Reports() =>
        Assert.Contains("distro", new ShellSession { Kind = SessionKind.Wsl }.LaunchValidationError!, StringComparison.OrdinalIgnoreCase);

    [Fact]
    public void LaunchValidationError_WslWithDistro_IsNull() =>
        Assert.Null(new ShellSession { Kind = SessionKind.Wsl, WslDistro = "Ubuntu" }.LaunchValidationError);
```

- [ ] **Step 2: Run → fail (compile error).**

- [ ] **Step 3: Implement**

In `ShellSession.cs`, after `FullCommandLine`:

```csharp
    /// <summary>
    /// Null when the session has everything it needs to launch; otherwise a sentence for
    /// the user. Checked at the top of <c>MainWindow.LaunchSessionAsync</c> BEFORE any
    /// WebView2 or PTY is created, because the arg builders throw on these and a throw at
    /// that point leaks the pane. state.json and imports are untrusted input.
    /// </summary>
    [JsonIgnore]
    public string? LaunchValidationError => Kind switch
    {
        SessionKind.Ssh when string.IsNullOrWhiteSpace(SshHost) => "This SSH session has no host. Edit the session and set one.",
        SessionKind.Wsl when string.IsNullOrWhiteSpace(WslDistro) => "This WSL session has no distro. Edit the session and pick one.",
        _ => null,
    };
```

In `LaunchSessionAsync`, right after the opening `Log($"LaunchSession START…")` line, add:

```csharp
        if (session.LaunchValidationError is { } validationError)
        {
            Log($"LaunchSession REFUSED: {validationError}");
            MessageBox.Show(this, $"Cannot start '{session.Name}'.\n\n{validationError}",
                "Launch Error", MessageBoxButton.OK, MessageBoxImage.Warning);
            if (removeOnFailure) _sessionManager.RemoveSession(session.Id);
            else { session.IsDormant = true; AddDormantSidebarItem(session); }
            if (_launchingSidebarItems.Remove(session.Id)) RebuildSidebarOrder();
            return;
        }
```

Confirm `AddDormantSidebarItem(ShellSession)` exists with that signature (CLAUDE.md "Sleep / Wake"). With `removeOnFailure: true` (the default, used by restore) a broken restored session is dropped with the message above — the same outcome as a failed PTY start today.

- [ ] **Step 4: Build + full tests → commit**

```bash
git add -A
git commit -m "fix(launch): refuse unlaunchable sessions before creating a pane

A WSL session with a blank distro (or SSH with a blank host) used to throw from
BuildWslArgs after the WebView2 and wrapper existed and outside the failure
handler, leaking both. Validate first via ShellSession.LaunchValidationError.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 6: Git polling off the UI thread, with a WSL-aware cadence

**Why:** `GitService.GetGitInfoAsync`/`GetRepoRootAsync` start with a synchronous `Directory.Exists` on the caller's thread, and `SessionViewModel.RefreshGitInfoAsync` runs on the dispatcher (constructor + `PeriodicTimer` continuation). On a `\\wsl$` path with a stopped distro that call boots the VM and freezes the UI for seconds, at startup and every 10 s. Also, a WSL session in a non-repo folder spawns a third `wsl.exe` every tick because a null `RepoRoot` is re-probed forever.

**Files:**
- Modify: `src/CodeShellManager/ViewModels/SessionViewModel.cs` (lines 79-97, 111-120, `ReloadGitInfoAsync`)
- Create: `tests/CodeShellManager.Tests/SessionViewModelGitPollingTests.cs`

**Interfaces:**
- Produces: `SessionViewModel.GitPollIntervalFor(SessionKind) : TimeSpan` — `internal static`; 10 s Local, 30 s Wsl.
- Produces: negative RepoRoot cache for WSL only (`_repoRootProbedNegative`), reset by `ReloadGitInfoAsync`.

- [ ] **Step 1: Tests**

```csharp
using System;
using CodeShellManager.Models;
using CodeShellManager.ViewModels;
using Xunit;

namespace CodeShellManager.Tests;

public class SessionViewModelGitPollingTests
{
    [Fact]
    public void GitPollInterval_LocalIsTenSeconds() =>
        Assert.Equal(TimeSpan.FromSeconds(10), SessionViewModel.GitPollIntervalFor(SessionKind.Local));

    [Fact]
    public void GitPollInterval_WslIsSlower()
    {
        // Each WSL probe is a wsl.exe spawn (LxssManager hop) and keeps the VM awake;
        // three times the local cadence is the documented trade.
        Assert.Equal(TimeSpan.FromSeconds(30), SessionViewModel.GitPollIntervalFor(SessionKind.Wsl));
    }
}
```

- [ ] **Step 2: Run → compile failure.**

- [ ] **Step 3: Implement**

In `SessionViewModel.cs`:

```csharp
    /// <summary>
    /// Git poll cadence per kind. WSL probes spawn wsl.exe (much heavier than a local git
    /// spawn) and defeat WSL2's idle-VM shutdown, so they run a third as often.
    /// </summary>
    internal static TimeSpan GitPollIntervalFor(SessionKind kind) =>
        kind == SessionKind.Wsl ? TimeSpan.FromSeconds(30) : TimeSpan.FromSeconds(10);

    // WSL only: a "not a repo" answer costs a wsl.exe spawn per tick, so remember it.
    // Local folders keep re-probing (a `git init` should be picked up within a tick).
    private bool _repoRootProbedNegative;

    public async Task RefreshGitInfoAsync()
    {
        // SSH sessions have no local working folder to inspect. WSL sessions store
        // their WorkingFolder as a `\\wsl$\<distro>\...` UNC; GitService detects that
        // and dispatches to `wsl.exe -- git -C <linuxPath>` internally (Git for
        // Windows itself trips on those UNCs — dubious-ownership / .git symlinks).
        if (Session.Kind == SessionKind.Ssh || _gitOverriddenByOsc) return;

        // Off the dispatcher: GitService begins with a synchronous Directory.Exists, and on
        // a \\wsl$ share that boots a stopped distro (seconds). Continuations return to the
        // captured UI context, so the property sets below stay on the UI thread.
        string folder = Session.WorkingFolder;
        var (branch, isDirty) = await Task.Run(() => GitService.GetGitInfoAsync(folder));
        GitBranch = branch;
        GitIsDirty = isDirty;
        GitInfoLoaded = true;

        // RepoRoot is stable for the life of the session — resolve it once. Don't gate on
        // a non-empty branch: detached HEADs report no branch but are still valid repos
        // that should participate in sibling detection, shared accent color, and clusters.
        if (RepoRoot == null && !_repoRootProbedNegative)
        {
            RepoRoot = await Task.Run(() => GitService.GetRepoRootAsync(folder));
            if (RepoRoot == null && Session.Kind == SessionKind.Wsl) _repoRootProbedNegative = true;
        }
    }
```

In `PollGitInfoAsync`: `using var timer = new PeriodicTimer(GitPollIntervalFor(Session.Kind));`.

In `ReloadGitInfoAsync` (it already clears `RepoRoot` and `_gitOverriddenByOsc`), also set `_repoRootProbedNegative = false;`.

- [ ] **Step 4: Build + full tests → commit**

```bash
git add -A
git commit -m "perf(git): probe off the UI thread; slower cadence and negative cache for WSL

Directory.Exists on a wsl share boots a stopped distro and was running on the
dispatcher at startup and every tick. WSL sessions now poll every 30s and
remember a not-a-repo answer instead of spawning wsl.exe for it forever.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 7: One UNC parser; distro-name boundary in the arg translator

**Why:** `TranslateUncArgsToLinux`'s regex has no boundary after the distro name, so with distro `Ubuntu` the UNC `\\wsl$\Ubuntu-22.04\home\alice\x` is rewritten to `/-22.04\home\alice\x` — and `git worktree add` inside Ubuntu then creates that directory. Separately, `NewSessionDialog.ParseWslUncPath` and `GitService.TryParseWslUnc` are two parsers of the same shape with different root conventions.

**Files:**
- Modify: `src/CodeShellManager/Services/WslDiscoveryService.cs` (add `TryParseUncPath`)
- Modify: `src/CodeShellManager/Services/GitService.cs` (`TryParseWslUnc` delegates; `TranslateUncArgsToLinux` boundary)
- Modify: `src/CodeShellManager/Views/NewSessionDialog.xaml.cs` (`ParseWslUncPath` delegates)
- Modify: `tests/CodeShellManager.Tests/GitServiceWslRoutingTests.cs`, `tests/CodeShellManager.Tests/WslDiscoveryServiceTests.cs`

**Interfaces:**
- Produces: `WslDiscoveryService.TryParseUncPath(string path) : (string? distro, string linuxPath)` — `linuxPath` is `"/"` for the distro root, `""` when not a WSL UNC. Accepts `\\wsl$\` and `\\wsl.localhost\`, either slash direction, case-insensitive prefix.
- `GitService.TryParseWslUnc(path)` becomes `=> WslDiscoveryService.TryParseUncPath(path)`.
- `NewSessionDialog.ParseWslUncPath(unc)` becomes a thin wrapper that maps `null → ""` and `"/" → ""` (dialog convention: blank Linux folder = home). `NewSessionDialogTests` must keep passing unchanged.

- [ ] **Step 1: Tests**

Add to `GitServiceWslRoutingTests`:

```csharp
    [Fact]
    public void TranslateUncArgsToLinux_PrefixCollidingDistro_LeftAlone()
    {
        // `Ubuntu` must not match `Ubuntu-22.04` — the default `wsl --install` naming.
        string args = "worktree add \"\\\\wsl$\\Ubuntu-22.04\\home\\alice\\x\" main";
        Assert.Equal(args, GitService.TranslateUncArgsToLinux(args, "Ubuntu"));
        string bare = "worktree add \\\\wsl$\\Ubuntu-22.04\\home\\alice\\x main";
        Assert.Equal(bare, GitService.TranslateUncArgsToLinux(bare, "Ubuntu"));
    }

    [Fact]
    public void TranslateUncArgsToLinux_DistroRootUnquoted_BecomesSlash()
    {
        Assert.Equal("-C / status", GitService.TranslateUncArgsToLinux("-C \\\\wsl$\\Ubuntu status", "Ubuntu"));
    }
```

Add to `WslDiscoveryServiceTests`:

```csharp
    [Theory]
    [InlineData(@"\\wsl$\Ubuntu\home\alice", "Ubuntu", "/home/alice")]
    [InlineData(@"\\wsl.localhost\Debian\srv\app", "Debian", "/srv/app")]
    [InlineData(@"\\WSL$\Ubuntu\", "Ubuntu", "/")]
    [InlineData(@"//wsl$/Ubuntu/home/alice", "Ubuntu", "/home/alice")]
    [InlineData(@"\\wsl$\Ubuntu", "Ubuntu", "/")]
    [InlineData(@"\\wsl$\", null, "")]
    [InlineData(@"C:\proj", null, "")]
    [InlineData("", null, "")]
    public void TryParseUncPath_KnownShapes(string path, string? distro, string linux)
    {
        var (d, l) = WslDiscoveryService.TryParseUncPath(path);
        Assert.Equal(distro, d);
        Assert.Equal(linux, l);
    }
```

- [ ] **Step 2: Run → the two new GitService tests fail; the discovery tests fail to compile.**

- [ ] **Step 3: Implement**

In `WslDiscoveryService.cs` add:

```csharp
    /// <summary>
    /// Splits a WSL UNC (<c>\\wsl$\Ubuntu\home\alice</c> or <c>\\wsl.localhost\…</c>, either
    /// slash direction) into (distro, linuxPath). linuxPath is "/" for the distro root.
    /// Returns (null, "") for anything that isn't a WSL UNC. The single parser for the
    /// whole app — GitService and NewSessionDialog both delegate here.
    /// </summary>
    public static (string? distro, string linuxPath) TryParseUncPath(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return (null, "");
        string normalized = path.Replace('/', '\\').TrimEnd('\\');
        foreach (var prefix in new[] { @"\\wsl$\", @"\\wsl.localhost\" })
        {
            if (!normalized.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) continue;
            string rest = normalized[prefix.Length..];
            if (string.IsNullOrEmpty(rest)) return (null, "");
            int slash = rest.IndexOf('\\');
            string distro = slash < 0 ? rest : rest[..slash];
            string linuxRest = slash < 0 ? "" : rest[(slash + 1)..];
            return (distro, string.IsNullOrEmpty(linuxRest) ? "/" : "/" + linuxRest.Replace('\\', '/'));
        }
        return (null, "");
    }
```

In `GitService.cs` replace the body of `TryParseWslUnc` with `=> WslDiscoveryService.TryParseUncPath(path);` (keep the `internal static` signature). In `TranslateUncArgsToLinux` change the `body` line to:

```csharp
        // Lookahead: the distro name must be followed by a separator, a quote, whitespace or
        // end-of-string — otherwise `Ubuntu` also matches `Ubuntu-22.04`.
        string body = $@"\\\\wsl(?:\$|\.localhost)\\{esc}(?=[\\""\s]|$)";
```

In `NewSessionDialog.xaml.cs` replace the body of `ParseWslUncPath` with:

```csharp
        var (distro, linux) = WslDiscoveryService.TryParseUncPath(unc);
        // Dialog convention: blank Linux folder means "the user's home", so the distro root
        // ("/") from the shared parser is reported as "" here.
        return distro is null ? ("", "") : (distro, linux == "/" ? "" : linux);
```

- [ ] **Step 4: Build + full tests → commit**

```bash
git add -A
git commit -m "fix(wsl): one UNC parser, and a distro-name boundary in the arg translator

Ubuntu no longer matches Ubuntu-22.04 when translating wsl UNC args for git;
GitService and NewSessionDialog share WslDiscoveryService.TryParseUncPath.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 8: Relaunch-from-search preserves the session kind

**Why:** `TryRelaunchFromHistoryAsync` recreates a session from `session_history`, which holds seven kind-agnostic columns. A closed WSL (or SSH) session relaunched from a search hit comes back Local at the UNC and runs the Windows `claude`. `RecentlyClosedEntry` already carries everything needed — store it alongside.

**Files:**
- Modify: `src/CodeShellManager/Services/SearchService.cs` (schema ~line 84-95, `SessionHistoryEntry` record line 22, `RecordSessionHistoryAsync`, both `GetSessionHistory*` readers)
- Modify: `src/CodeShellManager/MainWindow.xaml.cs` (`pty.Exited` handler ~line 1347; `TryRelaunchFromHistoryAsync` ~line 5171)
- Modify: `tests/CodeShellManager.Tests/SearchServiceTests.cs`

**Interfaces:**
- Produces: column `session_history.snapshot_json TEXT NULL`, added by `InitializeSchemaAsync` via `ALTER TABLE` when `PRAGMA table_info(session_history)` lacks it.
- Produces: `SessionHistoryEntry` gains a trailing `string? SnapshotJson = null` positional parameter.
- Produces: `RecordSessionHistoryAsync(..., string groupId, string? snapshotJson = null)`.

- [ ] **Step 1: Tests**

Append to `SearchServiceTests`:

```csharp
    [Fact]
    public async Task SessionHistory_RoundTripsSnapshotJson()
    {
        await _svc.RecordSessionHistoryAsync("sid-1", "n", @"\\wsl$\Ubuntu\home\a", "claude", "", "", "{\"Kind\":2}");
        var e = await _svc.GetSessionHistoryAsync("sid-1");
        Assert.NotNull(e);
        Assert.Equal("{\"Kind\":2}", e!.SnapshotJson);
    }

    [Fact]
    public async Task SessionHistory_WithoutSnapshot_ReadsNull()
    {
        await _svc.RecordSessionHistoryAsync("sid-2", "n", @"C:\p", "claude", "", "");
        var e = await _svc.GetLatestSessionHistoryForFolderAsync(@"C:\p");
        Assert.Null(e!.SnapshotJson);
    }

    [Fact]
    public async Task InitializeSchema_UpgradesPreSnapshotTable()
    {
        // Simulate a database created by the previous release: same table without the column.
        string path = Path.Combine(Path.GetTempPath(), $"csm-hist-{Guid.NewGuid():N}.db");
        var db = new SqliteConnection($"Data Source={path}");
        db.Open();
        try
        {
            await using (var cmd = db.CreateCommand())
            {
                cmd.CommandText = """
                    CREATE TABLE session_history (
                        id INTEGER PRIMARY KEY, session_id TEXT NOT NULL, session_name TEXT NOT NULL,
                        working_folder TEXT NOT NULL, command TEXT NOT NULL, args TEXT NOT NULL DEFAULT '',
                        group_id TEXT NOT NULL DEFAULT '', exited_at INTEGER NOT NULL);
                    INSERT INTO session_history (session_id, session_name, working_folder, command, exited_at)
                    VALUES ('old', 'o', 'C:\x', 'bash', 1);
                    """;
                await cmd.ExecuteNonQueryAsync();
            }
            await SearchService.InitializeSchemaAsync(db);
            await SearchService.InitializeSchemaAsync(db); // idempotent
            var svc = new SearchService(db);
            var e = await svc.GetSessionHistoryAsync("old");
            Assert.NotNull(e);
            Assert.Null(e!.SnapshotJson);
        }
        finally
        {
            db.Close(); db.Dispose(); SqliteConnection.ClearAllPools();
            try { File.Delete(path); } catch { }
        }
    }
```

- [ ] **Step 2: Run → compile error (`SnapshotJson`).**

- [ ] **Step 3: Implement `SearchService`**

Record: `public record SessionHistoryEntry(string SessionId, string SessionName, string WorkingFolder, string Command, string Args, string GroupId, DateTime ExitedAt, string? SnapshotJson = null);`

Add `snapshot_json TEXT NULL` to the `CREATE TABLE IF NOT EXISTS session_history` DDL (fresh databases). After the big DDL `ExecuteNonQueryAsync`, add a guarded upgrade for existing databases:

```csharp
        // Column added after the first release of session_history; CREATE TABLE IF NOT
        // EXISTS won't touch an existing table, so upgrade explicitly. Idempotent.
        bool hasSnapshot = false;
        await using (var probe = db.CreateCommand())
        {
            probe.CommandText = "PRAGMA table_info(session_history)";
            await using var r = await probe.ExecuteReaderAsync();
            while (await r.ReadAsync())
                if (string.Equals(r.GetString(1), "snapshot_json", StringComparison.OrdinalIgnoreCase)) hasSnapshot = true;
        }
        if (!hasSnapshot)
        {
            await using var alter = db.CreateCommand();
            alter.CommandText = "ALTER TABLE session_history ADD COLUMN snapshot_json TEXT NULL";
            await alter.ExecuteNonQueryAsync();
        }
```

`RecordSessionHistoryAsync`: add parameter `string? snapshotJson = null`, add `snapshot_json` to the column list and `$snap` to VALUES; `cmd.Parameters.AddWithValue("$snap", (object?)snapshotJson ?? DBNull.Value);`.

Both readers: select `snapshot_json` as the 8th column and pass `r.IsDBNull(7) ? null : r.GetString(7)`.

- [ ] **Step 4: Implement `MainWindow`**

In the `pty.Exited` handler (~line 1347) pass the snapshot:

```csharp
                _ = _searchService.RecordSessionHistoryAsync(
                    session.Id, session.Name, session.WorkingFolder,
                    session.Command, session.Args, session.GroupId,
                    System.Text.Json.JsonSerializer.Serialize(Models.RecentlyClosedEntry.FromSession(session)));
```

In `TryRelaunchFromHistoryAsync`, replace the final three lines (`CreateSession … SeedRunCommandsAsync … LaunchSessionAsync`) with:

```csharp
        // Prefer the full snapshot: it carries Kind and the SSH/WSL fields, so a WSL session
        // relaunches as WSL instead of Local-at-a-UNC. Rows from before the column exist
        // without one and fall back to the kind-agnostic columns.
        Models.RecentlyClosedEntry? snapshot = null;
        if (!string.IsNullOrEmpty(entry.SnapshotJson))
        {
            try { snapshot = System.Text.Json.JsonSerializer.Deserialize<Models.RecentlyClosedEntry>(entry.SnapshotJson); }
            catch (System.Text.Json.JsonException ex) { Log($"History snapshot unreadable for '{entry.SessionId}': {ex.Message}"); }
        }
        if (snapshot != null)
        {
            snapshot.MigrateLegacyFields();
            await ReopenClosedSessionAsync(snapshot);
            return;
        }

        var newSession = _sessionManager.CreateSession(
            entry.SessionName, entry.WorkingFolder, entry.Command, entry.Args, entry.GroupId);
        SeedRunCommandsAsync(newSession);
        await LaunchSessionAsync(newSession);
```

- [ ] **Step 5: Build + full tests → commit**

```bash
git add -A
git commit -m "fix(search): relaunching a closed session from a search hit keeps its kind

session_history gains a snapshot_json column holding the RecentlyClosedEntry;
relaunch goes through ReopenClosedSessionAsync so WSL/SSH sessions come back
as themselves. Existing databases are upgraded in InitializeSchemaAsync.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 9: New Session dialog — no re-entrancy during the WSL home probe

**Why:** `Start_Click` is `async void` and awaits a wsl.exe spawn (up to 3 s) with the default button still enabled. A second Enter runs a second probe and then sets `DialogResult` on a closed window (InvalidOperationException, logged as UNHANDLED). `BrowseWslFolder_Click` can pop its picker after the dialog closed.

**Files:**
- Modify: `src/CodeShellManager/Views/NewSessionDialog.xaml.cs` (`Start_Click`, `BrowseWslFolder_Click`, constructor)

- [ ] **Step 1: Add state + Closed hook**

Fields: `private bool _submitting; private bool _closed;` In the constructor after `InitializeComponent();`: `Closed += (_, _) => _closed = true;`

- [ ] **Step 2: Guard `Start_Click`**

Wrap the body: at the top `if (_submitting) return; _submitting = true; OkButton.IsEnabled = false; try { …existing body… } finally { if (!_closed) { _submitting = false; OkButton.IsEnabled = true; } }`. Immediately after the `await WslDiscoveryService.GetDistroHomeAsync(...)` line add `if (_closed) return;`. Every early `return` inside the body (validation failures) now exits through `finally`, which re-enables the button — that is the desired behaviour.

- [ ] **Step 3: Guard `BrowseWslFolder_Click`**

After `string seed = await ComputeWslBrowseSeedAsync(...)` add `if (_closed) return;`.

- [ ] **Step 4: Build + full tests → commit**

```bash
git add -A
git commit -m "fix(new-session): no double submit while the WSL home probe runs

Start_Click disables the primary button for the duration and bails out if the
window closed during the await, so a second Enter or a Cancel can't set
DialogResult on a closed window.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 10: Documentation — CLAUDE.md

**Files:**
- Modify: `CLAUDE.md` (Services table line ~81; "Session Lifecycle" steps 1-2 lines ~154-155; "Editing a Session's Configuration" lines 171-226; add "## WSL Sessions" after "SSH Remote Sessions" (ends ~line 238); "Recently Closed Sessions" ~260; "Per-Session Run Commands" `Mode` bullet ~328; "Color / Theme" accent key ~131)

- [ ] **Step 1: Services table** — add a row after `ShellIntegrationPayload`:

```
| `WslDiscoveryService` | `wsl -l -v` parsing (UTF-16, header skipped, `*` default marker, names with spaces), `GetDistroHomeAsync` (cached `cd ~ && pwd` per distro+user), `ToUncPath` / `TryParseUncPath` — the **only** UNC↔Linux path converters; GitService and NewSessionDialog delegate here |
```

- [ ] **Step 2: Session Lifecycle** — step 1 becomes "… `NewSessionDialog` modal (Local, Remote SSH, or WSL)"; step 2 "… caller sets `Kind` and copies SSH or WSL fields; for WSL it also sets `WorkingFolder` to the `\\wsl$\<distro>\<linux path>` UNC mirror (see 'WSL Sessions')".

- [ ] **Step 3: Editing a Session's Configuration** — in the dialog bullets add "Local/Remote/WSL radio … WSL distro (pre-selected once the async list loads; a since-uninstalled distro is kept as a `(not installed)` entry so Save cannot wipe it), WSL user and Linux folder are all pre-filled. The appearance panel is shown for Local and WSL, hidden only for SSH." In the "Result plumbing" bullets: `Diff` also reports `WorkingFolderChanged` for a WSL distro/Linux-folder change; `Apply(session, draft)` writes `Kind` directly and **re-derives the UNC `WorkingFolder`** for WSL. In "What needs a restart": replace "local↔remote flip" with "any `Kind` change" and add "any WSL field change (distro, user, Linux folder)".

- [ ] **Step 4: New section after "SSH Remote Sessions"**

```markdown
## WSL Sessions

`SessionKind.Wsl` launches `wsl.exe -d <distro> [-u <user>] --cd <linux-folder> -- bash -lc "<command args>"` (PR #65, hardened on `feat/wsl-sessions-v2`).

- **`Kind` is the only persisted discriminator.** `IsRemote` is a `[JsonIgnore]`d two-way convenience over `Kind` (`false` on an SSH session makes it Local; a WSL session is untouched). The legacy `"IsRemote"` JSON key lands in `LegacyIsRemote` and `StateService.Normalize` folds it into `Kind` — migration lives in the loader, never in a setter. A promote-only setter was tried first and silently broke "Edit session" (SSH→Local could not demote) and then every save (`FullCommandLine` was serialised and threw); see `ShellSessionMigrationTests`.
- **UNC mirror invariant.** A WSL session stores `WorkingFolder = WslDiscoveryService.ToUncPath(WslDistro, WslWorkingFolder)` (`\\wsl$\Ubuntu\home\alice\proj`). Explorer, the dormant row, run-command template seeding and `GitService` all work off that path unchanged. The Linux-side path lives on `WslWorkingFolder` and is what `--cd` receives. Every path that creates or edits a WSL session must keep both in step: `MainWindow` session creation, `InheritSessionKindFrom` (duplicate / worktree), `SessionConfigEditor.Apply`, `ReopenClosedSessionAsync`.
- **GitService routing.** `RunGitFullAsync` detects the UNC and runs `wsl.exe -d <distro> -- git -C <linux> …`, translating `\\wsl$` args to Linux (`TranslateUncArgsToLinux`, with a distro-name boundary so `Ubuntu` never matches `Ubuntu-22.04`) and Linux paths in stdout back to UNC. `SessionViewModel.RefreshGitInfoAsync` runs the probe on the thread pool (a `Directory.Exists` on `\\wsl$` boots a stopped distro and used to freeze the UI), polls WSL sessions every **30 s** (10 s local) and caches a "not a repo" answer for WSL so it does not spawn `wsl.exe` for it forever.
- **Quoting.** Everything that reaches `wsl.exe` goes through `ShellSession.QuoteForCmd` — MSVCRT rules (backslashes before a quote doubled, trailing backslashes doubled), verified by round-tripping through `CommandLineToArgvW` in `Win32CommandLineTests`. There is one `BuildWslArgs` (`ShellSession.BuildWslArgs(string? inner)`); `RunInstance` delegates to it and turns a build failure into a failed run chip rather than a throw.
- **Validation before UI.** `ShellSession.LaunchValidationError` (blank distro / blank SSH host) is checked at the top of `LaunchSessionAsync` before any WebView2 exists. `state.json` and imports are untrusted; a bad entry used to leak a pane.
- **Relaunch paths.** `RecentlyClosedEntry` and the `session_history.snapshot_json` column carry `Kind` + WSL fields, so Ctrl+Shift+T, the "Recently closed" list and relaunch-from-search all restore the right kind.
- **Known gaps:** WSL Claude sessions do not auto-resume on restore (`--resume` id lookup reads the Windows `~/.claude`); a distro name beginning with `-` is not defended against in `wsl.exe` option parsing.
```

- [ ] **Step 5: Per-Session Run Commands** — change "SSH parents ignore `Mode` — remote runs always go through bash." to "SSH and WSL parents ignore `Mode` — those runs always go through bash (`ssh … bash -c` / `wsl.exe … bash -lc`)."

- [ ] **Step 6: Color / Theme** — "For local sessions the key is `WorkingFolder`; for SSH sessions `user@host`; for WSL `wsl://<distro><linux-folder>`."

- [ ] **Step 7: Recently Closed Sessions** — add one sentence: "Entries carry `Kind` and the SSH/WSL fields; legacy entries are migrated by `StateService.Normalize` like sessions."

- [ ] **Step 8: Verify** — `grep -n IsRemote CLAUDE.md`; every remaining mention must describe the convenience property or the legacy key, not a branch point.

- [ ] **Step 9: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: WSL sessions section; Kind replaces IsRemote throughout CLAUDE.md

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015BEpUD83C7UHLMxNm8XByu"
```

---

### Task 11: Final gates

- [ ] **Step 1:** `dotnet build src/CodeShellManager/CodeShellManager.csproj -nologo -c Release 2>&1 | grep -E "warning|error|Build succeeded"` — no new warnings vs. `origin/main`.
- [ ] **Step 2:** `dotnet test tests/CodeShellManager.Tests/ -nologo -v q` — all pass; record the count.
- [ ] **Step 3:** `git log --oneline origin/main..HEAD` — every commit carries the trailer.
- [ ] **Step 4:** Dispatch a read-only review agent against `origin/main...HEAD` with the ten original findings as a checklist; fix anything CONFIRMED, re-run gates.
- [ ] **Step 5:** Push `feat/wsl-sessions-v2`, open the PR against `main` referencing #65 and crediting the contributor; body lists each finding → fix → test.

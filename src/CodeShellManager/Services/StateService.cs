using System;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

public class StateService
{
    private static string StatePath =>
        Environment.GetEnvironmentVariable("CSM_STATE_PATH")
        ?? Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
            "CodeShellManager", "state.json");

    /// <summary>Scratch file for the atomic swap. Never read; deleted if a save fails.</summary>
    private static string TempPath => StatePath + ".tmp";

    /// <summary>One-generation backup, written for free by <see cref="File.Replace(string,string,string)"/>.</summary>
    private static string BackupPath => StatePath + ".bak";

    /// <summary>Serializes concurrent saves — see SaveAsync.</summary>
    private static readonly SemaphoreSlim SaveGate = new(1, 1);

    /// <summary>Order in which snapshots were taken, and the newest one on disk (read/written under SaveGate).</summary>
    private static long _saveSeq, _writtenSeq;

    private static readonly JsonSerializerOptions Options = new()
    {
        WriteIndented = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };

    /// <summary>Returns the resolved state file path (respects CSM_STATE_PATH env var).</summary>
    public static string GetPath() => StatePath;

    /// <summary>
    /// Loads persisted state, falling back to the backup if the primary file is
    /// unreadable (issue #88).
    ///
    /// The file is rewritten on ~33 different UI actions, so an interrupted write is a
    /// realistic way to end up with truncated JSON. Previously any failure here returned
    /// an empty state silently — indistinguishable from a genuine first run — and the
    /// next save then overwrote the damaged-but-recoverable file. Now we try the backup
    /// first and log every step to crash.log so the loss is at least visible.
    /// </summary>
    public async Task<AppState> LoadAsync()
    {
        var primary = await TryLoadAsync(StatePath);
        if (primary != null) return Normalize(primary);

        // Nothing on disk at all is the ordinary first-run case, not a failure.
        if (!File.Exists(StatePath) && !File.Exists(BackupPath)) return new AppState();

        Log($"state file at '{StatePath}' is unreadable — trying backup");

        var backup = await TryLoadAsync(BackupPath);
        if (backup != null)
        {
            Log("recovered state from backup");
            return Normalize(backup);
        }

        Log("backup unusable as well — starting from empty state");
        return new AppState();
    }

    private static async Task<AppState?> TryLoadAsync(string path)
    {
        try
        {
            if (!File.Exists(path)) return null;
            string json = await File.ReadAllTextAsync(path);
            return JsonSerializer.Deserialize<AppState>(json, Options);
        }
        catch (Exception ex)
        {
            Log($"failed to read '{path}': {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Coerces explicitly-null collections to empty.
    ///
    /// The property initialisers on <see cref="AppState"/> only apply when a key is
    /// *absent*; an explicit <c>"Sessions": null</c> deserializes to a real null and
    /// NREs on first use. We never write null (DefaultIgnoreCondition), but
    /// <see cref="ImportExportService"/> will happily deserialize whatever JSON file
    /// the user points at, so the loader can't assume its input came from us.
    /// </summary>
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

    /// <summary>
    /// Writes state atomically: serialize to a temp file, then swap it into place,
    /// keeping the previous file as <c>state.json.bak</c> (issue #88).
    ///
    /// The swap is what matters — a crash mid-write damages the temp file, which
    /// nothing reads, instead of the live one. A bare overwrite could leave the real
    /// file truncated and take the whole workspace with it.
    /// </summary>
    public async Task SaveAsync(AppState state)
    {
        // Serialize HERE, on the caller's thread, and do the file work on the pool.
        //
        // Callers are almost all on the UI thread, and the AppState graph is mutated there
        // too, so the snapshot has to be taken there — serializing on another thread could
        // walk a session list mid-edit. The I/O is a different matter: the temp write and
        // File.Replace used to run on the UI thread as well (no ConfigureAwait, and
        // File.Replace is synchronous), and antivirus or a synced folder can make that swap
        // take a visible while. Every OSC 9001 prompt hook, window move and git RepoRoot
        // landing triggers a save.
        long seq = Interlocked.Increment(ref _saveSeq);
        byte[] json;
        try { json = JsonSerializer.SerializeToUtf8Bytes(state, Options); }
        catch (Exception ex) { Log($"save failed: {ex.Message}"); return; }

        await Task.Run(() => WriteAsync(json, seq)).ConfigureAwait(false);
    }

    private static async Task WriteAsync(byte[] json, long seq)
    {
        // Serialize saves. 29 of the ~32 SaveStateAsync call sites are fire-and-forget,
        // so overlapping writes are routine rather than exotic. Without this gate two
        // saves race on the same temp file, and one can File.Replace the other's
        // half-written content into the live file — manufacturing exactly the torn
        // state this method exists to prevent.
        await SaveGate.WaitAsync().ConfigureAwait(false);
        var path = StatePath;
        try
        {
            // The gate doesn't promise FIFO order, and snapshots are now taken before
            // waiting on it — so an older snapshot can reach this point after a newer
            // one has already been written. Writing it would roll state back.
            if (seq <= _writtenSeq) return;

            Directory.CreateDirectory(Path.GetDirectoryName(path)!);

            await File.WriteAllBytesAsync(TempPath, json).ConfigureAwait(false);

            if (File.Exists(path))
                // Atomic on NTFS, and rotates the old file into .bak in one step.
                // ignoreMetadataErrors: ACL/attribute copy failures must not fail the save.
                File.Replace(TempPath, path, BackupPath, ignoreMetadataErrors: true);
            else
                File.Move(TempPath, path);   // first write — nothing to back up or replace

            _writtenSeq = seq;
        }
        catch (Exception ex)
        {
            Log($"save failed: {ex.Message}");
            // Leave the live file alone; just clear the scratch file so it can't be
            // mistaken for real state or block the next save.
            try { if (File.Exists(TempPath)) File.Delete(TempPath); }
            catch { /* best effort */ }
        }
        finally
        {
            SaveGate.Release();
        }
    }

    private static void Log(string message)
    {
        try
        {
            string logPath = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "CodeShellManager", "crash.log");
            Directory.CreateDirectory(Path.GetDirectoryName(logPath)!);
            File.AppendAllText(logPath, $"[{DateTime.Now:HH:mm:ss.fff}] StateService: {message}\n");
        }
        catch { /* logger failure is not actionable */ }
    }
}

using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Microsoft.Data.Sqlite;

namespace CodeShellManager.Services;

public enum SearchResultType { Output, Note }

public record SearchResult(
    string SessionId,
    string SessionName,
    string Snippet,
    DateTime Timestamp,
    SearchResultType Type = SearchResultType.Output,
    string? FolderPath = null)
{
    public bool IsNote => Type == SearchResultType.Note;
    public string TypeLabel => Type == SearchResultType.Note ? "note" : "";
}

public record SessionHistoryEntry(
    string SessionId,
    string SessionName,
    string WorkingFolder,
    string Command,
    string Args,
    string GroupId,
    DateTime ExitedAt);

public class SearchService
{
    private readonly SqliteConnection _db;

    public SearchService(SqliteConnection db)
    {
        _db = db;
    }

    public static async Task InitializeSchemaAsync(SqliteConnection db)
    {
        await using var cmd = db.CreateCommand();
        cmd.CommandText = """
            CREATE TABLE IF NOT EXISTS session_output (
                id          INTEGER PRIMARY KEY,
                session_id  TEXT    NOT NULL,
                session_name TEXT   NOT NULL,
                ts          INTEGER NOT NULL,
                line        TEXT    NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS output_fts USING fts5(
                line, session_name,
                content=session_output,
                content_rowid=id
            );
            CREATE TRIGGER IF NOT EXISTS output_fts_insert
            AFTER INSERT ON session_output BEGIN
                INSERT INTO output_fts(rowid, line, session_name)
                VALUES (new.id, new.line, new.session_name);
            END;
            CREATE TRIGGER IF NOT EXISTS output_fts_delete
            AFTER DELETE ON session_output BEGIN
                INSERT INTO output_fts(output_fts, rowid, line, session_name)
                VALUES ('delete', old.id, old.line, old.session_name);
            END;
            CREATE TRIGGER IF NOT EXISTS output_fts_update
            AFTER UPDATE ON session_output BEGIN
                INSERT INTO output_fts(output_fts, rowid, line, session_name)
                VALUES ('delete', old.id, old.line, old.session_name);
                INSERT INTO output_fts(rowid, line, session_name)
                VALUES (new.id, new.line, new.session_name);
            END;
            CREATE TABLE IF NOT EXISTS project_notes (
                folder_path TEXT PRIMARY KEY,
                content     TEXT NOT NULL DEFAULT '',
                updated_at  INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS session_history (
                id             INTEGER PRIMARY KEY,
                session_id     TEXT    NOT NULL,
                session_name   TEXT    NOT NULL,
                working_folder TEXT    NOT NULL,
                command        TEXT    NOT NULL,
                args           TEXT    NOT NULL DEFAULT '',
                group_id       TEXT    NOT NULL DEFAULT '',
                exited_at      INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS ix_session_history_sid ON session_history(session_id);
            CREATE INDEX IF NOT EXISTS ix_session_history_folder ON session_history(working_folder);
            """;
        await cmd.ExecuteNonQueryAsync();
    }

    // ── Project notes ─────────────────────────────────────────────────────────

    public async Task<string?> GetNoteAsync(string folderPath)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = "SELECT content FROM project_notes WHERE folder_path = $fp";
        cmd.Parameters.AddWithValue("$fp", folderPath);
        var result = await cmd.ExecuteScalarAsync();
        return result as string;
    }

    public async Task SaveNoteAsync(string folderPath, string content)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = """
            INSERT INTO project_notes (folder_path, content, updated_at)
            VALUES ($fp, $content, $ts)
            ON CONFLICT(folder_path) DO UPDATE SET
                content    = excluded.content,
                updated_at = excluded.updated_at
            """;
        cmd.Parameters.AddWithValue("$fp", folderPath);
        cmd.Parameters.AddWithValue("$content", content);
        cmd.Parameters.AddWithValue("$ts", DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
        await cmd.ExecuteNonQueryAsync();
    }

    // ── Search ────────────────────────────────────────────────────────────────

    public async Task<List<SearchResult>> SearchAsync(string query, int limit = 100)
    {
        var results = new List<SearchResult>();
        if (string.IsNullOrWhiteSpace(query)) return results;

        // Search terminal output via FTS5
        try
        {
            await using var cmd = _db.CreateCommand();
            cmd.CommandText = """
                SELECT so.session_id, so.session_name,
                       snippet(output_fts, 0, '[', ']', '...', 20) AS snippet,
                       so.ts
                FROM output_fts
                JOIN session_output so ON so.id = output_fts.rowid
                WHERE output_fts MATCH $q
                ORDER BY so.ts DESC
                LIMIT $limit
                """;
            cmd.Parameters.AddWithValue("$q", query);
            cmd.Parameters.AddWithValue("$limit", limit);

            await using var reader = await cmd.ExecuteReaderAsync();
            while (await reader.ReadAsync())
            {
                results.Add(new SearchResult(
                    SessionId: reader.GetString(0),
                    SessionName: reader.GetString(1),
                    Snippet: reader.GetString(2),
                    Timestamp: DateTimeOffset.FromUnixTimeMilliseconds(reader.GetInt64(3)).LocalDateTime,
                    Type: SearchResultType.Output));
            }
        }
        catch { /* query may be malformed */ }

        // Search project notes via LIKE (short free-text, FTS5 overkill)
        try
        {
            await using var cmd2 = _db.CreateCommand();
            cmd2.CommandText = """
                SELECT folder_path, content, updated_at
                FROM project_notes
                WHERE content LIKE $q
                ORDER BY updated_at DESC
                LIMIT $limit
                """;
            cmd2.Parameters.AddWithValue("$q", $"%{query}%");
            cmd2.Parameters.AddWithValue("$limit", limit);

            await using var reader2 = await cmd2.ExecuteReaderAsync();
            while (await reader2.ReadAsync())
            {
                string folderPath = reader2.GetString(0);
                string content    = reader2.GetString(1);
                long   ts         = reader2.GetInt64(2);
                string label      = System.IO.Path.GetFileName(folderPath.TrimEnd('/', '\\')) ?? folderPath;

                results.Add(new SearchResult(
                    SessionId: "",
                    SessionName: label,
                    Snippet: BuildNoteSnippet(content, query),
                    Timestamp: DateTimeOffset.FromUnixTimeMilliseconds(ts).LocalDateTime,
                    Type: SearchResultType.Note,
                    FolderPath: folderPath));
            }
        }
        catch { /* ignore */ }

        return results;
    }

    private static string BuildNoteSnippet(string content, string query)
    {
        int idx = content.IndexOf(query, StringComparison.OrdinalIgnoreCase);
        if (idx < 0) return content.Length > 120 ? content[..120] + "..." : content;

        int start = Math.Max(0, idx - 40);
        int end   = Math.Min(content.Length, idx + query.Length + 40);
        return (start > 0 ? "..." : "")
             + content[start..idx]
             + "[" + content[idx..(idx + query.Length)] + "]"
             + content[(idx + query.Length)..end]
             + (end < content.Length ? "..." : "");
    }

    // ── Session history ───────────────────────────────────────────────────────

    public async Task RecordSessionHistoryAsync(
        string sessionId, string sessionName, string workingFolder,
        string command, string args, string groupId)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = """
            INSERT INTO session_history
                (session_id, session_name, working_folder, command, args, group_id, exited_at)
            VALUES ($sid, $name, $folder, $cmd, $args, $gid, $ts)
            """;
        cmd.Parameters.AddWithValue("$sid", sessionId);
        cmd.Parameters.AddWithValue("$name", sessionName);
        cmd.Parameters.AddWithValue("$folder", workingFolder);
        cmd.Parameters.AddWithValue("$cmd", command);
        cmd.Parameters.AddWithValue("$args", args);
        cmd.Parameters.AddWithValue("$gid", groupId);
        cmd.Parameters.AddWithValue("$ts", DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
        await cmd.ExecuteNonQueryAsync();
    }

    public async Task<SessionHistoryEntry?> GetSessionHistoryAsync(string sessionId)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = """
            SELECT session_id, session_name, working_folder, command, args, group_id, exited_at
            FROM session_history WHERE session_id = $sid ORDER BY exited_at DESC LIMIT 1
            """;
        cmd.Parameters.AddWithValue("$sid", sessionId);
        await using var r = await cmd.ExecuteReaderAsync();
        if (!await r.ReadAsync()) return null;
        return new SessionHistoryEntry(
            r.GetString(0), r.GetString(1), r.GetString(2),
            r.GetString(3), r.GetString(4), r.GetString(5),
            DateTimeOffset.FromUnixTimeMilliseconds(r.GetInt64(6)).LocalDateTime);
    }

    public async Task<SessionHistoryEntry?> GetLatestSessionHistoryForFolderAsync(string folderPath)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = """
            SELECT session_id, session_name, working_folder, command, args, group_id, exited_at
            FROM session_history WHERE working_folder = $fp ORDER BY exited_at DESC LIMIT 1
            """;
        cmd.Parameters.AddWithValue("$fp", folderPath);
        await using var r = await cmd.ExecuteReaderAsync();
        if (!await r.ReadAsync()) return null;
        return new SessionHistoryEntry(
            r.GetString(0), r.GetString(1), r.GetString(2),
            r.GetString(3), r.GetString(4), r.GetString(5),
            DateTimeOffset.FromUnixTimeMilliseconds(r.GetInt64(6)).LocalDateTime);
    }

    public async Task DeleteSessionLogsAsync(string sessionId)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = "DELETE FROM session_output WHERE session_id = $sid";
        cmd.Parameters.AddWithValue("$sid", sessionId);
        await cmd.ExecuteNonQueryAsync();
    }

    // ── Storage management ───────────────────────────────────────────────────

    /// <summary>Deletes output rows older than the retention cutoff. No-op if retentionDays &lt;= 0.</summary>
    public async Task<int> PruneOldOutputAsync(int retentionDays)
    {
        if (retentionDays <= 0) return 0;
        long cutoff = DateTimeOffset.UtcNow.AddDays(-retentionDays).ToUnixTimeMilliseconds();
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = "DELETE FROM session_output WHERE ts < $cutoff";
        cmd.Parameters.AddWithValue("$cutoff", cutoff);
        return await cmd.ExecuteNonQueryAsync();
    }

    /// <summary>Wipes all indexed terminal output and reclaims disk space via VACUUM.</summary>
    public async Task ClearAllOutputAsync()
    {
        await using (var del = _db.CreateCommand())
        {
            del.CommandText = "DELETE FROM session_output";
            await del.ExecuteNonQueryAsync();
        }
        await using (var vac = _db.CreateCommand())
        {
            vac.CommandText = "VACUUM";
            await vac.ExecuteNonQueryAsync();
        }
    }

    /// <summary>Returns the SQLite database file size in bytes (page_count * page_size).</summary>
    public async Task<long> GetDatabaseSizeBytesAsync()
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()";
        var result = await cmd.ExecuteScalarAsync();
        return result is long l ? l : Convert.ToInt64(result ?? 0);
    }
}

using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Microsoft.Data.Sqlite;

namespace CodeShellManager.Services;

public record SearchResult(string SessionId, string SessionName, string Snippet, DateTime Timestamp);

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
            """;
        await cmd.ExecuteNonQueryAsync();
    }

    public async Task<List<SearchResult>> SearchAsync(string query, int limit = 100)
    {
        var results = new List<SearchResult>();
        if (string.IsNullOrWhiteSpace(query)) return results;

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
                    reader.GetString(0),
                    reader.GetString(1),
                    reader.GetString(2),
                    DateTimeOffset.FromUnixTimeMilliseconds(reader.GetInt64(3)).LocalDateTime
                ));
            }
        }
        catch { /* query may be malformed */ }

        return results;
    }

    public async Task DeleteSessionLogsAsync(string sessionId)
    {
        await using var cmd = _db.CreateCommand();
        cmd.CommandText = "DELETE FROM session_output WHERE session_id = $sid";
        cmd.Parameters.AddWithValue("$sid", sessionId);
        await cmd.ExecuteNonQueryAsync();
    }
}

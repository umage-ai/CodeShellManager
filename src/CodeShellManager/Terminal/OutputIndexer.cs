using System;
using System.Text.RegularExpressions;
using System.Threading.Channels;
using System.Threading.Tasks;
using Microsoft.Data.Sqlite;

namespace CodeShellManager.Terminal;

public partial class OutputIndexer : IDisposable
{
    private readonly string _sessionId;
    private readonly string _sessionName;
    private readonly SqliteConnection _db;
    private readonly Channel<(string sessionId, string sessionName, string line)> _queue;
    private readonly Task _worker;
    private bool _disposed;

    public OutputIndexer(SqliteConnection db, string sessionId, string sessionName)
    {
        _db = db;
        _sessionId = sessionId;
        _sessionName = sessionName;
        _queue = Channel.CreateUnbounded<(string, string, string)>(
            new UnboundedChannelOptions { SingleReader = true });
        _worker = Task.Run(WriteLoopAsync);
    }

    public void Feed(string rawOutput)
    {
        string clean = AnsiPattern().Replace(rawOutput, "");
        var lines = clean.Split('\n', StringSplitOptions.RemoveEmptyEntries);
        foreach (var line in lines)
        {
            string trimmed = line.Trim();
            if (trimmed.Length > 0)
                _queue.Writer.TryWrite((_sessionId, _sessionName, trimmed));
        }
    }

    private async Task WriteLoopAsync()
    {
        await foreach (var (sid, sname, line) in _queue.Reader.ReadAllAsync())
        {
            try
            {
                await using var cmd = _db.CreateCommand();
                cmd.CommandText = """
                    INSERT INTO session_output (session_id, session_name, ts, line)
                    VALUES ($sid, $sname, $ts, $line)
                    """;
                cmd.Parameters.AddWithValue("$sid", sid);
                cmd.Parameters.AddWithValue("$sname", sname);
                cmd.Parameters.AddWithValue("$ts", DateTimeOffset.UtcNow.ToUnixTimeMilliseconds());
                cmd.Parameters.AddWithValue("$line", line);
                await cmd.ExecuteNonQueryAsync();
            }
            catch { /* non-critical */ }
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _queue.Writer.Complete();
    }

    [GeneratedRegex(@"\x1B\[[0-9;]*[mGKHFJABCDsuhl]|\x1B\].*?\x07|\x1B[=>]|\r", RegexOptions.Compiled)]
    private static partial Regex AnsiPattern();
}

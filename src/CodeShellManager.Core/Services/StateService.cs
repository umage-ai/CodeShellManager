using System;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
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

    private static readonly JsonSerializerOptions Options = new()
    {
        WriteIndented = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };

    /// <summary>Returns the resolved state file path (respects CSM_STATE_PATH env var).</summary>
    public static string GetPath() => StatePath;

    public async Task<AppState> LoadAsync()
    {
        try
        {
            var path = StatePath;
            if (!File.Exists(path)) return new AppState();
            string json = await File.ReadAllTextAsync(path);
            return JsonSerializer.Deserialize<AppState>(json, Options) ?? new AppState();
        }
        catch
        {
            return new AppState();
        }
    }

    public async Task SaveAsync(AppState state)
    {
        try
        {
            var path = StatePath;
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            string json = JsonSerializer.Serialize(state, Options);
            await File.WriteAllTextAsync(path, json);
        }
        catch { /* non-critical */ }
    }
}

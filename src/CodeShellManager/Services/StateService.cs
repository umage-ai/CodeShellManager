using System;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading.Tasks;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

public class StateService
{
    private static readonly string StatePath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "CodeShellManager", "state.json");

    private static readonly JsonSerializerOptions Options = new()
    {
        WriteIndented = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };

    public async Task<AppState> LoadAsync()
    {
        try
        {
            if (!File.Exists(StatePath)) return new AppState();
            string json = await File.ReadAllTextAsync(StatePath);
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
            Directory.CreateDirectory(Path.GetDirectoryName(StatePath)!);
            string json = JsonSerializer.Serialize(state, Options);
            await File.WriteAllTextAsync(StatePath, json);
        }
        catch { /* non-critical */ }
    }
}

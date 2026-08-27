using System;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading.Tasks;
using CodeShellManager.Models;

namespace CodeShellManager.Services;

public static class ImportExportService
{
    private static readonly JsonSerializerOptions Options = new()
    {
        WriteIndented = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };

    public static async Task ExportAsync(AppState state, string path)
    {
        string json = JsonSerializer.Serialize(state, Options);
        await File.WriteAllTextAsync(path, json);
    }

    public static async Task<AppState?> ImportAsync(string path)
    {
        try
        {
            string json = await File.ReadAllTextAsync(path);
            var state = JsonSerializer.Deserialize<AppState>(json, Options);
            // Same coercion StateService.LoadAsync applies. Property initialisers only
            // cover an ABSENT key — an explicit "Sessions": null deserializes to a real
            // null. This is the untrusted path (any file the user points at), and the
            // caller dereferences Settings immediately, outside its own try block.
            return state is null ? null : StateService.Normalize(state);
        }
        catch
        {
            return null;
        }
    }
}

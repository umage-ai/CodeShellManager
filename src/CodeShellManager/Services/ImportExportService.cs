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
            if (state is null) return null;
            StateService.Normalize(state);
            // An imported "Open with" list is a list of programs to launch, from a file the
            // user didn't necessarily write. Stricter than state.json: no batch files at all.
            // The settings editor shows each entry's full resolved command, so nothing kept
            // here is hidden from the user.
            state.Settings.OpenWith = OpenWithService.Sanitize(state.Settings.OpenWith, allowBatch: false, OpenWithService.Log);
            return state;
        }
        catch
        {
            return null;
        }
    }
}

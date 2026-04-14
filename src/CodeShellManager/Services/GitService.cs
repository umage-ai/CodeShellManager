using System;
using System.Diagnostics;
using System.Threading.Tasks;

namespace CodeShellManager.Services;

public static class GitService
{
    public static async Task<(string? branch, bool isDirty)> GetGitInfoAsync(string folderPath)
    {
        if (string.IsNullOrWhiteSpace(folderPath) || !System.IO.Directory.Exists(folderPath))
            return (null, false);

        try
        {
            string? branch = await RunGitAsync(folderPath, "branch --show-current");
            if (string.IsNullOrWhiteSpace(branch))
                return (null, false);

            string? statusOutput = await RunGitAsync(folderPath, "status --porcelain");
            bool isDirty = !string.IsNullOrWhiteSpace(statusOutput);

            return (branch.Trim(), isDirty);
        }
        catch
        {
            return (null, false);
        }
    }

    private static async Task<string?> RunGitAsync(string workingDir, string arguments)
    {
        var psi = new ProcessStartInfo("git")
        {
            Arguments = $"-C \"{workingDir}\" {arguments}",
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
            CreateNoWindow = true
        };

        using var process = Process.Start(psi);
        if (process is null)
            return null;

        var outputTask = process.StandardOutput.ReadToEndAsync();
        var completed = await Task.WhenAny(outputTask, Task.Delay(3000));

        if (completed != outputTask)
        {
            try { process.Kill(); } catch { }
            return null;
        }

        await process.WaitForExitAsync();
        return process.ExitCode == 0 ? await outputTask : null;
    }
}

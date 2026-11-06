using System;
using System.IO;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// "Open in Explorer" opened Documents instead of the folder for forward-slash paths and names
/// with a comma. The fix opens the folder through the shell, which only must never happen for
/// something that isn't an existing directory. See "Opening a folder in Explorer" in CLAUDE.md.
/// </summary>
public sealed class FolderOpenerTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "csm-folderopener-" + Guid.NewGuid().ToString("N")[..8]);

    public FolderOpenerTests() => Directory.CreateDirectory(_root);

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { }
    }

    [Fact]
    public void A_forward_slash_path_from_git_becomes_the_windows_form()
    {
        string dir = Directory.CreateDirectory(Path.Combine(_root, "wt-feat")).FullName;
        string asGitPrintsIt = dir.Replace('\\', '/');

        Assert.Equal(Path.GetFullPath(dir), FolderOpener.Target(asGitPrintsIt));
        Assert.DoesNotContain('/', FolderOpener.Target(asGitPrintsIt)!);
    }

    [Theory]
    [InlineData("a,b")]       // Explorer splits its arguments on commas, even quoted
    [InlineData("x & y")]
    [InlineData("100%")]
    [InlineData("with space")]
    public void Awkward_folder_names_are_accepted_unchanged(string name)
    {
        string dir = Directory.CreateDirectory(Path.Combine(_root, name)).FullName;
        Assert.Equal(dir, FolderOpener.Target(dir));
        Assert.Equal(dir, FolderOpener.Target(dir + "\\"));      // trailing separator dropped
    }

    [Fact]
    public void A_drive_root_keeps_its_separator()
    {
        string root = Path.GetPathRoot(_root)!;                    // "C:\"
        Assert.Equal(root, FolderOpener.Target(root.TrimEnd('\\') + "/"));
    }

    [Fact]
    public void Nothing_but_an_existing_directory_is_ever_opened()
    {
        // ShellExecute on a FILE runs it, and a session's folder comes from state.json and
        // imports — untrusted. So a file, a missing path, a relative path or a URL must all be
        // refused before the shell sees them.
        string file = Path.Combine(_root, "payload.cmd");
        File.WriteAllText(file, "@echo pwned");

        Assert.Null(FolderOpener.Target(file));
        Assert.Null(FolderOpener.Target(Path.Combine(_root, "missing")));
        Assert.Null(FolderOpener.Target("relative\\folder"));
        Assert.Null(FolderOpener.Target("https://example.com/"));
        Assert.Null(FolderOpener.Target("calc.exe"));
        Assert.Null(FolderOpener.Target(""));
        Assert.Null(FolderOpener.Target(null));
    }

    [Fact]
    public async Task OpenAsync_refuses_a_file_without_throwing()
    {
        string file = Path.Combine(_root, "payload.cmd");
        File.WriteAllText(file, "@echo pwned");
        Assert.False(await FolderOpener.OpenAsync(file));
    }
}

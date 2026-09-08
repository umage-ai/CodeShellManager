using System.Collections.Generic;
using CodeShellManager.Models;
using CodeShellManager.ViewModels;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// Tests for the coalesced git notification (issue #70).
///
/// GitBranch, GitIsDirty and GitInfoLoaded raised three separate PropertyChanged events per
/// poll per session. Each crossed a blocking Dispatcher.Invoke and rebuilt the sidebar row's
/// WPF inlines — 141 rebuilds per cycle at 47 sessions, nearly always to redraw identical
/// text.
/// </summary>
public class SessionViewModelGitInfoTests
{
    private static SessionViewModel MakeVm() =>
        // SessionKind.Ssh short-circuits RefreshGitInfoAsync and skips StartGitWatcher, so
        // constructing one of these spawns no git processes — the tests stay hermetic.
        new(new ShellSession
        {
            Id = "test-session",
            Name = "test",
            Kind = SessionKind.Ssh,
            SshHost = "example.invalid"
        });

    private static List<string> Record(SessionViewModel vm)
    {
        var seen = new List<string>();
        vm.PropertyChanged += (_, e) => seen.Add(e.PropertyName ?? "");
        return seen;
    }

    [Fact]
    public void ApplyGitInfo_raises_exactly_one_notification()
    {
        var vm = MakeVm();
        var seen = Record(vm);

        vm.ApplyGitInfo("main", isDirty: true);

        Assert.Equal(new[] { nameof(SessionViewModel.GitInfoVersion) }, seen);
        Assert.Equal("main", vm.GitBranch);
        Assert.True(vm.GitIsDirty);
        Assert.True(vm.GitInfoLoaded);
    }

    [Fact]
    public void An_unchanged_poll_result_raises_nothing_at_all()
    {
        // The important one. A branch changes maybe once an hour, so almost every poll
        // returns exactly what it returned last time and must cost zero UI work.
        var vm = MakeVm();
        vm.ApplyGitInfo("main", isDirty: false);

        var seen = Record(vm);
        vm.ApplyGitInfo("main", isDirty: false);
        vm.ApplyGitInfo("main", isDirty: false);
        vm.ApplyGitInfo("main", isDirty: false);

        Assert.Empty(seen);
    }

    [Fact]
    public void A_changed_branch_does_notify()
    {
        var vm = MakeVm();
        vm.ApplyGitInfo("main", isDirty: false);

        var seen = Record(vm);
        vm.ApplyGitInfo("feature/x", isDirty: false);

        Assert.Equal(new[] { nameof(SessionViewModel.GitInfoVersion) }, seen);
        Assert.Equal("feature/x", vm.GitBranch);
    }

    [Fact]
    public void A_changed_dirty_flag_does_notify()
    {
        var vm = MakeVm();
        vm.ApplyGitInfo("main", isDirty: false);

        var seen = Record(vm);
        vm.ApplyGitInfo("main", isDirty: true);

        Assert.Single(seen);
        Assert.True(vm.GitIsDirty);
    }

    [Fact]
    public void First_result_notifies_even_when_the_values_are_defaults()
    {
        // A repo on a branch named "" is not a thing, but a *folder that is not a repo*
        // returns (null, false) — and the row still has to switch from "loading" to
        // "no git", so the first result must notify even though nothing "changed".
        var vm = MakeVm();
        var seen = Record(vm);

        vm.ApplyGitInfo(null, isDirty: false);

        Assert.Equal(new[] { nameof(SessionViewModel.GitInfoVersion) }, seen);
        Assert.True(vm.GitInfoLoaded);
    }

    [Fact]
    public void Setting_a_property_directly_still_notifies_once()
    {
        // OSC 9001 (ApplyShellIntegration) and the folder-edit reload set these individually
        // rather than through ApplyGitInfo, so the single-notification contract has to hold
        // on that path too.
        var vm = MakeVm();
        var seen = Record(vm);

        vm.GitBranch = "from-osc";

        Assert.Equal(new[] { nameof(SessionViewModel.GitInfoVersion) }, seen);
        Assert.Equal("from-osc", vm.GitBranch);
    }

    [Fact]
    public void GitInfoVersion_increments_per_change()
    {
        var vm = MakeVm();
        int start = vm.GitInfoVersion;

        vm.ApplyGitInfo("a", isDirty: false);
        vm.ApplyGitInfo("b", isDirty: false);
        vm.ApplyGitInfo("b", isDirty: false); // no-op

        Assert.Equal(start + 2, vm.GitInfoVersion);
    }
}

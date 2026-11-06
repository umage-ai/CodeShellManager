using System;
using System.Collections.Generic;
using System.Text;
using CodeShellManager.Models;
using CodeShellManager.Services;
using CodeShellManager.ViewModels;
using Xunit;

namespace CodeShellManager.Tests;

/// <summary>
/// The OSC 7501 → session-indicator mapping on <see cref="SessionViewModel"/>:
/// working → IsWorking/WorkProgress; blocked+permission → approval (orange);
/// blocked+question/auth/none → input (green); done/error → NeedsAttention; idle/clear → nothing.
/// Plus the fallback rule: the heuristic AlertDetector stands down once 7501 is seen.
/// </summary>
public class SessionViewModelProgramStatusTests
{
    private static readonly DateTime T0 = new(2026, 11, 6, 12, 0, 0, DateTimeKind.Utc);

    private static SessionViewModel MakeVm() =>
        // Ssh: no git spawns, no watcher — hermetic (see SessionViewModelGitInfoTests).
        new(new ShellSession { Id = "s", Name = "s", Kind = SessionKind.Ssh, SshHost = "example.invalid" });

    private static string B64(string s) => Convert.ToBase64String(Encoding.UTF8.GetBytes(s));

    [Fact]
    public void Working_with_progress_sets_the_working_state()
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus("state=working:progress=40", T0);
        Assert.True(vm.HasProgramStatus);
        Assert.True(vm.IsWorking);
        Assert.Equal(40, vm.WorkProgress);
        Assert.False(vm.NeedsAttention);
        Assert.False(vm.IsWaitingForInput);
        Assert.False(vm.IsWaitingForApproval);
    }

    [Fact]
    public void Working_without_progress_is_indeterminate()
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus("state=working", T0);
        Assert.True(vm.IsWorking);
        Assert.Null(vm.WorkProgress);
    }

    [Fact]
    public void Blocked_permission_is_approval_and_raises_a_notice()
    {
        var vm = MakeVm();
        var notice = vm.ApplyProgramStatus($"state=blocked:kind=permission:msg={B64("Run rm?")}", T0);
        Assert.True(vm.IsWaitingForApproval);
        Assert.False(vm.IsWaitingForInput);
        Assert.True(vm.NeedsAttention);
        Assert.Equal("Run rm?", vm.AlertMessage);
        Assert.Equal("Run rm?", notice!.Message);
    }

    [Theory]
    [InlineData("state=blocked:kind=question")]
    [InlineData("state=blocked:kind=auth")]
    [InlineData("state=blocked")]
    public void Other_blocked_kinds_are_input(string payload)
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus(payload, T0);
        Assert.True(vm.IsWaitingForInput);
        Assert.False(vm.IsWaitingForApproval);
        Assert.True(vm.NeedsAttention);
    }

    [Theory]
    [InlineData("state=done", "Finished")]
    [InlineData("state=error", "Failed")]
    public void Done_and_error_need_attention(string payload, string message)
    {
        var vm = MakeVm();
        var notice = vm.ApplyProgramStatus(payload, T0);
        Assert.True(vm.NeedsAttention);
        Assert.Equal(message, notice!.Message);
        Assert.False(vm.IsWorking);
    }

    [Fact]
    public void Idle_and_clear_clear_the_indicators()
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus("state=blocked:kind=permission", T0);
        vm.ApplyProgramStatus("state=idle", T0);
        Assert.False(vm.NeedsAttention);
        Assert.False(vm.IsWaitingForApproval);

        vm.ApplyProgramStatus("state=working", T0);
        vm.ApplyProgramStatus("state=clear", T0);
        Assert.False(vm.IsWorking);
        Assert.Null(vm.AgentState);
    }

    [Fact]
    public void Restating_the_same_state_does_not_re_raise_attention_or_toast()
    {
        var vm = MakeVm();
        Assert.NotNull(vm.ApplyProgramStatus("state=blocked:kind=permission:progress=10", T0));
        vm.ClearAlert();   // user looked
        Assert.False(vm.NeedsAttention);

        // Same state, new progress: not a new attention moment.
        Assert.Null(vm.ApplyProgramStatus("state=blocked:kind=permission:progress=20", T0.AddMinutes(1)));
        Assert.False(vm.NeedsAttention);
        Assert.True(vm.IsWaitingForApproval);   // the dot is still truthful
    }

    [Fact]
    public void Toasts_are_rate_limited_per_session()
    {
        var vm = MakeVm();
        Assert.NotNull(vm.ApplyProgramStatus("state=blocked:kind=permission", T0));
        vm.ApplyProgramStatus("state=working", T0.AddSeconds(1));
        // Re-entered attention 2s after the last toast: badge yes, toast no.
        Assert.Null(vm.ApplyProgramStatus("state=blocked:kind=permission", T0.AddSeconds(2)));
        Assert.True(vm.NeedsAttention);

        vm.ApplyProgramStatus("state=working", T0.AddSeconds(3));
        Assert.NotNull(vm.ApplyProgramStatus("state=done", T0 + SessionViewModel.ProgramStatusToastInterval));
    }

    [Fact]
    public void Invalid_reports_are_ignored_and_do_not_claim_the_session()
    {
        var vm = MakeVm();
        Assert.Null(vm.ApplyProgramStatus("state=bogus", T0));
        Assert.Null(vm.ApplyProgramStatus($"state=done:msg={B64("a\nb")}", T0));
        Assert.False(vm.HasProgramStatus);
        Assert.False(vm.NeedsAttention);
    }

    [Fact]
    public void Heuristic_alerts_stand_down_once_7501_is_seen()
    {
        var vm = MakeVm();
        vm.AlertDetector = new AlertDetector(vm.Id, "s");
        vm.ApplyProgramStatus("state=working", T0);

        Assert.True(vm.AlertDetector.IsStoodDown);
        vm.RaiseAlert("Do you want to proceed?", AlertType.ToolApproval);   // a queued guess
        Assert.False(vm.IsWaitingForApproval);
        Assert.False(vm.NeedsAttention);
        Assert.True(vm.IsWorking);
    }

    [Fact]
    public void First_report_overrides_a_heuristic_state()
    {
        var vm = MakeVm();
        vm.RaiseAlert("❯", AlertType.InputRequired);
        Assert.True(vm.IsWaitingForInput);
        vm.ApplyProgramStatus("state=working", T0);
        Assert.False(vm.IsWaitingForInput);
        Assert.False(vm.NeedsAttention);
    }

    [Fact]
    public void Sessions_without_7501_keep_todays_alert_behaviour()
    {
        var vm = MakeVm();
        vm.RaiseAlert("Allow?", AlertType.ToolApproval);
        Assert.True(vm.IsWaitingForApproval);
        vm.ClearAlert();
        Assert.False(vm.IsWaitingForApproval);
        Assert.False(vm.NeedsAttention);
    }

    [Fact]
    public void User_interaction_acknowledges_done_but_keeps_live_states()
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus("state=done", T0);
        vm.ClearAlert();
        Assert.False(vm.NeedsAttention);
        Assert.Null(vm.AgentState);              // done retired: the user has seen it

        vm.ApplyProgramStatus("state=blocked:kind=permission", T0);
        vm.ClearAlert();                         // a keystroke while the prompt is up
        Assert.False(vm.NeedsAttention);         // badge acknowledged…
        Assert.True(vm.IsWaitingForApproval);    // …but the program is still blocked

        vm.ApplyProgramStatus("state=working", T0);
        vm.ClearAlert();
        Assert.True(vm.IsWorking);
    }

    [Fact]
    public void Process_exit_drops_working_and_blocked_but_keeps_done()
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus("state=working", T0);
        vm.DropTransientProgramStatus();
        Assert.False(vm.IsWorking);

        vm.ApplyProgramStatus("state=done", T0);
        vm.ApplyProgramStatus("state=blocked:id=sub", T0);
        vm.DropTransientProgramStatus();
        Assert.Equal(ProgramState.Done, vm.AgentState);
    }

    [Fact]
    public void Root_done_badges_and_toasts_even_while_a_child_keeps_working()
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus("state=working:id=bg", T0);   // long-lived background task
        vm.ApplyProgramStatus("state=working", T0);
        var notice = vm.ApplyProgramStatus("state=done", T0.AddMinutes(1));

        Assert.NotNull(notice);
        Assert.Equal("Finished", notice!.Message);
        Assert.True(vm.NeedsAttention);
        Assert.True(vm.IsWorking);                          // child work stays visible

        // More child progress must not wipe the unseen result.
        vm.ApplyProgramStatus("state=working:id=bg:progress=50", T0.AddMinutes(2));
        Assert.True(vm.NeedsAttention);

        vm.ClearAlert();                                    // seen
        Assert.False(vm.NeedsAttention);
        Assert.True(vm.IsWorking);
    }

    [Fact]
    public void Clear_all_rearms_the_heuristic()
    {
        var vm = MakeVm();
        vm.AlertDetector = new AlertDetector(vm.Id, "s");
        vm.ApplyProgramStatus("state=working", T0);
        Assert.True(vm.AlertDetector.IsStoodDown);

        vm.ApplyProgramStatus("state=clear", T0);
        Assert.False(vm.HasProgramStatus);
        Assert.False(vm.AlertDetector.IsStoodDown);

        vm.RaiseAlert("Allow?", AlertType.ToolApproval);     // the heuristic counts again
        Assert.True(vm.IsWaitingForApproval);
    }

    [Fact]
    public void A_child_clear_that_leaves_records_does_not_rearm()
    {
        var vm = MakeVm();
        vm.AlertDetector = new AlertDetector(vm.Id, "s");
        vm.ApplyProgramStatus("state=idle", T0);
        vm.ApplyProgramStatus("state=working:id=bg", T0);
        vm.ApplyProgramStatus("state=clear:id=bg", T0);
        Assert.True(vm.HasProgramStatus);
        Assert.True(vm.AlertDetector.IsStoodDown);
    }

    [Fact]
    public void Prompt_start_that_empties_the_store_rearms()
    {
        var vm = MakeVm();
        vm.AlertDetector = new AlertDetector(vm.Id, "s");
        vm.ApplyProgramStatus("state=blocked:kind=permission", T0);
        vm.DropTransientProgramStatus();
        Assert.False(vm.HasProgramStatus);
        Assert.False(vm.AlertDetector.IsStoodDown);
        Assert.False(vm.IsWaitingForApproval);
        Assert.False(vm.NeedsAttention);
    }

    [Fact]
    public void Acknowledging_the_last_result_does_not_rearm()
    {
        // Otherwise a Claude session whose "done" was just seen would have the regex guess
        // at its prompt again.
        var vm = MakeVm();
        vm.AlertDetector = new AlertDetector(vm.Id, "s");
        vm.ApplyProgramStatus("state=done", T0);
        vm.ClearAlert();
        Assert.True(vm.HasProgramStatus);
        Assert.True(vm.AlertDetector.IsStoodDown);
    }

    [Fact]
    public void Clear_status_menu_drops_everything_and_rearms()
    {
        var vm = MakeVm();
        vm.AlertDetector = new AlertDetector(vm.Id, "s");
        vm.ApplyProgramStatus("state=working:progress=10", T0);
        vm.ApplyProgramStatus("state=done:id=x", T0);
        vm.ClearProgramStatus();
        Assert.False(vm.IsWorking);
        Assert.False(vm.NeedsAttention);
        Assert.Null(vm.AgentState);
        Assert.False(vm.HasProgramStatus);
        Assert.False(vm.AlertDetector.IsStoodDown);
    }

    [Theory]
    [InlineData("state=working")]
    [InlineData("state=blocked:kind=permission")]
    [InlineData("state=idle")]
    public void Live_reports_after_exit_are_ignored(string late)
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus("state=working", T0);
        vm.OnProcessExited();
        Assert.Null(vm.ApplyProgramStatus(late, T0));
        Assert.False(vm.IsWorking);
        Assert.False(vm.IsWaitingForApproval);
    }

    [Fact]
    public void Results_after_exit_are_still_shown()
    {
        var vm = MakeVm();
        vm.OnProcessExited();
        Assert.NotNull(vm.ApplyProgramStatus("state=error", T0));
        Assert.True(vm.NeedsAttention);
    }

    [Fact]
    public void HasUnacknowledgedProgramStatus_is_false_when_there_is_nothing_to_ack()
    {
        var vm = MakeVm();
        Assert.False(vm.HasUnacknowledgedProgramStatus);   // no 7501 at all
        vm.ApplyProgramStatus("state=working", T0);
        Assert.False(vm.HasUnacknowledgedProgramStatus);   // the per-keystroke common case
        vm.ApplyProgramStatus("state=done", T0);
        Assert.True(vm.HasUnacknowledgedProgramStatus);
        vm.ClearAlert();
        Assert.False(vm.HasUnacknowledgedProgramStatus);
    }

    [Fact]
    public void State_changes_raise_one_ProgramStatusVersion_notification()
    {
        var vm = MakeVm();
        vm.ApplyProgramStatus("state=working:progress=1", T0);
        var seen = new List<string>();
        vm.PropertyChanged += (_, e) => seen.Add(e.PropertyName ?? "");

        vm.ApplyProgramStatus("state=working:progress=2", T0);
        Assert.Equal(new[] { nameof(SessionViewModel.ProgramStatusVersion) }, seen);

        seen.Clear();
        vm.ApplyProgramStatus("state=working:progress=2", T0);   // identical: nothing at all
        Assert.Empty(seen);
    }
}

using CodeShellManager.Services;
using CodeShellManager.ViewModels;
using Xunit;

namespace CodeShellManager.Tests;

public class MainViewModelSeamTests
{
    private sealed class ImmediateDispatcher : IDispatcher
    {
        public void Post(System.Action action) => action();
    }

    private sealed class NoopToast : IToastNotifier
    {
        public int Calls;
        public void Show(string title, string message, bool playSound) => Calls++;
    }

    private static MainViewModel NewVm()
    {
        var sm = new SessionManager();
        var ss = new StateService();
        return new MainViewModel(sm, ss, new ImmediateDispatcher(), new NoopToast());
    }

    [Fact]
    public void UpdateWindowState_Maximized_DoesNotOverwriteNormalBounds()
    {
        var vm = NewVm();
        vm.UpdateWindowState(isMaximized: false, isNormal: true, 10, 20, 800, 600);
        vm.UpdateWindowState(isMaximized: true, isNormal: false, 0, 0, 1920, 1040);

        Assert.True(vm.IsWindowMaximized());
        var bounds = vm.GetSavedWindowBounds();
        Assert.NotNull(bounds);
        Assert.Equal(10, bounds!.Left);
        Assert.Equal(800, bounds.Width);
    }

    [Fact]
    public void CleanStart_SuppressesRecentlyClosedPush()
    {
        var vm = NewVm();
        vm.CleanStart = true;
        vm.PushRecentlyClosed(new CodeShellManager.Models.ShellSession { Name = "x" });
        Assert.Empty(vm.RecentlyClosed);
    }
}

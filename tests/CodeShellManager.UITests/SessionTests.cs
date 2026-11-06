using System.Linq;
using System.Threading;
using CodeShellManager.UITests.Helpers;
using FlaUI.Core.Tools;
using System;
using Xunit;

namespace CodeShellManager.UITests;

[Collection("UITests")]
public sealed class SessionTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _f;

    public SessionTests(AppFixture fixture) => _f = fixture;

    [Fact]
    public void CreateSession_AppearsInSidebar()
    {
        int before = AppActions.GetSidebarSessionCount(_f.MainWindow);

        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "SmokeTest1");

        // Allow up to 5s for PTY to start and sidebar to populate
        Thread.Sleep(2000);

        int after = AppActions.GetSidebarSessionCount(_f.MainWindow);
        Assert.True(after > before, $"Expected sidebar to grow. Before: {before}, After: {after}");
    }

    [Fact]
    public void CloseSession_RemovedFromSidebar()
    {
        // Create a session to close
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "SmokeTest2");
        Thread.Sleep(2000);

        int before = AppActions.GetSidebarSessionCount(_f.MainWindow);
        Assert.True(before > 0, "Expected at least one session before closing.");

        Assert.True(AppActions.CloseFirstSession(_f.MainWindow), "No terminal pane to close.");
        Thread.Sleep(1000);

        int after = AppActions.GetSidebarSessionCount(_f.MainWindow);
        Assert.True(after < before, $"Expected sidebar to shrink. Before: {before}, After: {after}");
    }

    [Fact]
    public void EmptyState_ShownWhenNoSessions()
    {
        // Close all sessions until empty
        int count = AppActions.GetSidebarSessionCount(_f.MainWindow);
        for (int i = 0; i < count + 2 && AppActions.CloseFirstSession(_f.MainWindow); i++)
            Thread.Sleep(500);

        // The EmptyState TextBlock should be visible. Matched by AutomationId: its text is
        // two lines, so the old exact-name match on "No sessions open." could never hit.
        var emptyState = AppActions.WaitForElement(_f.MainWindow, "EmptyState");
        Assert.False(emptyState.IsOffscreen, "Empty-state text should be visible with no sessions.");
    }

    [Fact]
    public void CreateSshSession_AddsSidebarItem()
    {
        int before = AppActions.GetSidebarSessionCount(_f.MainWindow);

        AppActions.CreateSshSession(_f.App, _f.MainWindow, _f.Automation,
            host: "user@test-host", name: "SSH Test");

        // Allow time for session launch attempt (ssh will fail — host doesn't exist — but the
        // sidebar item is created before the PTY exits)
        System.Threading.Thread.Sleep(1000);

        int after = AppActions.GetSidebarSessionCount(_f.MainWindow);
        Assert.Equal(before + 1, after);
    }
}

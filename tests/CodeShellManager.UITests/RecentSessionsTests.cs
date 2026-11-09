using System;
using System.Linq;
using System.Threading;
using CodeShellManager.UITests.Helpers;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Definitions;
using FlaUI.Core.Tools;
using Xunit;

namespace CodeShellManager.UITests;

/// <summary>The "🕘 Recent" toolbar menu lists other sessions, most recently used first.</summary>
[Collection("UITests")]
public sealed class RecentSessionsTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _f;

    public RecentSessionsTests(AppFixture fixture) => _f = fixture;

    private MenuItem[] OpenRecentMenu()
    {
        AppActions.Press(AppActions.WaitForElement(_f.MainWindow, "RecentSessionsBtn"));
        // A ContextMenu is a popup: an unnamed Window with the Menu inside. UI Automation lists
        // it either at the top level of the app's process (seen on a locked desktop) or as a
        // child of the main window (seen on an unlocked one), so look in both places.
        var menu = Retry.WhileNull(
            () => _f.Automation.GetDesktop()
                      .FindAllChildren(cf => cf.ByProcessId(_f.App.ProcessId))
                      .SelectMany(w => w.FindAllChildren(cf => cf.ByControlType(ControlType.Window)).Prepend(w))
                      .Select(w => w.FindFirstChild(cf => cf.ByControlType(ControlType.Menu)))
                      .FirstOrDefault(m => m != null),
            TimeSpan.FromSeconds(5)).Result
            ?? throw new TimeoutException("Recent menu did not open.");
        return menu.FindAllChildren(cf => cf.ByControlType(ControlType.MenuItem))
                   .Select(e => e.AsMenuItem()).ToArray();
    }

    [Fact]
    public void RecentMenu_ListsPreviousSessionFirst_AndJumpsToIt()
    {
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation, folder: @"C:\Windows", name: "RecentA");
        Thread.Sleep(1500);
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation, folder: @"C:\Windows", name: "RecentB");
        Thread.Sleep(1500);

        // B is active (just created), so the first entry is the one used before it: A.
        var items = OpenRecentMenu();
        Assert.Equal("RecentA", items[0].Name);
        Assert.DoesNotContain(items, i => i.Name == "RecentB");

        items[0].Invoke();   // jump to A
        Thread.Sleep(800);

        // Now A is active, so B is the previous one.
        items = OpenRecentMenu();
        Assert.Equal("RecentB", items[0].Name);
        Assert.DoesNotContain(items, i => i.Name == "RecentA");
        items[0].Invoke();
    }
}

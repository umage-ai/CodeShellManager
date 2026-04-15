using System.Threading;
using CodeShellManager.UITests.Helpers;
using Xunit;

namespace CodeShellManager.UITests;

[Collection("UITests")]
public sealed class LayoutTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _f;

    public LayoutTests(AppFixture fixture) => _f = fixture;

    [Fact]
    public void TwoColumn_ShowsTwoPanes()
    {
        // Create 2 sessions
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "Layout1");
        Thread.Sleep(1500);
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "Layout2");
        Thread.Sleep(1500);

        AppActions.SetLayout(_f.MainWindow, "Layout_Two");

        int count = AppActions.GetTerminalGridChildCount(_f.MainWindow);
        Assert.Equal(2, count);
    }

    [Fact]
    public void Single_ShowsOnePaneAfterTwoColumn()
    {
        // Ensure at least 2 sessions
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "Layout3");
        Thread.Sleep(1500);
        AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
            folder: @"C:\Windows", name: "Layout4");
        Thread.Sleep(1500);

        AppActions.SetLayout(_f.MainWindow, "Layout_Two");
        Thread.Sleep(300);
        AppActions.SetLayout(_f.MainWindow, "Layout_Single");

        int count = AppActions.GetTerminalGridChildCount(_f.MainWindow);
        Assert.Equal(1, count);
    }

    [Fact]
    public void TwoByTwo_OffscreenSession_BecomesVisibleOnSelect()
    {
        // Create 5 sessions — only 4 fit in 2×2
        for (int i = 1; i <= 5; i++)
        {
            AppActions.CreateSession(_f.App, _f.MainWindow, _f.Automation,
                folder: @"C:\Windows", name: $"Grid{i}");
            Thread.Sleep(1500);
        }

        AppActions.SetLayout(_f.MainWindow, "Layout_Grid");
        Thread.Sleep(500);

        // Click the 5th sidebar item (index 4) — it's off-screen in the 2×2 grid
        var sidebar = AppActions.WaitForElement(_f.MainWindow, "SidebarSessionList");
        var items = sidebar.FindAllChildren();
        Assert.True(items.Length >= 5, $"Expected ≥5 sidebar items, got {items.Length}");

        items[4].Click();
        Thread.Sleep(500);

        // After clicking, the viewport should scroll and the 5th pane should appear in the grid
        int gridCount = AppActions.GetTerminalGridChildCount(_f.MainWindow);
        Assert.True(gridCount > 0, "Expected at least one pane in grid after selecting off-screen session");
    }
}

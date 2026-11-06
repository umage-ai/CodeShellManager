using System;
using System.Threading;
using System.Threading.Tasks;
using CodeShellManager.UITests.Helpers;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Tools;
using Xunit;

namespace CodeShellManager.UITests;

/// <summary>
/// Closing the app while a session is busy asks first ("Sessions still running").
/// Its own fixture, because the second half of the test really closes the app.
/// </summary>
[Collection("UITests")]
public sealed class CloseWarningTests : IClassFixture<AppFixture>
{
    private const string DialogTitle = AppActions.BusyDialogTitle;
    private readonly AppFixture _f;

    public CloseWarningTests(AppFixture fixture) => _f = fixture;

    [Fact]
    public void ClosingWithBusySession_Asks_KeepRunning_ThenCloseAnyway()
    {
        // A session that prints every second: busy by the "producing output" rule.
        var dialog = AppActions.OpenNewSessionDialog(_f.App, _f.MainWindow, _f.Automation);
        dialog.Require("NewSessionFolderBox").AsTextBox().Text = @"C:\Windows";
        dialog.Require("NewSessionCommandCombo").AsComboBox().Select("[custom]");
        AppActions.WaitForElement(dialog, "NewSessionCustomArgsBox").AsTextBox().Text = "ping -t 127.0.0.1";
        dialog.Require("NewSessionNameBox").AsTextBox().Text = "BusyPing";
        AppActions.Press(dialog.Require("NewSessionOkBtn"));
        AppActions.WaitForDialogClosed(_f.App, _f.MainWindow, _f.Automation, "New Session");

        // Give pwsh + ping time to start printing.
        Thread.Sleep(5000);

        // Closing the session itself (its toolbar ✕) asks too; keep it.
        var pane = AppActions.WaitForElement(_f.MainWindow, "TerminalGrid")
            .FindFirstChild(cf => cf.ByAutomationId("TerminalPane"));
        AppActions.Press(pane.Require("TerminalCloseBtn"));
        var ask = AppActions.WaitForDialog(_f.App, _f.MainWindow, _f.Automation, DialogTitle,
            TimeSpan.FromSeconds(10));
        AppActions.Press(ask.Require("BusyKeepRunningBtn"));
        AppActions.WaitForDialogClosed(_f.App, _f.MainWindow, _f.Automation, DialogTitle);
        Thread.Sleep(500);
        Assert.Equal(1, AppActions.GetSidebarSessionCount(_f.MainWindow));

        // Close through UI Automation's WindowPattern. Off-thread: the close handler shows a
        // modal dialog, and the pattern call may not return until it is answered.
        _ = Task.Run(() => { try { _f.MainWindow.Patterns.Window.Pattern.Close(); } catch { } });

        var warn = AppActions.WaitForDialog(_f.App, _f.MainWindow, _f.Automation, DialogTitle,
            TimeSpan.FromSeconds(10));
        AppActions.Press(warn.Require("BusyKeepRunningBtn"));
        AppActions.WaitForDialogClosed(_f.App, _f.MainWindow, _f.Automation, DialogTitle);

        Thread.Sleep(1000);
        Assert.False(_f.App.HasExited, "Keep running must leave the app open.");
        Assert.Equal(1, AppActions.GetSidebarSessionCount(_f.MainWindow));

        // Second attempt: close anyway, and the app really exits.
        _ = Task.Run(() => { try { _f.MainWindow.Patterns.Window.Pattern.Close(); } catch { } });
        warn = AppActions.WaitForDialog(_f.App, _f.MainWindow, _f.Automation, DialogTitle,
            TimeSpan.FromSeconds(10));
        AppActions.Press(warn.Require("BusyCloseAnywayBtn"));

        bool exited = Retry.WhileFalse(() => _f.App.HasExited, TimeSpan.FromSeconds(30)).Result;
        Assert.True(exited, "Close anyway must close the app.");
    }
}

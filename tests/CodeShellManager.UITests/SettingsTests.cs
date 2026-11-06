using System;
using System.Linq;
using System.Threading;
using CodeShellManager.UITests.Helpers;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Tools;
using Xunit;

namespace CodeShellManager.UITests;

[Collection("UITests")]
public sealed class SettingsTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _f;

    public SettingsTests(AppFixture fixture) => _f = fixture;

    [Fact]
    public void MaxResults_PersistsAfterSave()
    {
        // Open Settings
        var settingsWindow = AppActions.OpenSettings(_f.App, _f.MainWindow, _f.Automation);

        // Set Max Search Results to 42
        var maxResultsBox = settingsWindow.Require("MaxSearchResultsBox").AsTextBox();
        maxResultsBox.Text = "42";

        // Click Save
        AppActions.Press(settingsWindow.Require("SettingsSaveBtn"));
        AppActions.WaitForDialogClosed(_f.App, _f.MainWindow, _f.Automation, "Settings");

        // Reopen Settings and verify value persisted
        var settingsWindow2 = AppActions.OpenSettings(_f.App, _f.MainWindow, _f.Automation);

        var maxResultsBox2 = settingsWindow2.Require("MaxSearchResultsBox").AsTextBox();

        Assert.Equal("42", maxResultsBox2.Text);

        // Close settings
        AppActions.Press(settingsWindow2.Require("SettingsSaveBtn"));
        AppActions.WaitForDialogClosed(_f.App, _f.MainWindow, _f.Automation, "Settings");
    }
}

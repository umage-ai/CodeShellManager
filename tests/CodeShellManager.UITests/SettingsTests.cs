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
        AppActions.WaitForElement(_f.MainWindow, "SettingsBtn").AsButton().Click();

        var settingsResult = Retry.WhileNull(
            () => _f.App.GetAllTopLevelWindows(_f.Automation)
                        .FirstOrDefault(w => w.Title == "Settings"),
            TimeSpan.FromSeconds(5));

        if (!settingsResult.Success)
            throw new TimeoutException("Settings window did not open.");

        var settingsWindow = settingsResult.Result!;

        // Set Max Search Results to 42
        var maxResultsBox = settingsWindow.Require("MaxSearchResultsBox").AsTextBox();
        maxResultsBox.Text = "42";

        // Click Save
        settingsWindow.Require("SettingsSaveBtn").AsButton().Click();
        Thread.Sleep(500);

        // Reopen Settings and verify value persisted
        AppActions.WaitForElement(_f.MainWindow, "SettingsBtn").AsButton().Click();

        var settingsResult2 = Retry.WhileNull(
            () => _f.App.GetAllTopLevelWindows(_f.Automation)
                        .FirstOrDefault(w => w.Title == "Settings"),
            TimeSpan.FromSeconds(5));

        if (!settingsResult2.Success)
            throw new TimeoutException("Settings window did not reopen.");

        var settingsWindow2 = settingsResult2.Result!;

        var maxResultsBox2 = settingsWindow2.Require("MaxSearchResultsBox").AsTextBox();

        Assert.Equal("42", maxResultsBox2.Text);

        // Close settings
        settingsWindow2.Require("SettingsSaveBtn").AsButton().Click();
    }
}

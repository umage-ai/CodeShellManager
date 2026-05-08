using System;
using System.Linq;
using System.Threading;
using CodeShellManager.UITests.Helpers;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Tools;
using Xunit;

namespace CodeShellManager.UITests;

[Collection("UITests")]
public sealed class ProfilesTests : IClassFixture<AppFixture>
{
    private readonly AppFixture _f;

    public ProfilesTests(AppFixture fixture) => _f = fixture;

    [Fact]
    public void ProfileCombo_AbsentWhenSettingOff()
    {
        // Default state: ImportWindowsTerminalProfiles = false.
        // Open the New Session dialog and confirm the Profile combobox is absent or hidden.

        AppActions.WaitForElement(_f.MainWindow, "NewSessionBtn").AsButton().Click();

        var dialogResult = Retry.WhileNull(
            () => _f.App.GetAllTopLevelWindows(_f.Automation)
                        .FirstOrDefault(w => w.Title == "New Session"),
            TimeSpan.FromSeconds(5));

        if (!dialogResult.Success)
            throw new TimeoutException("New Session dialog did not open.");

        var dialog = dialogResult.Result!;

        // The combobox should either not be in the tree or be off-screen (parent
        // StackPanel is Visibility="Collapsed" by default).
        var combo = dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionProfileCombo"));

        bool effectivelyHidden = combo == null
            || !combo.IsAvailable
            || combo.Properties.IsOffscreen.IsSupported && combo.Properties.IsOffscreen.Value;

        Assert.True(effectivelyHidden,
            "Profile combobox should be absent or hidden when ImportWindowsTerminalProfiles is off.");

        // Cancel the dialog
        dialog.Close();
    }

    [Fact]
    public void ImportWindowsTerminalProfilesCheckbox_IsPresentInSettings()
    {
        // Verify the new settings checkbox renders so users can opt in.

        AppActions.WaitForElement(_f.MainWindow, "SettingsBtn").AsButton().Click();

        var settingsResult = Retry.WhileNull(
            () => _f.App.GetAllTopLevelWindows(_f.Automation)
                        .FirstOrDefault(w => w.Title == "Settings"),
            TimeSpan.FromSeconds(5));

        if (!settingsResult.Success)
            throw new TimeoutException("Settings window did not open.");

        var settingsWindow = settingsResult.Result!;

        var checkbox = settingsWindow.FindFirstDescendant(
            cf => cf.ByAutomationId("ImportWindowsTerminalProfilesCheck"));

        Assert.NotNull(checkbox);

        // Cancel button has no AutomationId — close the window directly
        settingsWindow.Close();

        Thread.Sleep(300);
    }
}

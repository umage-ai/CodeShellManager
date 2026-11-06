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

        var dialog = AppActions.OpenNewSessionDialog(_f.App, _f.MainWindow, _f.Automation);

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

        var settingsWindow = AppActions.OpenSettings(_f.App, _f.MainWindow, _f.Automation);

        var checkbox = settingsWindow.FindFirstDescendant(
            cf => cf.ByAutomationId("ImportWindowsTerminalProfilesCheck"));

        Assert.NotNull(checkbox);

        // Cancel button has no AutomationId — close the window directly
        settingsWindow.Close();

        Thread.Sleep(300);
    }
}

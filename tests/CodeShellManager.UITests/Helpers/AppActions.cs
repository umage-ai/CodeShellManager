using System;
using System.Linq;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Tools;
using FlaUI.UIA3;

namespace CodeShellManager.UITests.Helpers;

/// <summary>Reusable UI interaction helpers shared across test classes.</summary>
public static class AppActions
{
    private static readonly TimeSpan DefaultTimeout = TimeSpan.FromSeconds(10);

    /// <summary>Waits up to <paramref name="timeout"/> for an element with the given AutomationId.</summary>
    public static AutomationElement WaitForElement(Window window, string automationId,
        TimeSpan? timeout = null)
    {
        var result = Retry.WhileNull(
            () => window.FindFirstDescendant(cf => cf.ByAutomationId(automationId)),
            timeout ?? DefaultTimeout);

        if (!result.Success)
            throw new TimeoutException(
                $"Element with AutomationId '{automationId}' not found within timeout.");
        return result.Result;
    }

    /// <summary>
    /// Opens the New Session dialog, fills folder and name, and clicks Start Session.
    /// Waits for the dialog to close before returning.
    /// </summary>
    public static void CreateSession(Application app, Window window, UIA3Automation automation,
        string folder = @"C:\Windows", string name = "Test")
    {
        // Click New Session
        WaitForElement(window, "NewSessionBtn").AsButton().Click();

        // Wait for dialog window
        var dialogResult = Retry.WhileNull(
            () => app.GetAllTopLevelWindows(automation)
                     .FirstOrDefault(w => w.Title == "New Session"),
            DefaultTimeout);

        if (!dialogResult.Success)
            throw new TimeoutException("New Session dialog did not open.");

        var dialog = dialogResult.Result!;

        // Fill folder
        var folderBox = dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionFolderBox")).AsTextBox();
        folderBox.Text = folder;

        // Fill name
        var nameBox = dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionNameBox")).AsTextBox();
        nameBox.Text = name;

        // Click Start Session
        dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionOkBtn")).AsButton().Click();

        // Wait for dialog to close
        Retry.WhileFalse(
            () => app.GetAllTopLevelWindows(automation)
                     .All(w => w.Title != "New Session"),
            DefaultTimeout);
    }

    /// <summary>Returns the number of direct children in the sidebar session list.</summary>
    public static int GetSidebarSessionCount(Window window)
    {
        var list = WaitForElement(window, "SidebarSessionList");
        return list.FindAllChildren().Length;
    }

    /// <summary>Returns the number of visible children in the terminal grid.</summary>
    public static int GetTerminalGridChildCount(Window window)
    {
        var grid = WaitForElement(window, "TerminalGrid");
        return grid.FindAllChildren(cf => cf.ByControlType(FlaUI.Core.Definitions.ControlType.Pane))
                   .Length;
    }

    /// <summary>Clicks a layout button by its AutomationId (e.g. "Layout_Two").</summary>
    public static void SetLayout(Window window, string layoutAutomationId)
    {
        WaitForElement(window, layoutAutomationId).AsButton().Click();
        System.Threading.Thread.Sleep(300); // let layout refresh
    }

    /// <summary>
    /// Opens the New Session dialog, switches to Remote (SSH) mode,
    /// fills in host and name, and clicks Start Session.
    /// Waits for the dialog to close before returning.
    /// </summary>
    public static void CreateSshSession(Application app, Window window, UIA3Automation automation,
        string host = "user@test-host", string name = "SSH Test")
    {
        WaitForElement(window, "NewSessionBtn").AsButton().Click();

        var dialogResult = Retry.WhileNull(
            () => app.GetAllTopLevelWindows(automation)
                     .FirstOrDefault(w => w.Title == "New Session"),
            DefaultTimeout);

        if (!dialogResult.Success)
            throw new TimeoutException("New Session dialog did not open.");

        var dialog = dialogResult.Result!;

        // Switch to Remote mode
        dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionRemoteRadio")).AsRadioButton().Click();

        // Fill SSH host
        var hostBox = dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionSshHostBox")).AsTextBox();
        hostBox.Text = host;

        // Fill session name
        var nameBox = dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionNameBox")).AsTextBox();
        nameBox.Text = name;

        // Click Start Session
        dialog.FindFirstDescendant(
            cf => cf.ByAutomationId("NewSessionOkBtn")).AsButton().Click();

        Retry.WhileFalse(
            () => app.GetAllTopLevelWindows(automation)
                     .All(w => w.Title != "New Session"),
            DefaultTimeout);
    }
}

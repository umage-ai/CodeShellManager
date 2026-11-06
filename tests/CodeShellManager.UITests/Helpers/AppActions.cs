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

        if (!result.Success || result.Result is null)
            throw new TimeoutException(
                $"Element with AutomationId '{automationId}' not found within timeout.");
        return result.Result;
    }

    /// <summary>
    /// Finds a descendant by AutomationId, or throws naming the id that was missing.
    ///
    /// FlaUI's FindFirstDescendant returns null when nothing matches, and the call sites
    /// here dereference the result immediately. Silencing that with <c>!</c> would turn a
    /// missing-element bug into a NullReferenceException with no indication of *which*
    /// element — strictly worse than the warning it hides. See issue #92.
    /// </summary>
    internal static AutomationElement Require(this AutomationElement parent, string automationId)
        => parent.FindFirstDescendant(cf => cf.ByAutomationId(automationId))
           ?? throw new InvalidOperationException(
               $"UI element with AutomationId '{automationId}' was not found.");

    /// <summary>
    /// Presses a button through UI Automation's Invoke pattern rather than a mouse click.
    /// A real click moves the user's cursor and lands on whatever window happens to be on
    /// top; Invoke goes straight to the button. WPF queues the click on the dispatcher, so
    /// this returns even when the button opens a modal dialog.
    /// </summary>
    public static void Press(AutomationElement button) => button.AsButton().Invoke();

    /// <summary>
    /// Finds an open dialog by title. The app's dialogs are OWNED by the main window, and
    /// UI Automation lists an owned window under its owner rather than at the top level —
    /// so <c>GetAllTopLevelWindows</c> alone never sees them, and every test that waited for
    /// "New Session" or "Settings" that way timed out. Both places are searched.
    /// </summary>
    public static Window? FindDialog(Application app, Window main, UIA3Automation automation, string title)
        => main.FindAllChildren(cf => cf.ByControlType(FlaUI.Core.Definitions.ControlType.Window))
               .Select(e => e.AsWindow())
               .FirstOrDefault(w => w.Title == title)
           ?? app.GetAllTopLevelWindows(automation).FirstOrDefault(w => w.Title == title);

    /// <summary>Waits for a dialog to open, or throws naming it.</summary>
    public static Window WaitForDialog(Application app, Window main, UIA3Automation automation,
        string title, TimeSpan? timeout = null)
    {
        var result = Retry.WhileNull(() => FindDialog(app, main, automation, title),
            timeout ?? DefaultTimeout);
        if (!result.Success || result.Result is null)
            throw new TimeoutException($"{title} dialog did not open.");
        return result.Result;
    }

    /// <summary>Waits for a dialog to close.</summary>
    public static void WaitForDialogClosed(Application app, Window main, UIA3Automation automation,
        string title)
        => Retry.WhileFalse(() => FindDialog(app, main, automation, title) is null, DefaultTimeout);

    /// <summary>Opens the New Session dialog and returns it.</summary>
    public static Window OpenNewSessionDialog(Application app, Window window, UIA3Automation automation)
    {
        Press(WaitForElement(window, "NewSessionBtn"));
        return WaitForDialog(app, window, automation, "New Session");
    }

    /// <summary>Opens the Settings window and returns it.</summary>
    public static Window OpenSettings(Application app, Window window, UIA3Automation automation)
    {
        Press(WaitForElement(window, "SettingsBtn"));
        return WaitForDialog(app, window, automation, "Settings");
    }

    /// <summary>
    /// Opens the New Session dialog, fills folder and name, and clicks Start Session.
    /// Waits for the dialog to close before returning.
    /// </summary>
    public static void CreateSession(Application app, Window window, UIA3Automation automation,
        string folder = @"C:\Windows", string name = "Test")
    {
        var dialog = OpenNewSessionDialog(app, window, automation);

        dialog.Require("NewSessionFolderBox").AsTextBox().Text = folder;
        dialog.Require("NewSessionNameBox").AsTextBox().Text = name;

        Press(dialog.Require("NewSessionOkBtn"));
        WaitForDialogClosed(app, window, automation, "New Session");
    }

    /// <summary>
    /// The live sidebar rows, top to bottom, found by their AutomationId
    /// (<c>SidebarSessionRow</c>) rather than as the list's children: the list also holds
    /// group and worktree-cluster headers, and in inline-headers mode the rows sit inside
    /// group sections.
    /// </summary>
    public static AutomationElement[] GetSidebarSessionRows(Window window)
        => WaitForElement(window, "SidebarSessionList")
           .FindAllDescendants(cf => cf.ByAutomationId("SidebarSessionRow"));

    /// <summary>Number of live sessions in the sidebar.</summary>
    public static int GetSidebarSessionCount(Window window) => GetSidebarSessionRows(window).Length;

    /// <summary>
    /// Number of terminal panes laid out in the grid. Only direct children are inspected —
    /// a descendant search would walk into every WebView2's page tree.
    /// </summary>
    public static int GetTerminalGridChildCount(Window window)
        => WaitForElement(window, "TerminalGrid")
           .FindAllChildren(cf => cf.ByAutomationId("TerminalPane"))
           .Length;

    /// <summary>
    /// Closes the first pane in the grid with its toolbar ✕ (<c>TerminalCloseBtn</c>).
    /// Returns false when there is no pane left. Used instead of Ctrl+W: synthesized
    /// keystrokes need an unlocked, focused desktop ("Access is denied" otherwise), while
    /// Invoke reaches the button regardless.
    /// </summary>
    public static bool CloseFirstSession(Window window)
    {
        var pane = WaitForElement(window, "TerminalGrid")
            .FindFirstChild(cf => cf.ByAutomationId("TerminalPane"));
        if (pane is null) return false;
        Press(pane.Require("TerminalCloseBtn"));
        // A session still printing (e.g. claude starting up) makes the app ask first
        // ("Sessions still running"). These tests mean to close it, so confirm.
        var busy = Retry.WhileNull(() => FindOwnedDialog(window, BusyDialogTitle),
            TimeSpan.FromSeconds(1)).Result;
        if (busy != null) Press(busy.Require("BusyCloseAnywayBtn"));
        return true;
    }

    /// <summary>Title of the "close anyway?" dialog shown for busy sessions.</summary>
    public const string BusyDialogTitle = "Sessions still running";

    private static Window? FindOwnedDialog(Window main, string title)
        => main.FindAllChildren(cf => cf.ByControlType(FlaUI.Core.Definitions.ControlType.Window))
               .Select(e => e.AsWindow())
               .FirstOrDefault(w => w.Title == title);

    /// <summary>Clicks a layout button by its AutomationId (e.g. "Layout_Two").</summary>
    public static void SetLayout(Window window, string layoutAutomationId)
    {
        Press(WaitForElement(window, layoutAutomationId));
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
        var dialog = OpenNewSessionDialog(app, window, automation);

        dialog.Require("NewSessionRemoteRadio").AsRadioButton().IsChecked = true;
        dialog.Require("NewSessionSshHostBox").AsTextBox().Text = host;
        dialog.Require("NewSessionNameBox").AsTextBox().Text = name;

        Press(dialog.Require("NewSessionOkBtn"));
        WaitForDialogClosed(app, window, automation, "New Session");
    }
}

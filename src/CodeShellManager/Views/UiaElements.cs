using System.Windows.Automation.Peers;
using System.Windows.Controls;

namespace CodeShellManager.Views;

// WPF layout elements (Border, StackPanel, Grid) have no automation peer, so UI Automation
// never sees them however they are tagged: AutomationProperties.AutomationId on one is
// silently ignored, and FlaUI's FindFirstDescendant can never find it. The sidebar rows,
// the session list and the terminal panes are all built from these, which is why the FlaUI
// tests waited out their timeouts on 'SidebarSessionList' and 'TerminalGrid'.
//
// These subclasses only add a peer (control type Group), so an AutomationId set on them
// becomes visible. Layout and rendering are untouched, and they remain a Border / StackPanel
// / Grid to every caller.

internal sealed class UiaBorder : Border
{
    protected override AutomationPeer OnCreateAutomationPeer() => new UiaGroupPeer(this);
}

internal sealed class UiaStackPanel : StackPanel
{
    protected override AutomationPeer OnCreateAutomationPeer() => new UiaGroupPeer(this);
}

internal sealed class UiaGrid : Grid
{
    protected override AutomationPeer OnCreateAutomationPeer() => new UiaGroupPeer(this);
}

internal sealed class UiaGroupPeer(System.Windows.FrameworkElement owner)
    : FrameworkElementAutomationPeer(owner)
{
    protected override AutomationControlType GetAutomationControlTypeCore() => AutomationControlType.Group;
    protected override string GetClassNameCore() => Owner.GetType().Name;
    protected override bool IsControlElementCore() => true;
}

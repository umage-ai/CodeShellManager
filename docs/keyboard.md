# Keyboard

Everything in CodeShellManager can be done from the keyboard. This page lists the keys and how to
change them. The list in the app (`F1`, `Ctrl+/` or `?`) is the same table, and it is always up to
date with your own settings.

## Keys

| Key | Action |
|---|---|
| `Ctrl+K` | Command palette: every command, by name |
| `Ctrl+T` | New session |
| `Ctrl+Alt+T` | Duplicate the active session |
| `Ctrl+Shift+T` | Reopen the last closed session |
| `Ctrl+W` | Close the active session |
| `Ctrl+Shift+R` | Restart the active session |
| `F2` | Rename the active session (or the focused row or group) |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous session in this group |
| `Ctrl+J` | Next session that needs you |
| `Ctrl+Shift+E` | Recent sessions (Enter: the previous one) |
| `Ctrl+1` … `Ctrl+9` | Rail tabs (1 is All) |
| `Ctrl+Alt+PgUp` / `Ctrl+Alt+PgDn` | Previous / next rail tab |
| `Ctrl+PgUp` / `Ctrl+PgDn` | Previous / next add-on screen tab |
| `Ctrl+Alt+Arrows` | Focus the pane in that direction |
| `Ctrl+F` | Search all output and notes |
| `Ctrl+I` | Show or hide the inspector |
| `Ctrl+Shift+N` | Notes for the active session |
| `Ctrl+.` | The active pane's menu |
| `Ctrl+Shift+C` | Copy Claude's last reply (nothing selected) |
| `F5` / `Shift+F5` | Run / stop the default run command |
| `Ctrl+,` | Settings |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Larger / smaller / 100% interface |
| `F10` | Main menu: every action, by area (or a clean tap of Alt) |
| `Shift+F10` | Menu of the focused item (outside the terminal) |
| `F6` / `Shift+F6` | Next / previous keyboard region (title bar, rail, sidebar, inbox, screen tabs, panes, inspector) |
| `Esc` | Close a menu, panel or dialog; from a region, back to the terminal |
| `F1`, `Ctrl+/`, `?` | The keyboard list |

Chords that stay fixed: the arrows, Home, End, Enter and Space in lists and menus; Tab and
Shift+Tab in dialogs and the quick-open; Left and Right on the quick-open command row; Ctrl+Enter
to start a session and keep the panel open; Ctrl+click on a link; Ctrl+Alt+C to copy a selection
from a Claude pane as shown.

## Menus and lists

- In any menu, Up and Down move, Home and End jump, Enter or Space picks, Right opens a submenu,
  Left closes it, and Esc closes the menu. Pressing a letter picks the item with that underlined
  letter.
- In the sidebar, Up and Down move between rows, Shift+Up and Shift+Down extend the selection,
  Space selects the focused row, Enter focuses its session.
- Dialogs: Tab moves between the controls, Enter presses the focused button (or the default one),
  Esc cancels.

## Changing a key

Open **Settings › Keyboard**. It lists every action with its keys.

- Click a key to record a new one: press the keys you want, and Esc cancels.
- **Clear** removes an action's keys. **Reset** puts one action back to its default, and
  **Reset all** does it for every action.

Two rules keep the keyboard usable:

- A key needs Ctrl or Alt, or be a function key (F1 to F12). Plain letters, digits, Tab and the
  arrows are never shortcuts, so typing and the terminal keep them.
- A key can belong to one action only. If you record a key another action uses, the change is
  refused and the message names that action. Clear it there first.

The same is available to agents and scripts: `keymap_list` shows every action with its keys,
conflicts and skipped entries; `keymap_set` sets an action's keys (an empty list clears them);
`keymap_reset` resets one action or all. See [tools.md](tools.md).

Your changes are saved in `state.json`, under `Keymap`, and only the ones that differ from the
defaults are written. A damaged entry is skipped, with the rest of the file kept, and it is listed
by `keymap_list`. An action name this version does not know is kept, for a newer version.

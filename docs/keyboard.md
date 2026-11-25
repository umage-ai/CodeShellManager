# Keyboard

Most of CodeShellManager can be driven from the keyboard, and the keys can be changed. This page
lists them and how to change them. The list in the app (`F1`, `Ctrl+/` or `?`) is the same table,
and it is always up to date with your own settings. Some parts still need the mouse (the Grid menu,
for one); `docs/design/keyboard.md` has the current list of gaps.

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
| `Ctrl+PgUp` / `Ctrl+PgDn` | Previous / next add-on screen tab (while one is open) |
| `Ctrl+Alt+Arrows` | Focus the pane in that direction |
| `Ctrl+F` | Search all output and notes |
| `Ctrl+I` | Show or hide the inspector |
| `Ctrl+Shift+N` | Notes for the active session |
| `Ctrl+.` | The active pane's menu |
| `Ctrl+Shift+C` | Copy Claude's last reply (nothing selected) |
| `F5` / `Shift+F5` / `Ctrl+Shift+F5` | Run (shows it if running) / stop / restart (asks first) the default run command |
| `Ctrl+,` | Settings |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Larger / smaller / 100% interface |
| `F10` | Main menu: every action, by area |
| `Shift+F10` | Menu of the focused item (outside the terminal) |
| `F6` / `Shift+F6` | Next / previous keyboard region (title bar, rail, sidebar, screen tabs, panes, inspector); the focus goes to the region's shown item |
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
- F6 moves the focus into a region and the arrows then work in it: the sidebar moves between
  rows, the rail and the inspector between their tabs, the screen strip between its tabs.

## Terminals and these keys

Every shortcut in the table above is taken from a focused terminal (F6, F10, Ctrl+Alt+Arrows,
Ctrl+1 to Ctrl+9, Ctrl+Tab and Ctrl+J among them), with two exceptions: Shift+F10 stays with the
terminal, and Ctrl+PgUp and Ctrl+PgDn go to the screen tabs only while an add-on screen is open
(with none, a terminal keeps them; vim uses them for its tabs). Programs that use the same keys
(Midnight Commander and htop use F6 and F10, for instance) lose them while CodeShellManager is in
front: rebind them in Settings › Keyboard. Plain keys, Tab, Esc and the arrows
are never taken from a terminal. When Tab moves the focus into a terminal, the focus ring leaves the
list or bar it was on, so the keys you type go where the ring is. While a menu is open, the plain
keys it does not use (Backspace, Delete, the F-keys) and typed letters are not passed on to the
terminal either.

**Shift+Enter** types a newline in the prompt of a program that supports it (Claude Code, OpenCode,
Copilot CLI) instead of sending it. The terminal sends Esc then Enter (the Meta+Enter that Claude
Code's `/terminal-setup` sets up in other terminals), or `CSI 13;2 u` when the program has turned on
the kitty keyboard protocol. A plain shell reads Esc then Enter differently (cmd.exe clears the line
and runs it, for instance): turn off *Shift+Enter inserts a newline* in Settings › Terminal and
Shift+Enter is a plain Enter again. Ctrl+Enter and Alt+Enter are not affected.

## Changing a key

Open **Settings › Keyboard**. It lists every action with its keys.

- Click a key to record a new one: press the keys you want, and Esc cancels.
- **Clear** removes an action's keys. **Reset** puts one action back to its default, and
  **Reset all** does it for every action.

Four rules keep the keyboard usable:

- A key needs Ctrl or must be a function key (F1 to F12). Plain letters, digits, Tab, Alt+letter and
  the arrows are never shortcuts, so typing and the terminal keep them.
- Ctrl+Alt with a letter, a digit or a punctuation key cannot be recorded: on keyboards with an
  AltGr key (Danish, German and others) AltGr arrives as Ctrl+Alt, and AltGr+2 is @, AltGr+7 is {,
  AltGr+E is the euro sign. Ctrl+Alt with an arrow, PgUp, PgDn, Home, End or a function key is
  fine. Whenever a Ctrl+Alt press types a character, it is typed and no shortcut runs.
- A key can belong to one action only. If you record a key another action uses, the change is
  refused and the message names that action. Clear it there first.
- The Ctrl+letter keys a shell or Claude Code needs (Ctrl+A, C, D, E, L, R, U, Z, and Ctrl+W, which
  closes the session by default) cannot be given to another action.

The same is available to agents and scripts: `keymap_list` shows every action with its keys,
conflicts and skipped entries; `keymap_set` sets an action's keys (an empty list clears them);
`keymap_reset` resets one action or all. See [tools.md](tools.md).

Your changes are saved in `state.json`, under `Keymap`, and only the ones that differ from the
defaults are written. A damaged entry is skipped, with the rest of the file kept, and it is listed
by `keymap_list`. An action name this version does not know is kept, for a newer version.

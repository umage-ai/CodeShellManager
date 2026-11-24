# Keyboard: drive the whole UI without a mouse

Date: 2026-11-24. Branch `kb/navigation`. Design note only: no app code changes in this step.
`file:line` references are to this worktree at `f1ada33`.

**Goal (owner-approved):** a power user can reach and operate every part of CSM from the keyboard,
and can rebind any action. Every existing shortcut keeps working.

## 1. Survey (facts)

- Bindings are hardcoded in `CsmApp::shortcuts` (`crates/csm-ui/src/app.rs` ~1865-2004). Each one is
  a `pressed(ctx, mods, key)` call on `consume_exact` (`app.rs:2257`), which matches modifiers
  exactly, so `Ctrl+T` never takes `Ctrl+Shift+T`.
- The display list `SHORTCUTS` (`crates/csm-ui/src/screens/overlays.rs:131`) and the palette hint
  table `shortcut_for` (`crates/csm-ui/src/screens/palette.rs:72`) are separate hand copies.
- Global shortcuts run before the panes draw (`app.rs:2095`). Ctrl chords and F-keys win over the
  terminal. The terminal consumes Tab, arrows and Escape (`crates/csm-ui/src/terminal/view.rs:378-382`),
  so egui's own Tab traversal cannot leave a pane.
- Esc closes overlays only when the terminal is not focused (`app.rs:2007`).
- Menus are the `Menu` enum (`app.rs:148-169`: Pane, Row, Group, Grid, Runs, GitHub, QuickGroup,
  Sort, Recent, ClaudeReplies). They are drawn by `menu_area` with no key handling. The only
  exception is the Recent menu's `recent_keys` (`app.rs:2016`).
- Sidebar rows have a focus ring (`sidebar.rs:955`) and respond to Enter, but not to arrows.
  Rail tabs and group headers respond to clicks only. The add-on screen tab strip
  (`addon_screen/mod.rs:184,200`) and the inspector tabs respond to clicks only. The inbox panel
  responds to clicks only.
- Confirm dialogs (`screens/overlays.rs:31-60`) take Enter and Esc, but Tab does not move between
  buttons.
- Focus ring: `size::RING` (`theme.rs:643`) in colour `t.blue`, applied ad hoc per widget.
- Tools that exist: `session_focus`, `view_set` (rail tab), `layout_set`, `screen_open`
  (`docs/tools.md`).
- Chords already taken (checked against the code): Ctrl+K, Ctrl+F, Ctrl+T, Ctrl+Alt+T,
  Ctrl+Shift+T, Ctrl+Shift+E, Ctrl+J, Ctrl+I, Ctrl+Shift+N, Ctrl+Shift+R, Ctrl+Shift+C, Ctrl+Alt+C,
  Ctrl+., Ctrl+W, Ctrl+Tab, Ctrl+Shift+Tab, Ctrl+1..9, Ctrl+0, Ctrl+=, Ctrl+-, Ctrl+,, Ctrl+/,
  F1, F2, F5, Shift+F5, Esc, Enter, Tab (new session command).
- Keys the terminal needs (`crates/csm-term/src/input.rs`): all printable keys, Ctrl+letter
  control codes, Alt+letter (ESC prefix), arrows, Home, End, PgUp, PgDn, Insert, Delete,
  F-keys. Claude Code also uses Esc, Shift+Tab and Ctrl+letter.

## 2. Focus-region model

One focus owner at a time. The region decides which keys the UI consumes; everything else goes to
the focused widget or the active terminal.

| Region | What it holds | Focus lands here by |
|---|---|---|
| `TitleBar` | title buttons: Recent, Grid, clock, Settings, zoom | F6 cycle |
| `RailTabs` | rail tabs (All, groups) | F6 cycle; Ctrl+Alt+PgUp/PgDn change tab |
| `SidebarList` | session rows and group headers | F6 cycle |
| `InboxPanel` | "needs you" list | F6 cycle |
| `ScreenStrip` | add-on screen tab strip | F6 cycle |
| `Panes` | one terminal per visible session | F6 cycle; click; Ctrl+Alt+Arrow |
| `Inspector` | inspector tab strip and its body | F6 cycle; Ctrl+I opens it |

Rules:

- **F6 / Shift+F6** cycle the regions in the order above, skipping hidden ones. Pure UI, no tool.
- **Esc** in a non-terminal region returns focus to the active terminal. Esc first closes the
  topmost menu, dialog or overlay, as it does today. Esc inside a focused terminal is passed to the
  terminal (Claude needs it).
- **Ctrl+Alt+Arrow** moves focus between visible panes by geometry: pick the nearest pane whose
  rectangle lies in the arrow's direction, with ties broken by distance along the axis. The
  direction-picking function is pure and lives in `csm-ui`. The focus change itself is the
  existing `session_focus` tool.
- **Terminal focus** keeps every key except the reserved set in section 4. Keys that a region
  needs in order to move (Tab, arrows) are consumed only while that region has focus, never by a
  terminal.
- **Focus ring**: every region above, every focusable row, tab, button and menu item draws the
  same ring when it has keyboard focus. See section 6.

Per-region keyboard behaviour (keys work only where the mouse already does the same thing):

| Region | Keys |
|---|---|
| Title bar | Left/Right move between buttons; Enter or Space activate |
| Rail tabs | Left/Right (or Up/Down, whichever axis the strip uses) move; Home/End jump; Enter or Space select |
| Sidebar list | Up/Down move the ring; Shift+Up/Down extend selection; Ctrl+Space toggles one row (as Ctrl+click); Home/End; Enter opens or runs the row; F2 renames; Delete closes it if the row menu offers Close; Shift+F10 or the context-menu key opens the row menu |
| Group headers | Enter toggles collapse; Shift+F10 opens the group menu |
| Inbox panel | Up/Down move; Enter opens the session |
| Screen strip | Left/Right move; Home/End; Enter or Space select; Ctrl+PgUp/PgDn switch tabs from any region |
| Inspector | Left/Right move between tabs (tablist); Tab goes into the body; Esc returns to the terminal |
| Panes | everything goes to the terminal; the pane menu is Ctrl+. (exists) and Shift+F10 outside the terminal |

Anything not in this table keeps its mouse behaviour only and is listed as a gap in step 3.

## 3. Menus, dialogs and tab strips

**Menus (all `Menu` variants).** One shared key handler on `menu_area`:

- Up/Down move the ring over enabled items, skipping separators and disabled items; Home/End jump.
- Enter or Space activates; Esc closes and returns focus to the element that opened it.
- Right opens a submenu where one exists; Left closes it.
- Access letters: each item label may mark one letter (underlined). Pressing that letter activates
  the item. Labels without a marker get no access letter. Letters are unique per menu.
- Opening: Shift+F10 and the context-menu key open the focused item's menu (row, group, pane,
  header). The Recent menu keeps its digits (1-9, 0).

**Main actions menu: F10.** A new menu listing every action grouped by area (Sessions, Panes,
Views, Layout, Runs, Git, Notes, Settings, Help). It is built from the tool registry plus the UI-only
actions, so a new tool appears without UI work. Each row shows its current chord from the keymap.
Typing filters the list; it is not a second command palette, and Ctrl+K stays the palette. Alt
on its own is an optional later alias (step 6); Alt is the terminal's meta prefix, so a bare Alt
tap is fragile and is not used in step 1.

**Dialogs (confirm and text dialogs, `screens/overlays.rs`).**

- Tab and Shift+Tab move between the controls (fields and buttons) inside the dialog only.
- The default button is marked (accented ring at rest) and Enter presses it. Esc cancels.
- Focus is trapped in the dialog while it is open and restored to the opener on close.

**Tab strips.**

- Add-on screens: Ctrl+PgUp/PgDn switch tabs (tool `screen_tab_next` / `screen_tab_prev`).
- Inspector tabs: arrows inside the tablist (UI only).
- Rail tabs: Ctrl+Alt+PgUp/PgDn (tool `rail_tab_next` / `rail_tab_prev`), plus the existing
  Ctrl+1..9.

## 4. Chord table

Chords are written as the keymap stores them (`Ctrl`, `Alt`, `Shift`). "Free" means checked against
section 1 and the terminal key list. "Risk" lists known OS or app conflicts.

| Chord | Action (id) | Kind | Check |
|---|---|---|---|
| F6 | Next focus region | UI | free |
| Shift+F6 | Previous focus region | UI | free |
| Esc | Back to active terminal / close topmost | UI | existing Esc, extended |
| Ctrl+Alt+Left/Right/Up/Down | Focus pane by geometry | UI + `session_focus` | free. Risk: Intel graphics drivers bind Ctrl+Alt+Arrow to screen rotation; the keymap lets the user rebind it |
| Ctrl+PgUp / Ctrl+PgDn | Screen tab prev / next (`screen_tab_prev`, `screen_tab_next`) | tool | free; PgUp/PgDn are only sent to the pty when no chord matches |
| Ctrl+Alt+PgUp / Ctrl+Alt+PgDn | Rail tab prev / next (`rail_tab_prev`, `rail_tab_next`) | tool | free |
| F10 | Main actions menu | UI | free. Windows reserves F10 only for a native menu bar, which CSM does not have |
| Shift+F10 | Menu of the focused item (outside terminal) | UI | free; the terminal keeps it when focused |
| Menu key (context menu key) | Same as Shift+F10 | UI | free |
| Ctrl+Space | Toggle the focused sidebar row in the selection | UI | free |
| Shift+Up / Shift+Down | Extend the sidebar selection | UI | free outside terminal |
| Enter / Space | Activate the focused control | UI | existing Enter; Space only outside terminal |
| F2 | Rename (existing) | tool `session_rename` | existing |
| Delete | Close the focused row's session, with confirm | tool `session_close` | free outside terminal; only if the row menu offers it |
| Tab / Shift+Tab | Move within a dialog or tablist | UI | consumed only when a non-terminal region has focus |

Existing chords stay exactly as they are (section 1). Chords kept out of the new table on purpose:
plain letters, digits without Ctrl, Alt+letter (the terminal's meta prefix), and Ctrl+letter that a
shell or Claude uses (Ctrl+A, Ctrl+E, Ctrl+R, Ctrl+U). Ctrl+W is already the close-session chord;
see the open point in section 10.

## 5. Rebindable keymap

**Model.** One table: action id -> ordered list of chords, plus a scope (`global`, `terminal`
excluded, `menu`, `dialog`, `region:<name>`). Action ids are tool ids for tools (`session_new`) and
`ui.<name>` for UI-only actions (`ui.focus_region_next`).

- **Defaults in code** (a `const` table in `csm-core`, pure: no egui). The table in section 4 is its
  first version. The existing hardcoded bindings move into it in step 2.
- **Chord type and parser in `csm-core`**: parse and format `Ctrl+Alt+PgUp` style strings, with
  one canonical spelling. Pure and tested.
- **User overrides** in the settings file, as `keymap: { "<action id>": ["Ctrl+K"] }`. An empty
  list removes the binding. Missing entries keep the default. Unknown ids are kept but reported
  (they may come from a newer build).
- **Conflict detection**: two actions in the same scope with the same chord is a conflict. Saving
  is refused with the names of both actions. The settings UI shows the conflict inline.
- **One source for every label**: the help overlay (`SHORTCUTS`), the palette hints
  (`shortcut_for`), menu labels and the F10 menu all read the keymap. The hand-written
  `SHORTCUTS` and `shortcut_for` tables are deleted in step 6.
- **Settings screen**: a Keyboard section lists every action with its chords, a "press a chord"
  capture field, Clear and Reset per row, and conflict messages. Editing is a UI action that writes
  the setting (it is not a tool, because it is a settings screen, but the setting itself is
  readable through the existing settings tool).

Egui mapping stays in `csm-ui`: it converts `egui::Modifiers` and `egui::Key` to the `csm-core`
chord and matches it with `consume_exact`, so matching is one loop over the table, not one `if` per
binding.

## 6. Focus ring

One token family in `theme.rs`, next to `size::RING`:

- `focus.ring` colour (default `t.blue`, its own token so themes can change it). Alpha variant
  `focus.ring_soft` for the fill of a focused row.
- `size::RING` stays the stroke width. The ring is drawn by one helper, `focus_ring(ui, rect, radius)`,
  used by every region above. Ad hoc `blue` strokes are removed in step 3.
- The ring is drawn only when keyboard focus is in use (the last input was a key), so mouse users
  do not see it. Mouse hover keeps its own style.

## 7. Tools

Tools are added only where the action changes app state and makes sense outside the UI, per
`docs/tools.md`. Pure widget movement stays UI.

| Tool | Effect | Notes |
|---|---|---|
| `screen_tab_next` / `screen_tab_prev` | switch the add-on screen tab | state change; exposed to CLI and MCP |
| `rail_tab_next` / `rail_tab_prev` | switch the rail tab | state change; `view_set` already exists, so these are thin wrappers that compute the next tab |
| `pane_focus_next` / `pane_focus_prev` | focus the next / previous visible pane | state change; wraps `session_focus` |

Pane geometry (Ctrl+Alt+Arrow) resolves to `session_focus` with the id chosen by UI. It needs no
new tool, because the outcome is the same tool call.

Effects are declared honestly in the `ToolSpec` (all three are `state` effects, no consent prompt
for MCP beyond what `session_focus` already asks).

## 8. Build steps

Each step is one commit on `kb/navigation`, gated as the rest of the repo is (section 9). None
of them push.

1. **Keymap core (`csm-core`).** Chord type, parser, formatter, default table, conflict check,
   override merge. Unit tests only; no UI change.
2. **Keymap in the app (`csm-ui`).** `CsmApp::shortcuts` becomes one loop over the table. Existing
   behaviour is kept; tests cover that every current chord still maps to its tool. Hardcoded
   `SHORTCUTS` and `shortcut_for` still exist and must match the table (a test checks it).
3. **Focus model and ring.** `FocusRegion`, F6 / Shift+F6, Esc return, Ctrl+Alt+Arrow pane focus,
   `focus_ring` helper and tokens, arrow keys in sidebar, rail, inbox, title bar and screen strip.
   Screenshot case: focus ring on sidebar and rail (`crates/csm-ui/tests/screenshots.rs`).
4. **Menus and dialogs.** Shared key handler in `menu_area` for all `Menu` variants (arrows,
   Enter, Esc, access letters, Shift+F10 and menu key). Dialog Tab trap and default button.
   Screenshot case: an open menu with keyboard focus.
5. **Tools and tab strips.** `screen_tab_*`, `rail_tab_*`, `pane_focus_*` in `csm-engine`;
   Ctrl+PgUp/PgDn and Ctrl+Alt+PgUp/PgDn bound; inspector tablist arrows. `docs/tools.md` regenerated
   by `cargo xtask tools`. Tests for each tool (state and effect declared).
6. **Main actions menu, keymap settings and labels.** F10 menu built from the tool registry plus UI
   actions; Keyboard section in settings with capture, conflict messages and reset; help overlay,
   palette hints and menu labels read the keymap; the old hand tables are deleted. Optional: Alt
   tap alias, if it proves reliable on Windows. Docs: a keyboard section in `docs/`.

**Status (branch `kb/navigation`).** Step 3's focus model is in: `FocusRegion` and the F6 /
Shift+F6 cycle over the regions drawn last frame (`crates/csm-ui/src/focus.rs`), Esc back to the
terminal, Ctrl+Alt+Arrow pane movement by geometry (`session_focus`, no new tool), and the focus
ring from `Tokens::focus()` through `widgets::focus_ring`. Not yet: the arrow keys inside each
region (sidebar, rail, inbox, title bar, screen strip), the Inspector and Inbox regions' own
keys, the ring on buttons and switches, and the dialog and menu keys (step 4). The inbox panel is
not a region yet (it is drawn inside the sidebar).

## 9. Gate and rules for these steps

- Gate per step, in the worktree: `cargo +1.97 test --workspace --no-fail-fast`,
  `cargo +1.97 clippy --workspace --all-targets -- -D warnings`, `cargo +1.97 xtask tools`,
  `cargo +1.97 xtask ci --skip-cargo`. `CARGO_TARGET_DIR` is the worktree's shared target directory.
- Screenshots (`CSM_SCREENS_OUT`) for every step that changes what is visible; they are looked at
  before the commit.
- No `unwrap`, `expect` or indexing on input-derived values. No I/O or lock waits in frame code: the
  keymap file is read once at start and written from a background thread.
- Theme tokens only for colour and size (the ring included). No literal colours in screen code.
- Other branches (quit board, notifications, themes) are not touched by these steps.

## 10. Open questions for the owner

1. Ctrl+W in the terminal (section 4): keep closing the session, or only outside the terminal?
2. Ctrl+Alt+Arrow may clash with Intel graphics rotation on some machines. Accept the risk with
   a rebind path, or use Alt+Shift+Arrow as the default instead?
3. F10 for the main actions menu, with the Alt tap as an optional alias later: agreed?

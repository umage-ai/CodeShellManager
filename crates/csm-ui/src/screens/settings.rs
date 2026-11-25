//! Settings (title bar ⚙, Ctrl+,): a page over the work area, not a modal. Sections on the left,
//! one column of controls on the right. Values come from tools (`settings_get`, `app_info`,
//! `grant_list`); every change is a tool call (`settings_set`, `view_set`, `grant_revoke`), and
//! the page shows what the engine answered, never a local copy that could disagree.

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{Action, CsmApp, SettingsSection};
use crate::icons;
use crate::model::GroupMode;
use crate::theme::{self, Tokens, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

fn set(key: &str, value: Value) -> Action {
    Action::Tool("settings_set", json!({"key": key, "value": value}))
}

pub fn show(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, t.bg);

    // The section list.
    let nav = Rect::from_min_max(rect.min, pos2(rect.min.x + size::SETTINGS_NAV_W, rect.max.y));
    ui.painter().rect_filled(nav, 0.0, t.mantle);
    ui.painter().vline(nav.max.x - size::HAIRLINE / 2.0, nav.y_range(), Stroke::new(size::HAIRLINE, t.surface0));
    let head_y = nav.min.y + space::LG + space::SM;
    ui.painter().text(pos2(nav.min.x + space::LG, head_y), Align2::LEFT_CENTER, "Settings", semibold(text::TITLE), t.fg);
    let mut y = head_y + space::XL;
    for sec in SettingsSection::ALL {
        let r = Rect::from_min_size(pos2(nav.min.x + space::SM, y), vec2(nav.width() - space::LG, size::CONTROL + space::XS));
        y += r.height() + space::XXS;
        let resp = ui.interact(r, Id::new(("settings-sec", sec.label())), Sense::click());
        let on = app.ui.settings.section == sec;
        if on || resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(radius::md()), if on { t.row_sel } else { t.row_hover });
        }
        let ir = Rect::from_min_size(pos2(r.min.x + space::ROW_X, r.center().y - icon::MD / 2.0), Vec2::splat(icon::MD));
        icons::paint(ui, ir, sec.icon(), icon::MD, if on { t.blue } else { t.muted });
        ui.painter().text(
            pos2(ir.max.x + space::SM, r.center().y),
            Align2::LEFT_CENTER,
            sec.label(),
            theme::medium(text::BODY),
            if on { t.fg } else { t.fg2 },
        );
        let label = format!("Settings: {}", sec.label());
        resp.widget_info(move || egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label.clone()));
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            app.ui.settings.section = sec;
            app.ui.settings.keymap_capture = None;
            if sec == SettingsSection::Usage {
                app.ui.settings.usage = crate::screens::backup::UsageState::default();
                app.ui.usage.summary_asked = false;
            }
        }
    }
    let close = Rect::from_min_size(pos2(nav.min.x + space::SM, nav.max.y - size::CONTROL - space::MD), vec2(nav.width() - space::LG, size::CONTROL));
    if ui
        .scope_builder(UiBuilder::new().max_rect(close), |ui| widgets::button(ui, ButtonKind::Ghost, Some("x"), "Close settings", Some("Esc")))
        .inner
        .clicked()
    {
        actions.push(Action::CloseSettings);
    }

    // The section.
    let body = Rect::from_min_max(pos2(nav.max.x, rect.min.y), rect.max);
    let col_w = size::SETTINGS_W.min(body.width() - space::XL * 2.0).max(size::MENU_W);
    let col = Rect::from_min_max(pos2(body.min.x + space::XL, body.min.y + space::LG), pos2(body.min.x + space::XL + col_w, body.max.y));
    ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
        egui::ScrollArea::vertical().id_salt("settings-body").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_width(col_w - space::SM);
            ui.spacing_mut().item_spacing.y = space::SM;
            ui.label(egui::RichText::new(app.ui.settings.section.label()).font(semibold(text::TITLE)).color(t.fg));
            ui.add_space(space::XS);
            if app.ui.settings.values.is_empty() {
                ui.label(egui::RichText::new("Loading the settings…").font(theme::regular(text::BODY)).color(t.muted));
                return;
            }
            match app.ui.settings.section {
                SettingsSection::General => general(ui, app, actions),
                SettingsSection::Appearance => appearance(ui, app, actions),
                SettingsSection::Keyboard => keyboard(ui, app, actions),
                SettingsSection::Groups => groups(ui, app, actions),
                SettingsSection::Mcp => mcp(ui, app, actions),
                SettingsSection::Addons => crate::screens::addons::show(ui, app, actions),
                SettingsSection::Remote => crate::screens::remote::settings(ui, app, actions),
                SettingsSection::Usage => {
                    crate::screens::backup::usage(ui, app, actions);
                    crate::screens::usage::settings(ui, app, actions);
                }
                SettingsSection::About => about(ui, app, actions),
            }
            ui.add_space(space::XL);
        });
    });
}

/// Settings → Keyboard: every action with its chords, from the keyboard map. A chord button records
/// the next key press (Escape cancels); Clear removes the action's chords, Reset puts them back,
/// and Reset all does it for every action. Each change is a `keymap_set` or `keymap_reset` call: the
/// engine checks it (a chord another action uses, or one that cannot be bound) and says why in a toast.
fn keyboard(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let map = app.prefs.keymap.clone();
    let recording = app.ui.settings.keymap_capture.clone();
    let customised = map.entries().iter().any(|e| e.customised());
    match recording.as_deref().and_then(|id| map.entries().iter().find(|e| e.id() == id)) {
        Some(e) => caption(ui, &format!("Press the new keys for {}. Escape cancels.", e.name())),
        None => caption(ui, "Click a shortcut to record a new one. A shortcut uses the Control key or a function key."),
    }
    if customised && widgets::button(ui, ButtonKind::Ghost, Some("restart"), "Reset all", None).clicked() {
        actions.push(Action::Tool("keymap_reset", json!({})));
    }
    if !map.conflicts().is_empty() {
        let list: Vec<String> = map.conflicts().iter().map(|c| format!("{} ({})", c.chord, c.dropped)).collect();
        caption(ui, &format!("Two actions share a chord; the first claim is kept: {}.", list.join(", ")));
    }
    for e in map.entries() {
        ui.add_space(space::XXS);
        ui.horizontal(|ui| {
            let name = egui::RichText::new(e.name()).font(theme::regular(text::BODY)).color(t.fg2);
            ui.add(egui::Label::new(name).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if e.customised() && widgets::button(ui, ButtonKind::Ghost, None, "Reset", None).clicked() {
                    actions.push(Action::Tool("keymap_set", json!({"action": e.id(), "chords": e.default_chords().iter().map(ToString::to_string).collect::<Vec<_>>()})));
                }
                if !e.chords().is_empty() && widgets::button(ui, ButtonKind::Ghost, None, "Clear", None).clicked() {
                    actions.push(Action::Tool("keymap_set", json!({"action": e.id(), "chords": []})));
                }
                let shown = if recording.as_deref() == Some(e.id()) {
                    "Press keys…".to_owned()
                } else {
                    let all: Vec<String> = e.chords().iter().map(ToString::to_string).collect();
                    if all.is_empty() { "None".to_owned() } else { all.join(" or ") }
                };
                if widgets::button(ui, ButtonKind::Secondary, None, &shown, None).clicked() {
                    app.ui.settings.keymap_capture = Some(e.id().to_owned());
                }
            });
        });
    }
    ui.add_space(space::SM);
    caption(ui, "The same keys work in the help list (F1), the command palette and the menus, which read this list.");
}

pub(crate) fn caption(ui: &mut Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(egui::RichText::new(s).font(theme::regular(text::CAPTION)).color(t.muted)).wrap());
}

pub(crate) fn section(ui: &mut Ui, s: &str) {
    ui.add_space(space::SM);
    widgets::caps(ui, s);
}

/// A labelled one-line text setting: saved (`settings_set`) on Enter or when it loses focus.
fn text_setting(ui: &mut Ui, app: &mut CsmApp, key: &str, label: &str, hint: &str, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let saved = app.ui.settings.str(key).unwrap_or_default().to_string();
    let mut buf = app.ui.settings.drafts.get(key).cloned().unwrap_or_else(|| saved.clone());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).font(theme::regular(text::BODY)).color(t.fg));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let edit = egui::TextEdit::singleline(&mut buf)
                .id(Id::new(("setting", key.to_string())))
                .hint_text(egui::RichText::new(hint).color(t.faint))
                .font(theme::mono(text::MONO))
                .desired_width(size::FIELD_W);
            let r = ui.add(edit);
            let label = label.to_string();
            r.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, label.clone()));
            // Escape gives the field its saved text back; only Enter or leaving the field saves.
            let escaped = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
            if escaped {
                buf.clone_from(&saved);
            }
            let commit = !escaped && (r.lost_focus() || (r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))));
            if commit && buf != saved {
                actions.push(set(key, Value::String(buf.trim().to_string())));
            }
        });
    });
    if buf == saved {
        app.ui.settings.drafts.remove(key);
    } else {
        app.ui.settings.drafts.insert(key.to_string(), buf);
    }
}

pub(crate) fn bool_setting(ui: &mut Ui, app: &CsmApp, key: &str, label: &str, hint: &str, actions: &mut Vec<Action>) {
    let on = app.ui.settings.bool(key).unwrap_or(false);
    if widgets::toggle(ui, on, label, Some(hint)).clicked() {
        actions.push(set(key, Value::Bool(!on)));
    }
}

/// A whole-number setting with a drag value; saved when the drag or the typing ends.
fn number_setting(
    ui: &mut Ui,
    app: &mut CsmApp,
    key: &str,
    label: &str,
    range: std::ops::RangeInclusive<i64>,
    unit: &str,
    actions: &mut Vec<Action>,
) {
    let t = Tokens::get(ui.ctx());
    let saved = app.ui.settings.num(key).map_or(0, |n| n as i64);
    let draft_key = format!("#{key}");
    let mut v: i64 = app.ui.settings.drafts.get(&draft_key).and_then(|d| d.parse().ok()).unwrap_or(saved);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).font(theme::regular(text::BODY)).color(t.fg));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(unit).font(theme::regular(text::CAPTION)).color(t.muted));
            let r = ui.add(egui::DragValue::new(&mut v).range(range));
            let label = label.to_string();
            r.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::DragValue, true, label.clone()));
            let escaped = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
            if escaped {
                v = saved;
            }
            let done = !escaped && (r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus()));
            if done && v != saved {
                actions.push(set(key, json!(v)));
            }
        });
    });
    if v == saved {
        app.ui.settings.drafts.remove(&draft_key);
    } else {
        app.ui.settings.drafts.insert(draft_key, v.to_string());
    }
}

fn general(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    section(ui, "New sessions");
    text_setting(ui, app, "DefaultWorkingFolder", "Default folder", "C:\\src (blank: ask)", actions);
    text_setting(ui, app, "DefaultCommand", "Default command", "claude", actions);
    section(ui, "Launch commands");
    caption(ui, "The commands of the New session panel, in this order (Left and Right on its command row cycle them).");
    let cmds: Vec<String> = app
        .ui
        .settings
        .values
        .get("LaunchCommands")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    widgets::card_frame(&t).inner_margin(egui::Margin::symmetric(space::SM as i8, space::XS as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, c) in cmds.iter().enumerate() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL), Sense::hover());
            ui.painter().text(pos2(r.min.x + space::XS, r.center().y), Align2::LEFT_CENTER, c, theme::mono(text::MONO), t.fg);
            let mut x = r.max.x;
            let mut btn = |ui: &mut Ui, name: &str, tip: &str| {
                x -= size::CONTROL_SM;
                let b = Rect::from_center_size(pos2(x + size::CONTROL_SM / 2.0, r.center().y), Vec2::splat(size::CONTROL_SM));
                x -= space::XXS;
                widgets::icon_button_at(ui, b, Id::new(("launch-cmd", i, name.to_string())), name, false, tip).clicked()
            };
            if btn(ui, "x", &format!("Remove “{c}”")) {
                let mut v = cmds.clone();
                v.remove(i);
                actions.push(set("LaunchCommands", json!(v)));
            }
            if i > 0 && btn(ui, "chevU", &format!("Move “{c}” up")) {
                let mut v = cmds.clone();
                v.swap(i, i - 1);
                actions.push(set("LaunchCommands", json!(v)));
            }
        }
        if cmds.is_empty() {
            ui.label(
                egui::RichText::new("No commands: the New session panel offers the default command only.")
                    .font(theme::regular(text::CAPTION))
                    .color(t.muted),
            );
        }
    });
    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut app.ui.settings.new_command)
            .id(Id::new("settings-new-command"))
            .hint_text(egui::RichText::new("claude --model …").color(t.faint))
            .font(theme::mono(text::MONO))
            .desired_width(size::FIELD_W);
        let r = ui.add(edit);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "New launch command"));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let add = widgets::button(ui, ButtonKind::Secondary, Some("plus"), "Add", None).clicked();
        let c = app.ui.settings.new_command.trim().to_string();
        if (enter || add) && !c.is_empty() && !c.chars().any(char::is_control) {
            let mut v = cmds.clone();
            if !v.contains(&c) {
                v.push(c);
                actions.push(set("LaunchCommands", json!(v)));
            }
            app.ui.settings.new_command.clear();
        }
    });
    section(ui, "Starting and closing");
    bool_setting(
        ui,
        app,
        "AutoRestoreSessions",
        "Restore sessions on start",
        "Sessions that were open when the app closed start again, one at a time.",
        actions,
    );
    bool_setting(
        ui,
        app,
        "AutoResumeClaude",
        "Resume Claude conversations",
        "A restored or restarted Claude session continues its conversation (--resume).",
        actions,
    );
    bool_setting(
        ui,
        app,
        "PassSessionNameToClaude",
        "Name Claude conversations after the session",
        "Claude starts with --name and the session's name, so its /resume list and terminal title show it. Needs a Claude Code that knows --name; applies from the next start.",
        actions,
    );
    close_action(ui, app, actions);
    bool_setting(
        ui,
        app,
        "WarnOnCloseWhenBusy",
        "Warn when closing busy sessions",
        "Ask before quitting while a session is working or waiting on you.",
        actions,
    );
    number_setting(ui, app, "ClaudeLaunchStaggerMs", "Claude launch stagger", 0..=10_000, "ms", actions);
    caption(ui, "A flat pause between Claude starts, so two never rewrite ~/.claude.json at once.");
    number_setting(ui, app, "QuitClaudeConcurrency", "Claude sessions closed at once when quitting", 1..=8, "", actions);
    caption(
        ui,
        "Higher is faster, but Claude rewrites ~/.claude.json as it exits, so some updates can be lost; it is backed up first and restored if damaged. 1 closes them one at a time.",
    );
    crate::screens::startup::show(ui, app, actions);
    crate::screens::claude_commands::show(ui, app, actions);
    crate::screens::backup::general(ui, app, actions);
    notifications(ui, app, actions);
    updates(ui, app, actions);
    storage(ui, app, actions);
}

/// "When I close the window": Ask, Keep running in the tray, Quit (`CloseAction`). Only where
/// there is a tray icon: without one, closing always quits.
fn close_action(ui: &mut Ui, app: &CsmApp, actions: &mut Vec<Action>) {
    if !app.background.tray_available {
        return;
    }
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("When I close the window").font(theme::regular(text::BODY)).color(t.fg));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            use csm_core::models::CloseAction;
            let choices = [
                (CloseAction::Ask, "Ask", "Ask every time"),
                (CloseAction::Tray, "Tray", "Keep running in the tray"),
                (CloseAction::Quit, "Quit", "Quit"),
            ];
            let labels = choices.map(|(_, _, l)| l);
            let sel = choices.iter().position(|(a, _, _)| *a == app.prefs.close_action);
            if let Some(i) = widgets::segmented(ui, "When I close the window", &labels, sel)
                && let Some((_, id, _)) = choices.get(i)
            {
                actions.push(set("CloseAction", json!(id)));
            }
        });
    });
    caption(ui, "In the tray, sessions keep working; the tray icon opens the window again, and its menu has Quit.");
}

fn notifications(ui: &mut Ui, app: &CsmApp, actions: &mut Vec<Action>) {
    section(ui, "Notifications");
    bool_setting(
        ui,
        app,
        "ShowToastNotifications",
        "Show system notifications",
        "When a session needs your approval or has a question, errors, or finishes while the window is in the background.",
        actions,
    );
    bool_setting(ui, app, "ShowNotificationSound", "Play a sound with notifications", "The system's notification sound.", actions);
    caption(
        ui,
        "Never for the session you are looking at, and at most one per session every 5 seconds. The tray icon shows how many sessions need you either way.",
    );
    if widgets::button(ui, ButtonKind::Secondary, Some("send"), "Send a test notification", None).clicked() {
        actions.push(Action::Tool("notification_test", json!({})));
    }
}

/// Settings › General › Updates: the daily check (`CheckForUpdates`) and "Check now" (`update_check`).
fn updates(ui: &mut Ui, app: &CsmApp, actions: &mut Vec<Action>) {
    section(ui, "Updates");
    bool_setting(
        ui,
        app,
        "CheckForUpdates",
        "Check for updates",
        "Once a day, ask GitHub whether a newer CodeShellManager is released and say so in the title bar. Nothing is downloaded, and nothing about you is sent.",
        actions,
    );
    if widgets::button(ui, ButtonKind::Secondary, Some("restart"), "Check now", None).clicked() {
        actions.push(Action::Tool("update_check", json!({})));
    }
}

/// Settings › General › Storage: the search index's limits (`IndexMaxMb`,
/// `ClaudeUsageRetentionDays`) and "Clean up now" (`storage_clean`). The engine also cleans up on
/// its own every hour; `storage_info` reports the sizes (shown as a toast).
fn storage(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    section(ui, "Storage");
    number_setting(ui, app, "IndexMaxMb", "Search index size limit", 100..=102_400, "MB", actions);
    caption(ui, "Terminal output is kept for search up to this size in total; the oldest lines go first. A closed session's output is removed 30 days after it was last seen.");
    number_setting(ui, app, "ClaudeUsageRetentionDays", "Keep Claude usage for", 0..=3650, "days", actions);
    caption(ui, "Older Claude usage rows are removed (0 keeps them all).");
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Secondary, Some("eraser"), "Clean up now", None).clicked() {
            actions.push(Action::Tool("storage_clean", json!({})));
        }
        if widgets::button(ui, ButtonKind::Ghost, Some("gauge"), "Show sizes", None).clicked() {
            actions.push(Action::Tool("storage_info", json!({})));
        }
    });
}

fn appearance(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    section(ui, "Theme");
    let cur = app.ui.settings.str("Theme").unwrap_or("dark").to_ascii_lowercase();
    let options = ["Dark", "Light", "System"];
    let ids = ["dark", "light", "system"];
    let sel = ids.iter().position(|i| *i == cur || (cur == "mocha" && *i == "dark") || (cur == "latte" && *i == "light"));
    if let Some(i) = widgets::segmented(ui, "Theme", &options, sel)
        && let Some(id) = ids.get(i)
    {
        actions.push(Action::Tool("theme_set", json!({"mode": id})));
    }
    caption(ui, "System follows the light or dark setting of your desktop. Dark mode draws the dark theme below, light mode the light one.");
    let themes = theme::registry(&app.model.addons);
    for (kind, title, key) in [(theme::ThemeKind::Mocha, "Dark theme", "dark"), (theme::ThemeKind::Latte, "Light theme", "light")] {
        section(ui, title);
        let chosen = if kind == theme::ThemeKind::Mocha { app.prefs.theme_dark.clone() } else { app.prefs.theme_light.clone() };
        let (_, fallback) = app.theme_palette(kind);
        let drawn = if fallback.is_some() { kind.base().builtin_id().to_owned() } else { chosen };
        for entry in themes.iter().filter(|e| e.palette.kind == kind) {
            if theme_row(ui, entry, title, entry.id == drawn) {
                actions.push(Action::Tool("theme_set", json!({key: entry.id})));
            }
        }
        if let Some(why) = fallback {
            caption(ui, &why);
        }
    }
    caption(ui, "Add-on packs can add themes (docs/addons.md, Themes). The Themes sample in Settings › Add-ons has Catppuccin Frappé and Macchiato and a high-contrast dark theme.");
    section(ui, "Interface");
    let zoom = csm_core::models::UI_ZOOM_RANGE;
    number_setting(ui, app, "UiZoomPercent", "Interface size", i64::from(*zoom.start())..=i64::from(*zoom.end()), "%", actions);
    caption(ui, "Text, icons and spacing of the whole window; terminals keep their own font size. Ctrl+= and Ctrl+- step it, Ctrl+0 resets it.");
    section(ui, "Terminal");
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Font").font(theme::regular(text::BODY)).color(t.fg));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let face = lock_face(app);
            ui.label(egui::RichText::new(face).font(theme::mono(text::MONO)).color(t.fg2));
        });
    });
    caption(ui, "The built-in JetBrains Mono, or the font passed with --font at start-up.");
    number_setting(ui, app, "TerminalFontPoints", "Font size (0 = default)", 0..=32, "pt", actions);
    number_setting(ui, app, "TerminalPadding", "Padding inside a pane", 0..=32, "pt", actions);
    bool_setting(
        ui,
        app,
        "AllowOsc52Clipboard",
        "Let programs copy to the clipboard",
        "OSC 52, as tmux, Claude Code's fullscreen mode and remote shells use it (up to 1 MB). Programs can never read the clipboard.",
        actions,
    );
    bool_setting(
        ui,
        app,
        "CleanClaudeCopies",
        "Clean up copies from Claude",
        "Copying a selection in a Claude Code pane drops its markers, frames and indentation and joins the lines Claude wrapped at the pane's edge. Ctrl+Alt+C copies a selection as shown.",
        actions,
    );
    bool_setting(
        ui,
        app,
        "ShiftEnterNewline",
        "Shift+Enter inserts a newline",
        "Keeps typing on a new line instead of sending the prompt. cmd.exe may clear the line instead.",
        actions,
    );
    section(ui, "Windows Terminal profiles");
    bool_setting(
        ui,
        app,
        "ImportWindowsTerminalProfiles",
        "Offer Windows Terminal profiles",
        "The New and Edit session panel gets a Profile row: a profile's colours, font size, cursor and padding are stamped onto the session.",
        actions,
    );
    caption(
        ui,
        "Read from Windows Terminal's settings (Stable, Preview or unpackaged). A session keeps its look if the profile changes later. Font face, weight, ligatures, blink, transparency and the retro effect are kept with the session but not drawn.",
    );
}

/// One theme to pick: its swatches, its name and where it comes from; a check when it is the one
/// drawn in that mode. True when clicked.
fn theme_row(ui: &mut Ui, entry: &theme::ThemeEntry, slot: &str, selected: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG), Sense::click());
    if selected || resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::md()), if selected { t.row_sel } else { t.row_hover });
    }
    let mut x = rect.min.x + space::ROW_X;
    let side = size::BADGE;
    for c in entry.palette.swatches() {
        let r = Rect::from_min_size(pos2(x, rect.center().y - side / 2.0), Vec2::splat(side));
        ui.painter().rect_filled(r, CornerRadius::same(radius::sm()), c);
        ui.painter().rect_stroke(r, CornerRadius::same(radius::sm()), Stroke::new(size::HAIRLINE, t.surface1), egui::StrokeKind::Inside);
        x += side + space::XXS;
    }
    x += space::SM;
    ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, &entry.name, theme::medium(text::BODY), t.fg);
    let mut right = rect.max.x - space::ROW_X;
    if selected {
        let ir = Rect::from_min_size(pos2(right - icon::MD, rect.center().y - icon::MD / 2.0), Vec2::splat(icon::MD));
        icons::paint(ui, ir, "check", icon::MD, t.blue);
    }
    right -= icon::MD + space::SM;
    let source = entry.source.as_deref().unwrap_or("built in");
    ui.painter().text(pos2(right, rect.center().y), Align2::RIGHT_CENTER, source, theme::regular(text::CAPTION), t.muted);
    let label = format!("{slot}: {}", entry.name);
    resp.widget_info(move || egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, label.clone()));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// The primary terminal face's name.
fn lock_face(app: &CsmApp) -> String {
    let host = crate::terminal::lock(&app.host);
    let d = host.renderer.fonts.describe();
    let name = d.split(" (").next().unwrap_or_default();
    // "<face> <size>pt": the face is what this row is about.
    let face = name.rsplit_once(' ').map_or(name, |(f, _)| f);
    if face.starts_with("JetBrainsMono") { "JetBrains Mono (built in)".to_string() } else { face.to_string() }
}

fn groups(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let model = app.model.clone();
    section(ui, "How groups appear");
    let options = ["Rail", "Headers", "Flat"];
    let sel = match model.group_mode {
        GroupMode::Rail => 0,
        GroupMode::Headers => 1,
        GroupMode::Flat => 2,
    };
    if let Some(i) = widgets::segmented(ui, "Group view", &options, Some(sel))
        && i != sel
    {
        let v = ["rail", "headers", "flat"].get(i).copied().unwrap_or("rail");
        actions.push(Action::Tool("view_set", json!({"view": v})));
    }
    caption(
        ui,
        "Rail: one group at a time, a tab per group, and the panes follow the tab. Headers: every group in one list, collapsible. Flat: no groups.",
    );
    caption(ui, "The same switch sits next to SESSIONS in the sidebar. Each group keeps its own layout.");
    section(ui, "Sidebar");
    let inbox = model.inbox_panel.open;
    if widgets::toggle(ui, inbox, "Show the Needs you inbox", Some("Ctrl+J jumps to the next session that needs you either way.")).clicked() {
        actions.push(Action::Tool("inbox_panel_set", json!({"open": !inbox})));
    }
    bool_setting(ui, app, "ShowGitBranch", "Show the branch on each row", "The branch name next to the status glyph.", actions);
}

fn mcp(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    section(ui, "Claude sessions");
    bool_setting(
        ui,
        app,
        "OfferMcpToClaude",
        "Offer CodeShellManager's tools to Claude sessions",
        "Claude sessions started here get `csm mcp` as an MCP server. Off by default: it changes the claude command line.",
        actions,
    );
    caption(ui, "Any MCP client can use them by hand: claude mcp add codeshell -- csm mcp");
    section(ui, "Always allowed");
    caption(ui, "Clients that answered “Always for this client” on a consent card. Revoke one and it asks again.");
    let grants = app.ui.settings.grants.clone();
    widgets::card_frame(&t).inner_margin(egui::Margin::symmetric(space::MD as i8, space::XS as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        if grants.is_empty() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG), Sense::hover());
            ui.painter().text(
                pos2(r.min.x, r.center().y),
                Align2::LEFT_CENTER,
                "No client has an “Always” grant.",
                theme::regular(text::BODY),
                t.muted,
            );
        }
        for (i, g) in grants.iter().enumerate() {
            let client = g["client"].as_str().unwrap_or_default().to_string();
            let tool = g["tool"].as_str().unwrap_or_default().to_string();
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG), Sense::hover());
            if i > 0 {
                ui.painter().hline(r.x_range(), r.min.y, Stroke::new(size::HAIRLINE, t.surface0));
            }
            let cr = ui.painter().text(pos2(r.min.x, r.center().y), Align2::LEFT_CENTER, &client, theme::medium(text::BODY), t.fg);
            let title = app.dispatch.tool(&tool).map_or(tool.clone(), |s| s.title);
            let b = Rect::from_min_max(pos2(r.max.x - size::MENU_W / 2.0 + space::XL, r.min.y + space::XS), pos2(r.max.x, r.max.y - space::XS));
            widgets::paint_text(
                ui,
                pos2(cr.max.x + space::SM, r.center().y),
                Align2::LEFT_CENTER,
                &format!("may run {title} ({tool})"),
                theme::regular(text::CAPTION),
                t.muted,
                b.min.x - cr.max.x - space::LG,
            );
            let label = format!("Revoke {client} · {tool}");
            let resp = ui
                .scope_builder(UiBuilder::new().max_rect(b).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| {
                    widgets::button(ui, ButtonKind::Danger, None, "Revoke", None)
                })
                .inner;
            resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
            if resp.clicked() {
                actions.push(Action::Tool("grant_revoke", json!({"client": client, "tool": tool})));
            }
        }
    });
}

/// umage.ai's links on the About page: (label, icon, url). Opened through the `open_url` tool.
pub const ABOUT_LINKS: [(&str, &str, &str); 4] = [
    ("umage.ai", "send", "https://umage.ai"),
    ("Product page", "terminal", "https://umage.ai/products/code-shell-manager/"),
    ("Source on GitHub", "branch", "https://github.com/umage-ai/CodeShellManager"),
    ("Report an issue", "help", "https://github.com/umage-ai/CodeShellManager/issues"),
];

/// What umage.ai does, in its own three focus areas (the About page's pitch).
const UMAGE_FOCUS: [(&str, &str); 3] = [
    (
        "Agentic development",
        "We build AI agents and agent-based systems, and make development teams productive with coding agents like the ones you run here.",
    ),
    ("AI in business", "Advice, implementation and transformation: AI readiness, prioritising use cases and rolling AI out across an organisation."),
    ("AI sovereignty", "Technological and data sovereignty for EU and Nordic organisations: hosting, model choice and data residency."),
];

fn about(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let info = app.ui.settings.info.clone();
    let s = |k: &str| info.get(k).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string)).unwrap_or_else(|| "…".into());

    // Hero: the logo, the name, what it is, the version.
    ui.horizontal(|ui| {
        let (logo, _) = ui.allocate_exact_size(Vec2::splat(size::ABOUT_LOGO), Sense::hover());
        ui.painter().rect_filled(logo, CornerRadius::same(radius::lg()), t.blue);
        icons::paint(ui, logo, "terminal", icon::HERO, t.on_accent);
        ui.add_space(space::LG);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("CodeShellManager").font(semibold(text::DISPLAY)).color(t.fg));
            ui.label(egui::RichText::new("A multi-terminal host for Claude Code & friends").font(theme::regular(text::BODY)).color(t.fg2));
            ui.label(egui::RichText::new(about_version_line(&s("version"), &s("platform"))).font(theme::regular(text::CAPTION)).color(t.muted));
        });
    });
    ui.add_space(space::XL);

    // Made by umage.ai: the official logo (the website's SVG, black on light, white on dark), the pitch, the links.
    egui::Frame::new()
        .fill(t.mantle)
        .stroke(Stroke::new(size::HAIRLINE, t.surface0))
        .corner_radius(CornerRadius::same(radius::lg()))
        .inner_margin(space::LG)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Made by ").font(theme::regular(text::TITLE)).color(t.muted));
                let logo = if t.dark() {
                    egui::include_image!("../../assets/brand/umage-ai-logo-white.svg")
                } else {
                    egui::include_image!("../../assets/brand/umage-ai-logo.svg")
                };
                ui.add(egui::Image::new(logo).fit_to_exact_size(vec2(f32::INFINITY, text::TITLE * 2.0)).alt_text("umage.ai logo"));
            });
            ui.add_space(space::XS);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "umage.ai is a Danish AI consultancy. We work as forward-deployed engineers with teams that want real value \
                         from AI. CodeShellManager is the tool we use every day to run many coding agents side by side, and it's free \
                         and open source.",
                    )
                    .font(theme::regular(text::BODY))
                    .color(t.fg),
                )
                .wrap(),
            );
            ui.add_space(space::MD);
            for (title, body) in UMAGE_FOCUS {
                ui.horizontal_top(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(space::MD, text::BODY), Sense::hover());
                    ui.painter().circle_filled(dot.center(), space::XXS + 1.0, t.brand);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(title).font(semibold(text::BODY)).color(t.fg));
                        ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(text::CAPTION)).color(t.muted)).wrap());
                    });
                });
                ui.add_space(space::SM);
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new("Want your team to get more out of coding agents? We'd love to hear from you.")
                        .font(theme::regular(text::BODY))
                        .color(t.fg2),
                )
                .wrap(),
            );
            ui.add_space(space::MD);
            ui.horizontal_wrapped(|ui| {
                for (i, (label, icon_name, url)) in ABOUT_LINKS.into_iter().enumerate() {
                    let kind = if i == 0 { ButtonKind::Primary } else { ButtonKind::Secondary };
                    if widgets::button(ui, kind, Some(icon_name), label, None).on_hover_text(url).clicked() {
                        actions.push(Action::Tool("open_url", json!({"url": url})));
                    }
                }
            });
        });
    ui.add_space(space::XL);

    // Diagnostics (what a bug report needs).
    ui.label(egui::RichText::new("Diagnostics").font(semibold(text::EMPH)).color(t.fg));
    ui.add_space(space::SM);
    egui::Grid::new("about-kv").num_columns(2).spacing(vec2(space::LG, space::SM)).show(ui, |ui| {
        let k = |ui: &mut Ui, s: &str| {
            ui.label(egui::RichText::new(s).font(theme::regular(text::BODY)).color(t.muted));
        };
        k(ui, "Version");
        ui.label(egui::RichText::new(format!("CodeShellManager {}", s("version"))).font(theme::regular(text::BODY)));
        ui.end_row();
        k(ui, "Platform");
        ui.label(egui::RichText::new(s("platform")).font(theme::regular(text::BODY)));
        ui.end_row();
        k(ui, "ConPTY in use");
        let conpty = info.get("conpty").and_then(Value::as_str).unwrap_or("not decided yet (no session has started)");
        ui.add(egui::Label::new(egui::RichText::new(conpty).font(theme::regular(text::BODY))).wrap());
        ui.end_row();
        k(ui, "Sessions");
        ui.label(egui::RichText::new(format!("{} live of {} · {} groups", s("live"), s("sessions"), s("groups"))).font(theme::regular(text::BODY)));
        ui.end_row();
        k(ui, "State file");
        let path = info.get("state_path").and_then(Value::as_str).map(String::from);
        let saving = info.get("saving").and_then(Value::as_bool).unwrap_or(false);
        let shown = match (&path, saving) {
            (Some(p), true) => p.clone(),
            (Some(p), false) => format!("{p} (not saved in this mode)"),
            (None, _) => "none: nothing is saved (CSM_CONFIG_DIR is empty or --clean)".into(),
        };
        ui.add(egui::Label::new(egui::RichText::new(&shown).font(theme::mono(text::MONO))).wrap());
        ui.end_row();
    });
    if let Some(p) = info.get("state_path").and_then(Value::as_str)
        && widgets::button(ui, ButtonKind::Secondary, Some("copy"), "Copy the state file's path", None).clicked()
    {
        actions.push(Action::CopyText(p.to_string()));
    }
    ui.add_space(space::SM);
    caption(ui, "The search index and notes live beside the state file; a portable marker beside the exe keeps everything there.");
}

/// The About hero's version line: "Version 2.0.0 · windows-x86_64", with "pre-release" between
/// them for a version that has a semver pre-release part (2.0.0-beta.4, 2.0.0-dev.12).
fn about_version_line(version: &str, platform: &str) -> String {
    if version.contains('-') { format!("Version {version} · pre-release · {platform}") } else { format!("Version {version} · {platform}") }
}

#[cfg(test)]
mod tests {
    use super::about_version_line;

    #[test]
    fn only_a_pre_release_version_says_so() {
        assert_eq!(about_version_line("2.0.0", "windows"), "Version 2.0.0 · windows");
        assert_eq!(about_version_line("2.0.0-beta.4", "windows"), "Version 2.0.0-beta.4 · pre-release · windows");
    }
}

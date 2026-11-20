//! The tray icon (Windows, macOS): the app's logo, a tooltip with the "needs you" count and a dot
//! on the logo while any session needs you, and a menu of tools: Show window (`window_show`), Next
//! that needs you (`inbox_next`), Quit (the window's own close path, ending in `app_quit`). A
//! left click or a double click shows the window.
//!
//! It lives on the main thread (the OS delivers its clicks through the event loop eframe pumps);
//! clicks arrive on a channel and wake the window, which acts on them in its next pass. Linux has
//! no tray here: its trays need a GTK main loop that eframe does not run.

use std::sync::mpsc::Receiver;

/// What the person picked on the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
pub enum TrayCommand {
    Show,
    NextNeedsYou,
    Quit,
}

#[cfg(any(windows, target_os = "macos"))]
mod imp {
    use std::sync::mpsc::{self, Receiver};

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::TrayCommand;

    const SIDE: u32 = 32;

    pub struct Tray {
        icon: TrayIcon,
        shown: Option<usize>,
    }

    fn icon(badge: bool) -> Option<Icon> {
        let d = csm_ui::notify::tray_icon(SIDE, badge);
        Icon::from_rgba(d.rgba, d.width, d.height).map_err(|e| log::warn!("tray: icon: {e}")).ok()
    }

    pub fn start(ctx: &egui::Context) -> Option<(Tray, Receiver<TrayCommand>)> {
        let (tx, rx) = mpsc::channel();
        let menu = Menu::new();
        let items = [
            MenuItem::with_id("show", "Show window", true, None),
            MenuItem::with_id("next", "Next that needs you", true, None),
            MenuItem::with_id("quit", "Quit", true, None),
        ];
        let sep = PredefinedMenuItem::separator();
        let added = [menu.append(&items[0]), menu.append(&items[1]), menu.append(&sep), menu.append(&items[2])];
        if let Some(Err(e)) = added.into_iter().find(Result::is_err) {
            log::warn!("tray: menu: {e}");
        }
        let built = TrayIconBuilder::new()
            .with_id("codeshell")
            .with_tooltip(csm_core::notify_rules::tray_tooltip(0))
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon(icon(false)?)
            .build();
        let icon = match built {
            Ok(i) => i,
            Err(e) => {
                log::warn!("tray: no tray icon ({e})");
                return None;
            }
        };
        let (menu_tx, menu_ctx) = (tx.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let cmd = match e.id.0.as_str() {
                "show" => TrayCommand::Show,
                "next" => TrayCommand::NextNeedsYou,
                "quit" => TrayCommand::Quit,
                _ => return,
            };
            let _ = menu_tx.send(cmd);
            menu_ctx.request_repaint();
        }));
        let click_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            let show = matches!(
                e,
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                    | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. }
            );
            if show {
                let _ = tx.send(TrayCommand::Show);
                click_ctx.request_repaint();
            }
        }));
        Some((Tray { icon, shown: None }, rx))
    }

    impl Tray {
        /// Follows the "needs you" count: the tooltip, and the dot on the logo.
        pub fn set_needs_you(&mut self, n: usize) {
            if self.shown == Some(n) {
                return;
            }
            let badge_changed = self.shown.is_none_or(|old| (old > 0) != (n > 0));
            self.shown = Some(n);
            if let Err(e) = self.icon.set_tooltip(Some(csm_core::notify_rules::tray_tooltip(n))) {
                log::warn!("tray: tooltip: {e}");
            }
            if badge_changed
                && let Err(e) = self.icon.set_icon(icon(n > 0))
            {
                log::warn!("tray: icon: {e}");
            }
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            // The handlers hold the window's context; let them go with the icon.
            MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
            TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use std::sync::mpsc::Receiver;

    use super::TrayCommand;

    pub struct Tray;

    pub fn start(_ctx: &egui::Context) -> Option<(Tray, Receiver<TrayCommand>)> {
        log::info!("tray: no tray icon on this platform");
        None
    }

    impl Tray {
        pub fn set_needs_you(&mut self, _n: usize) {}
    }
}

/// The tray and the clicks it delivers; `None` where there is no tray.
pub struct TrayHost {
    tray: imp::Tray,
    rx: Receiver<TrayCommand>,
}

impl TrayHost {
    pub fn start(ctx: &egui::Context) -> Option<TrayHost> {
        imp::start(ctx).map(|(tray, rx)| TrayHost { tray, rx })
    }

    /// What was picked since the last call.
    pub fn commands(&self) -> Vec<TrayCommand> {
        self.rx.try_iter().collect()
    }

    pub fn set_needs_you(&mut self, n: usize) {
        self.tray.set_needs_you(n);
    }
}

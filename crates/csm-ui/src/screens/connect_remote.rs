//! "Connect to another CodeShellManager…": pairs this laptop with an office from the link the office
//! shows, shows the check code to compare with the office's screen, and lists the paired offices with
//! Connect (a remote window) and Forget.
//!
//! The pairing itself is the engine's `peer_client_*` tools; this screen only asks and shows the
//! answers. Pairing is started only from this window, by the person (the tools refuse every other
//! caller). The status is polled while a pairing runs, on the frame clock, never on this thread.

use std::time::Duration;

use egui::{Align, Id, Layout, Order, RichText, Ui, pos2};
use serde_json::json;

use crate::app::{Action, CsmApp, Overlay};
use crate::backend::Call;
use crate::remote_window::{ConnectDialog, PairPhase};
use crate::theme::{self, Tokens, size, space, text};
use crate::widgets::{self, ButtonKind};

/// How often a running pairing's status is read, in seconds.
const POLL_EVERY: f64 = 0.4;
/// The dialog's width.
const WIDTH: f32 = 460.0;

/// Asks for the pairing status while a pairing runs (once every [`POLL_EVERY`]).
pub fn poll(app: &mut CsmApp, ctx: &egui::Context) {
    if app.ui.overlay != Overlay::ConnectRemote || !app.ui.connect.busy() {
        return;
    }
    let now = ctx.input(|i| i.time);
    if now - app.ui.connect_polled >= POLL_EVERY {
        app.ui.connect_polled = now;
        app.dispatch.submit("peer_client_pair_status", json!({}));
    }
    ctx.request_repaint_after(Duration::from_millis(200));
}

/// Takes the dialog's own calls' answers. Other calls are not its business.
pub fn on_reply(app: &mut CsmApp, _ctx: &egui::Context, call: &Call, reply: &csm_engine::Reply) {
    let mut refresh = false;
    let d = &mut app.ui.connect;
    match (call.id.as_str(), reply) {
        ("peer_hosts_list", Ok(out)) => d.apply_hosts(&out.data),
        ("peer_hosts_list", Err(e)) => {
            d.hosts_loaded = true;
            d.error = Some(e.to_string());
        }
        ("peer_client_pair" | "peer_client_pair_status" | "peer_client_pair_cancel", Ok(out)) => {
            d.apply_status(&out.data);
            // A paired office joins the list at once.
            refresh = matches!(d.phase, PairPhase::Paired(_));
        }
        ("peer_client_pair" | "peer_client_pair_status", Err(e)) => {
            d.phase = PairPhase::Failed(e.to_string());
        }
        ("peer_host_forget", Ok(_)) => refresh = true,
        ("peer_host_forget", Err(e)) => d.error = Some(e.to_string()),
        _ => {}
    }
    if refresh {
        app.dispatch.submit("peer_hosts_list", json!({}));
    }
}

/// The dialog, over a scrim, when it is open.
pub fn show(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("connect-remote-scrim")).order(Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
        ui.painter().rect_filled(rect, 0.0, t.scrim);
        if resp.clicked() {
            actions.push(Action::CloseOverlays);
        }
    });
    let pos = pos2(screen.center().x - WIDTH / 2.0, screen.min.y + size::PALETTE_TOP);
    egui::Area::new(Id::new("connect-remote")).order(Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        widgets::card_frame(&t).show(ui, |ui| {
            ui.set_width(WIDTH);
            body(ui, &mut app.ui.connect, actions);
        });
    });
}

fn caption(ui: &mut Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(RichText::new(s).font(theme::regular(text::CAPTION)).color(t.muted)).wrap());
}

fn body(ui: &mut Ui, d: &mut ConnectDialog, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    ui.label(RichText::new("Connect to another CodeShellManager").font(theme::semibold(text::BODY)).color(t.fg));
    caption(ui, "On the other CodeShellManager, turn on Settings > Remote access. It shows a pairing link: paste it here, then compare the four digits on both screens.");
    ui.add_space(space::SM);

    ui.label(RichText::new("Pairing link").font(theme::medium(text::CAPTION)).color(t.fg2));
    ui.add(egui::TextEdit::singleline(&mut d.link).id(Id::new("connect-link")).hint_text("csm://pair#...").desired_width(f32::INFINITY));
    ui.label(RichText::new("This laptop's name (optional)").font(theme::medium(text::CAPTION)).color(t.fg2));
    ui.add(egui::TextEdit::singleline(&mut d.name).id(Id::new("connect-name")).hint_text("The computer's name").desired_width(f32::INFINITY));
    ui.add_space(space::SM);

    ui.horizontal(|ui| {
        let can = d.can_pair();
        if ui.add_enabled_ui(can, |ui| widgets::button(ui, ButtonKind::Primary, None, "Pair", None)).inner.clicked() {
            actions.push(Action::ConnectRemotePair);
        }
        if d.busy() && widgets::button(ui, ButtonKind::Secondary, None, "Cancel pairing", None).clicked() {
            actions.push(Action::ConnectRemoteCancel);
        }
    });

    if let Some(e) = d.error.clone() {
        ui.add(egui::Label::new(RichText::new(e).font(theme::regular(text::CAPTION)).color(t.red)).wrap());
    }
    match d.phase.clone() {
        PairPhase::Idle => {}
        PairPhase::Connecting => caption(ui, "Connecting to the other CodeShellManager..."),
        PairPhase::CheckCode(code) => {
            ui.add_space(space::XS);
            ui.label(RichText::new("Check code").font(theme::medium(text::CAPTION)).color(t.muted));
            ui.label(RichText::new(code).font(theme::semibold(text::TITLE)).color(t.fg));
            caption(ui, "Compare these four digits with the other screen. Its person presses Allow there; nothing is stored until then.");
        }
        PairPhase::Paired(name) => {
            ui.add(egui::Label::new(RichText::new(format!("Allowed. {name} is stored: connect to it below.")).font(theme::medium(text::CAPTION)).color(t.green)).wrap());
        }
        PairPhase::Failed(why) => {
            ui.add(egui::Label::new(RichText::new(format!("The pairing did not happen. {why}")).font(theme::regular(text::CAPTION)).color(t.red)).wrap());
        }
        PairPhase::Cancelled => caption(ui, "Pairing cancelled. Nothing was stored."),
    }

    ui.add_space(space::MD);
    ui.separator();
    ui.add_space(space::SM);
    ui.label(RichText::new("Paired offices").font(theme::semibold(text::BODY)).color(t.fg));
    if !d.hosts_loaded {
        caption(ui, "Reading the list...");
    } else if d.hosts.is_empty() {
        caption(ui, "No paired offices yet. Pair one above.");
    }
    for h in d.hosts.clone() {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(&h.name).font(theme::medium(text::BODY)).color(t.fg));
                caption(ui, &h.address);
            });
            // Right to left: Connect sits at the edge, Forget beside it.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::button(ui, ButtonKind::Primary, None, "Connect", None).clicked() {
                    actions.push(Action::ConnectRemoteOpen { host_id: h.host_id.clone(), name: h.name.clone() });
                }
                if widgets::button(ui, ButtonKind::Ghost, None, "Forget", None).clicked() {
                    actions.push(Action::ConnectRemoteForget { host_id: h.host_id.clone(), name: h.name.clone() });
                }
            });
        });
    }

    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::button(ui, ButtonKind::Ghost, None, "Close", None).clicked() {
                actions.push(Action::CloseOverlays);
            }
        });
    });
}

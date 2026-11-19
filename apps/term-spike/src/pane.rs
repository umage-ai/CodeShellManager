//! A spike pane: one live terminal (`csm_ui::terminal::LocalTerminal`) plus the widget state of
//! the place it is shown (`TerminalView`), and where it lives (grid, floating, torn off).

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use csm_ui::terminal::{LocalTerminal, TerminalView};
use egui::{Context, Id, ViewportId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Grid,
    Floating,
    TornOff,
}

pub struct Pane {
    pub id: usize,
    pub name: String,
    pub placement: Placement,
    pub term: Arc<LocalTerminal>,
    pub view: TerminalView,
}

impl Pane {
    pub fn new(id: usize, name: String, argv: Vec<OsString>, cwd: Option<PathBuf>, placement: Placement, ctx: &Context) -> Self {
        let ctx = ctx.clone();
        let repaint: csm_ui::terminal::Repaint = Arc::new(move || ctx.request_repaint_of(ViewportId::ROOT));
        let term = Arc::new(LocalTerminal::new(name.clone(), argv, cwd, csm_term::Palette::default(), repaint));
        Self { id, name, placement, term, view: TerminalView::new(Id::new(("csm-term-pane", id))) }
    }
}

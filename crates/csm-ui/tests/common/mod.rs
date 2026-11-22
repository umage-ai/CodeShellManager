//! Shared test setup: the app on the static demo backend (no processes), a fixed clock, a real
//! engine with the demo tools, at the mock-up's 1280×800.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub use csm_ui::ToolPort;
use csm_ui::CsmApp;
use csm_ui::demo::{DemoBackend, DemoOptions};
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::theme::ThemeKind;
use egui::vec2;
use egui_kittest::Harness;

/// 2026-11-19 09:42 UTC, so ages ("4m") are stable.
pub const NOW_MS: u64 = 1_795_513_320_000;

pub fn backend(theme: ThemeKind) -> DemoBackend {
    DemoBackend::new(DemoOptions { live: false, now_ms: Some(NOW_MS), theme, ..DemoOptions::default() })
}

pub fn app_with(backend: DemoBackend) -> CsmApp {
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    CsmApp::new(Box::new(csm_ui::EnginePort::new(csm_ui::demo_engine().unwrap())), Box::new(backend), TerminalRenderer::new(fonts))
}

pub fn harness(app: CsmApp) -> Harness<'static, CsmApp> {
    Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app)
}

pub fn harness_wgpu(app: CsmApp, ppp: f32) -> Harness<'static, CsmApp> {
    harness_wgpu_sized(app, ppp, vec2(1280.0, 800.0))
}

pub fn harness_wgpu_sized(app: CsmApp, ppp: f32, size: egui::Vec2) -> Harness<'static, CsmApp> {
    Harness::builder()
        .with_size(size)
        .with_step_dt(1.0 / 60.0)
        .with_pixels_per_point(ppp)
        .wgpu()
        .build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app)
}

/// Steps until the engine has answered every submitted call, for up to 15 s. The bound is time,
/// not a step count: a loaded CI runner (or a tool that reads the disk, like `profile_list`) can
/// take far longer than a developer machine, and a passing run returns as soon as it's idle.
pub fn settle(h: &mut Harness<'static, CsmApp>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        h.step();
        if !h.state().dispatch.busy() {
            h.step();
            h.step();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the engine did not answer within 15 s");
}

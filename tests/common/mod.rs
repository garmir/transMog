//! shared helpers for driving the app through egui_kittest
#![allow(dead_code)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui_wgpu::{RenderState, RendererOptions, WgpuConfiguration};
use egui::Role;
use egui_kittest::kittest::{By, Queryable};
use egui_kittest::wgpu::WgpuTestRenderer;
use egui_kittest::{Harness, SnapshotOptions};
use transmog::ui::App;

pub type AppHarness = Harness<'static, App>;

const BUILTIN_CAMERA: &str = "USB 2.0 Camera";

/// wgpu's request futures resolve without an executor on native targets,
/// so a spin on a no-op waker is enough
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

/// a headless render state on the adapter kittest prefers: lavapipe when
/// it is installed, else the gpu. None without any adapter, which is the
/// case inside the nix build sandbox
pub fn render_state() -> Option<RenderState> {
    let setup = egui_kittest::wgpu::default_wgpu_setup();
    let instance = block_on(setup.new_instance());
    let config = WgpuConfiguration {
        wgpu_setup: setup,
        ..Default::default()
    };
    block_on(RenderState::create(
        &config,
        &instance,
        None,
        RendererOptions::PREDICTABLE,
    ))
    .ok()
}

/// where snapshots land: TRANSMOG_SNAPSHOT_DIR, else a directory under
/// the system temp dir, never inside the repository
pub fn snapshot_dir() -> PathBuf {
    let dir = std::env::var_os("TRANSMOG_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("transmog-snapshots"));
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    dir
}

/// the app with devices scanned and nothing started; the harness and the
/// app share one render state when an adapter exists, and the app shows
/// "no renderer" otherwise
pub fn harness_no_camera() -> AppHarness {
    let render_state = render_state();
    // the combo boxes and the centre drag values have no accessible
    // name, which the default check treats as a failure
    let mut builder = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_accessibility_check(false)
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()));
    if let Some(rs) = &render_state {
        builder = builder.renderer(WgpuTestRenderer::from_render_state(rs.clone()));
    }
    // App::show replaces this context with the harness's own on its
    // first frame
    let app = App::from_parts(&egui::Context::default(), render_state);
    builder.build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

/// the app capturing from the built-in camera on /dev/video0; None when
/// the node is missing or the capture fails to start (busy, unplugged)
pub fn harness_builtin_camera() -> Option<AppHarness> {
    if !Path::new("/dev/video0").exists() {
        eprintln!("no /dev/video0");
        return None;
    }
    let mut harness = harness_no_camera();
    let named = harness
        .state()
        .selected_device()
        .is_some_and(|d| d.name.contains(BUILTIN_CAMERA))
        || harness.state().capture_config().is_some();
    if !named {
        eprintln!("no capture device enumerated");
        return None;
    }
    harness.state_mut().autostart_with(BUILTIN_CAMERA);
    harness.step();
    if !harness.state().capture_running() {
        eprintln!("builtin camera not started: {}", harness.state().status());
        return None;
    }
    Some(harness)
}

/// steps `n` frames with a pause between them so the capture thread can
/// deliver; `run` cannot be used while capturing because the app keeps
/// asking for repaints
pub fn wait_frames(harness: &mut AppHarness, n: usize) {
    for _ in 0..n {
        harness.step();
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// sets a slider through the accesskit SetValue action, which egui's
/// Slider applies on the next frame. The slider's number field shares
/// its label, so the query is pinned to the slider role
pub fn set_slider(harness: &mut AppHarness, label: &str, value: f64) {
    harness
        .get_by_role_and_label(Role::Slider, label)
        .set_value_accesskit(value);
    harness.step();
}

pub fn click(harness: &mut AppHarness, label: &str) {
    harness.get_by_role_and_label(Role::Button, label).click();
    harness.step();
}

/// opens the combo box whose current text is `selected` and picks
/// `item`; a combo box carries its selected text as its value, not as a
/// label
pub fn combo_select(harness: &mut AppHarness, selected: &str, item: &str) {
    harness
        .get(By::new().role(Role::ComboBox).value(selected))
        .click();
    // the popup area is laid out on the frame after the click
    harness.run_ok();
    harness.get_by_label(item).click();
    harness.run_ok();
}

/// the text of the Label widget whose text contains `part`; accesskit
/// keeps a Role::Label's text in its value, not its label
pub fn label_containing(harness: &AppHarness, part: &str) -> String {
    harness
        .get(By::new().role(Role::Label).label_contains(part))
        .value()
        .expect("label text")
}

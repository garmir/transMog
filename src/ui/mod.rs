mod controls_panel;
mod devices;
mod fx_panel;
mod live;
mod overlay;
mod preview;
mod qa_panel;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui_wgpu::RenderState;
use parking_lot::Mutex;

use crate::capture::{
    self, tap, Capture, CaptureConfig, DeviceInfo, FormatInfo, SharedTap, Tap, TapStatus,
};
use crate::controls::preset::{self, Preset, PresetFormat, Readback};
use crate::controls::{ControlInfo, Controls};

const CONTROL_REFRESH: Duration = Duration::from_secs(1);

/// indices into `App::formats` and its nested size and fps lists
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Selection {
    pub format: usize,
    pub size: usize,
    pub fps: usize,
}

pub struct App {
    ctx: egui::Context,
    render_state: Option<RenderState>,
    devices: Vec<DeviceInfo>,
    selected_device: Option<usize>,
    formats: Vec<FormatInfo>,
    selection: Selection,
    capture: Option<Capture>,
    controls: Option<Controls>,
    control_infos: Vec<ControlInfo>,
    controls_queried: Instant,
    preview: Option<preview::Preview>,
    status: String,
    presets: Vec<String>,
    preset_ui: qa_panel::State,
    readback: Vec<Readback>,
    /// wanted against read back, for the last preset with a format line
    readback_format: Option<(String, String)>,
    tap: Option<SharedTap>,
    tap_dir: PathBuf,
    overlay: overlay::Overlay,
    show_overlay: bool,
    live: live::Live,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> App {
        let mut app = App::from_parts(&cc.egui_ctx, cc.wgpu_render_state.clone());
        app.autostart_with("C920");
        app
    }

    /// everything but the autostart, so a test harness can build the app
    /// on its own context and render state and pick the device itself
    pub fn from_parts(ctx: &egui::Context, render_state: Option<RenderState>) -> App {
        let preview = render_state.as_ref().map(preview::Preview::new);
        let mut app = App {
            ctx: ctx.clone(),
            render_state,
            devices: Vec::new(),
            selected_device: None,
            formats: Vec::new(),
            selection: Selection::default(),
            capture: None,
            controls: None,
            control_infos: Vec::new(),
            controls_queried: Instant::now(),
            preview,
            status: String::new(),
            presets: preset::list(),
            preset_ui: qa_panel::State::default(),
            readback: Vec::new(),
            readback_format: None,
            tap: None,
            tap_dir: tap::default_dir(),
            overlay: overlay::Overlay::new(tap::default_dir().join("overlay")),
            show_overlay: true,
            live: live::Live::new(tap::default_dir().join("control")),
        };
        app.status = if app.preview.is_none() {
            "no wgpu render state: the preview needs the wgpu backend".into()
        } else {
            "idle".into()
        };
        app.rescan();
        app
    }

    /// opens on the first device whose name contains `prefer` when one is
    /// plugged in, else on the first device, so the window never comes
    /// up empty
    pub fn autostart_with(&mut self, prefer: &str) {
        if self.devices.is_empty() {
            self.status = "no capture devices found".into();
            return;
        }
        let preferred = self
            .devices
            .iter()
            .position(|d| d.name.contains(prefer))
            .unwrap_or(0);
        if self.selected_device != Some(preferred) {
            self.select_device(preferred);
        }
        self.start();
    }

    /// re-enumerates devices and keeps the current one selected when it
    /// is still present
    fn rescan(&mut self) {
        let current = self
            .selected_device
            .and_then(|i| self.devices.get(i))
            .map(|d| d.path.clone());
        self.devices = capture::list_devices();
        let index = current
            .and_then(|path| self.devices.iter().position(|d| d.path == path))
            .or(if self.devices.is_empty() {
                None
            } else {
                Some(0)
            });
        match index {
            Some(i) if Some(i) != self.selected_device => self.select_device(i),
            Some(_) => {}
            None => {
                self.stop();
                self.selected_device = None;
                self.formats.clear();
                self.selection = Selection::default();
            }
        }
    }

    fn select_device(&mut self, index: usize) {
        self.stop();
        self.selected_device = Some(index);
        self.formats.clear();
        self.selection = Selection::default();
        let Some(device) = self.devices.get(index) else {
            return;
        };
        match capture::list_formats(&device.path) {
            Ok(formats) => {
                self.formats = formats;
                self.selection = devices::preferred_selection(&self.formats);
            }
            Err(e) => self.status = format!("{}: {e:#}", device.path.display()),
        }
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn capture_running(&self) -> bool {
        self.capture.is_some()
    }

    pub fn selected_device(&self) -> Option<&DeviceInfo> {
        self.devices.get(self.selected_device?)
    }

    pub fn selected_format(&self) -> Option<&FormatInfo> {
        self.formats.get(self.selection.format)
    }

    /// the cached v4l2 controls of the running capture; empty when idle
    pub fn control_infos(&self) -> &[ControlInfo] {
        &self.control_infos
    }

    /// the preview's pass stack; empty without a renderer. reach a
    /// concrete pass through `Pass::as_any`
    pub fn passes(&self) -> &[Box<dyn crate::render::Pass>] {
        self.preview.as_ref().map_or(&[], preview::Preview::passes)
    }

    /// the device, format, size and fps the start button would use
    pub fn capture_config(&self) -> Option<CaptureConfig> {
        let device = self.devices.get(self.selected_device?)?;
        let format = self.formats.get(self.selection.format)?;
        let size = format.sizes.get(self.selection.size)?;
        let fps = size.fps.get(self.selection.fps).copied().unwrap_or(30);
        Some(CaptureConfig {
            path: device.path.clone(),
            format: format.format,
            width: size.width,
            height: size.height,
            fps,
        })
    }

    fn start(&mut self) {
        self.stop();
        let Some(cfg) = self.capture_config() else {
            self.status = "nothing to capture: no device or format selected".into();
            return;
        };
        if let Some(preview) = &mut self.preview {
            preview.reset();
        }
        match Capture::start(cfg.clone()) {
            Ok(capture) => {
                let ctx = self.ctx.clone();
                capture.set_wake(move || ctx.request_repaint());
                capture.set_tap(self.tap.clone());
                if let Some(out) = self.preview.as_mut().and_then(preview::Preview::ndi_mut) {
                    out.set_fps(capture.actual().fps);
                }
                self.capture = Some(capture);
            }
            Err(e) => {
                self.status = format!("start {}: {e:#}", cfg.path.display());
                return;
            }
        }
        self.status = format!(
            "capturing {:?} {}x{}@{} from {}",
            cfg.format,
            cfg.width,
            cfg.height,
            cfg.fps,
            cfg.path.display()
        );
        match Controls::open(&cfg.path) {
            Ok(controls) => {
                self.controls = Some(controls);
                self.query_controls();
            }
            Err(e) => self.status += &format!("; no controls: {e:#}"),
        }
    }

    fn stop(&mut self) {
        self.capture = None;
        self.controls = None;
        self.control_infos.clear();
    }

    /// the tap keeps its sequence across capture restarts; none turns it
    /// off. the overlay file is read from the same directory
    pub fn set_tap(&mut self, dir: Option<PathBuf>) {
        let Some(dir) = dir else {
            self.tap = None;
            if let Some(capture) = &self.capture {
                capture.set_tap(None);
            }
            return;
        };
        match Tap::open(&dir, tap::DEFAULT_SLOTS) {
            Ok(tap) => {
                let tap = Arc::new(Mutex::new(tap));
                if let Some(capture) = &self.capture {
                    capture.set_tap(Some(Arc::clone(&tap)));
                }
                self.tap = Some(tap);
                self.overlay = overlay::Overlay::new(dir.join("overlay"));
                self.follow_tap_dir(&dir);
                self.tap_dir = dir;
            }
            Err(e) => self.status = format!("tap: {e:#}"),
        }
    }

    /// sends the effected view as the NDI source `HOST (transMog)`
    pub fn set_ndi(&mut self, on: bool) {
        let (Some(preview), Some(rs)) = (&mut self.preview, &self.render_state) else {
            self.status = "ndi: no renderer".into();
            return;
        };
        match preview.set_ndi(rs, on) {
            Ok(()) => {
                let fps = self.capture.as_ref().map(|c| c.actual().fps);
                if let (Some(out), Some(fps)) = (preview.ndi_mut(), fps) {
                    out.set_fps(fps);
                }
            }
            Err(e) => self.status = format!("ndi: {e:#}"),
        }
    }

    /// the size NDI out sends at, now and after it is next turned on
    pub fn set_ndi_size(&mut self, size: (u32, u32)) {
        match &mut self.preview {
            Some(preview) => preview.set_ndi_size(size),
            None => self.status = "ndi: no renderer".into(),
        }
    }

    pub fn ndi_size(&self) -> Option<(u32, u32)> {
        self.preview.as_ref().map(preview::Preview::ndi_size)
    }

    pub fn ndi_stats(&self) -> Option<crate::ndi::Stats> {
        self.preview.as_ref()?.ndi().map(crate::ndi::Output::stats)
    }

    pub fn tap_status(&self) -> Option<TapStatus> {
        self.tap.as_ref().map(|t| t.lock().status())
    }

    /// the format line first, which restarts the capture when it differs,
    /// then the controls; everything is read back into `readback`. with
    /// the qa lock on, another preset leaves the held controls alone.
    /// true when every value read back as set
    pub fn apply_preset(&mut self, name: &str) -> bool {
        self.preset_ui.choice = Some(name.to_string());
        let mut preset = match preset::load(name) {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("preset {name}: {e:#}");
                return false;
            }
        };
        let mut held = 0;
        if name != crate::live::LOCK_PRESET {
            preset.controls.retain(|(key, _)| {
                let keep = !self.held(key);
                held += usize::from(!keep);
                keep
            });
        }
        self.readback.clear();
        self.readback_format = None;
        if let Some(want) = &preset.format {
            match devices::find_selection(&self.formats, want) {
                Some(selection) => {
                    if selection != self.selection || self.capture.is_none() {
                        self.selection = selection;
                        self.start();
                    }
                }
                None => {
                    self.status = format!("preset {name}: the camera does not offer {want}");
                    return false;
                }
            }
            let got = self
                .capture
                .as_ref()
                .map(|c| PresetFormat::from(c.actual()).to_string())
                .unwrap_or_else(|| "not capturing".into());
            self.readback_format = Some((want.to_string(), got));
        }
        let Some(controls) = &self.controls else {
            self.status = format!("preset {name}: no camera controls open");
            return false;
        };
        match preset.apply(controls) {
            Ok(readback) => self.readback = readback,
            Err(e) => {
                self.status = format!("preset {name}: {e:#}");
                return false;
            }
        }
        self.query_controls();
        let off = self.readback.iter().filter(|r| !r.ok()).count()
            + usize::from(self.readback_format.as_ref().is_some_and(|(w, g)| w != g));
        self.status = if off == 0 {
            format!("preset {name}: every value read back as set")
        } else {
            format!("preset {name}: {off} values read back different")
        };
        if held > 0 {
            self.status += &format!(", {held} held by the qa lock");
        }
        for r in &self.readback {
            tracing::info!(preset = name, control = %r.name, wanted = r.wanted, got = ?r.got, error = ?r.error);
        }
        off == 0
    }

    /// the last apply's read-back
    pub fn readback(&self) -> &[Readback] {
        &self.readback
    }

    fn save_preset(&mut self, name: &str) {
        let format = self
            .capture
            .as_ref()
            .map(|c| PresetFormat::from(c.actual()));
        let preset = Preset::from_current(format, &self.control_infos);
        match preset::save(name, &preset) {
            Ok(path) => {
                self.status = format!("saved {}", path.display());
                self.presets = preset::list();
                self.preset_ui.choice = Some(name.to_string());
            }
            Err(e) => self.status = format!("save preset: {e:#}"),
        }
    }

    fn apply_qa_event(&mut self, event: qa_panel::Event) {
        match event {
            qa_panel::Event::Apply(name) => {
                self.apply_preset(&name);
            }
            qa_panel::Event::Save(name) => self.save_preset(&name),
            qa_panel::Event::Reload => self.presets = preset::list(),
            qa_panel::Event::Tap(on) => {
                let dir = self.tap_dir.clone();
                self.set_tap(on.then_some(dir));
            }
            qa_panel::Event::Ndi(on) => self.set_ndi(on),
        }
    }

    fn query_controls(&mut self) {
        self.controls_queried = Instant::now();
        let Some(controls) = &self.controls else {
            return;
        };
        match controls.query() {
            Ok(infos) => self.control_infos = infos,
            Err(e) => self.status = format!("query controls: {e:#}"),
        }
    }

    fn apply_control_actions(&mut self, actions: Vec<controls_panel::Action>) {
        if actions.is_empty() {
            return;
        }
        let reset_all = actions
            .iter()
            .any(|a| matches!(a, controls_panel::Action::ResetAll));
        if let Some(controls) = &self.controls {
            for action in actions {
                let result = match action {
                    controls_panel::Action::Set(id, value) => controls.set(id, value),
                    controls_panel::Action::Reset(id) => controls.reset(id),
                    controls_panel::Action::ResetAll => controls.reset_all(),
                };
                if let Err(e) = result {
                    self.status = format!("{action:?}: {e:#}");
                }
            }
        }
        // reset all also resets what the lock holds
        if reset_all && self.live.qa_lock {
            self.hold();
        }
        self.query_controls();
    }

    fn show_side_panel(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        let mut qa_events = Vec::new();
        egui::Panel::right("side")
            .resizable(true)
            .default_size(320.0)
            .size_range(200.0..=600.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::CollapsingHeader::new("lens")
                            .default_open(true)
                            .show(ui, |ui| {
                                if let Some(preview) = &mut self.preview {
                                    fx_panel::show(ui, preview.passes_mut());
                                    // Pass::ui reports no change, so any input
                                    // that can edit a widget counts as one
                                    if edited_this_frame(ui.ctx()) {
                                        preview.invalidate();
                                    }
                                } else {
                                    ui.weak("no renderer");
                                }
                            });
                        egui::CollapsingHeader::new("presets")
                            .default_open(true)
                            .show(ui, |ui| {
                                qa_events.extend(qa_panel::show_presets(
                                    ui,
                                    &mut self.preset_ui,
                                    &self.presets,
                                    &self.readback,
                                    self.readback_format.as_ref(),
                                ));
                            });
                        egui::CollapsingHeader::new("qa tap")
                            .default_open(false)
                            .show(ui, |ui| {
                                let status = self.tap_status();
                                qa_events.extend(qa_panel::show_tap(
                                    ui,
                                    status.as_ref(),
                                    &self.tap_dir,
                                    &mut self.show_overlay,
                                ));
                            });
                        egui::CollapsingHeader::new("ndi")
                            .default_open(false)
                            .show(ui, |ui| {
                                let stats = self.ndi_stats();
                                qa_events.extend(qa_panel::show_ndi(ui, stats.as_ref()));
                                if stats.is_some() {
                                    ui.ctx().request_repaint_after(CONTROL_REFRESH);
                                }
                            });
                        egui::CollapsingHeader::new("live control")
                            .default_open(false)
                            .show(ui, |ui| live::show(ui, self));
                        egui::CollapsingHeader::new("camera")
                            .default_open(true)
                            .show(ui, |ui| {
                                let held = |key: &str| self.held(key);
                                actions = controls_panel::show(ui, &self.control_infos, &held);
                            });
                    });
            });
        self.apply_control_actions(actions);
        for event in qa_events {
            self.apply_qa_event(event);
        }
    }

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        if let (Some(preview), Some(rs)) = (&mut self.preview, &self.render_state) {
            if let Some((latest, meta)) = self.capture.as_ref().and_then(Capture::latest_with_meta)
            {
                preview.upload(rs, &latest, Some(meta));
            }
            preview.render(rs);
        }
        let overlay = self.show_overlay && self.tap.is_some();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(12)))
            .show(ui, |ui| match &self.preview {
                Some(preview) => {
                    if let Some((rect, size)) = preview.show(ui) {
                        if overlay {
                            let bent = preview.passes().iter().any(|p| p.enabled());
                            self.overlay.show(ui, rect, size, !bent);
                            // the overlay file changes without a new frame
                            ui.ctx().request_repaint_after(Duration::from_millis(250));
                        }
                    }
                }
                None => {
                    ui.centered_and_justified(|ui| ui.weak("preview unavailable"));
                }
            });
    }
}

/// pointer buttons, keys, text, wheel and touch; a bare pointer move
/// edits nothing
fn edited_this_frame(ctx: &egui::Context) -> bool {
    ctx.input(|i| {
        i.pointer.any_down()
            || i.events.iter().any(|e| {
                !matches!(
                    e,
                    egui::Event::PointerMoved(_)
                        | egui::Event::MouseMoved(_)
                        | egui::Event::PointerGone
                        | egui::Event::ModifiersChanged(_)
                        | egui::Event::WindowFocused(_)
                )
            })
    })
}

impl App {
    /// the whole window: top bar, status line, side panel and preview.
    /// `eframe::App::ui` and the test harness both call this
    pub fn show(&mut self, ui: &mut egui::Ui) {
        // a test harness builds its context after the app, so the wake
        // target for new frames follows the ui that draws them
        self.ctx = ui.ctx().clone();
        if let Some(reason) = self.capture.as_ref().and_then(Capture::finished) {
            self.stop();
            self.status = format!("capture ended: {reason}");
        }
        if self.capture.is_some() {
            // frames wake the ui through Capture::set_wake; this tick keeps
            // the fps readout and the control refresh alive when they stop
            ui.ctx().request_repaint_after(CONTROL_REFRESH);
        }
        if self.controls.is_some() && self.controls_queried.elapsed() >= CONTROL_REFRESH {
            self.query_controls();
        }
        self.tick_live();

        egui::Panel::top("top").show(ui, |ui| {
            if let Some(event) = devices::show_top_bar(ui, self) {
                match event {
                    devices::Event::Rescan => self.rescan(),
                    devices::Event::SelectDevice(i) => self.select_device(i),
                    devices::Event::Select(selection) => {
                        self.selection = selection;
                        if self.capture.is_some() {
                            self.start();
                        }
                    }
                    devices::Event::Toggle => {
                        if self.capture.is_some() {
                            self.stop();
                            self.status = "stopped".into();
                        } else {
                            self.start();
                        }
                    }
                }
            }
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.monospace(&self.status);
            });
        });

        self.show_side_panel(ui);
        self.show_preview(ui);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

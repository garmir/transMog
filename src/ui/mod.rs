mod controls_panel;
mod devices;
mod fx_panel;
mod preview;

use std::time::{Duration, Instant};

use eframe::egui_wgpu::RenderState;

use crate::capture::{self, Capture, CaptureConfig, DeviceInfo, FormatInfo};
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
        self.query_controls();
    }

    fn show_side_panel(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
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
                        egui::CollapsingHeader::new("camera")
                            .default_open(true)
                            .show(ui, |ui| {
                                actions = controls_panel::show(ui, &self.control_infos);
                            });
                    });
            });
        self.apply_control_actions(actions);
    }

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        if let (Some(preview), Some(rs)) = (&mut self.preview, &self.render_state) {
            if let Some(latest) = self.capture.as_ref().and_then(Capture::latest) {
                preview.upload(rs, &latest);
            }
            preview.render(rs);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(12)))
            .show(ui, |ui| match &self.preview {
                Some(preview) => preview.show(ui),
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

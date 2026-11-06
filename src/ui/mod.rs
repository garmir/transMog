mod aim_panel;
mod controls_panel;
mod devices;
mod fx_panel;
mod live;
mod overlay;
mod preview;
mod qa_panel;
mod reconnect;
mod rect_view;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui_wgpu::RenderState;
use parking_lot::Mutex;

use crate::aim::{calibration, Aim, FitRun, Limits, Ptz, Step};
use crate::capture::{
    self, tap, Capture, CaptureConfig, DeviceInfo, FormatInfo, SharedTap, Tap, TapStatus,
};
use crate::controls::preset::{self, ctl_name, Preset, PresetFormat, Readback};
use crate::controls::{ControlInfo, ControlKind, Controls};
use crate::rect::{self, Rectify};

const CONTROL_REFRESH: Duration = Duration::from_secs(1);
/// how long a framing write gets to land before the fit reads a picture
const FIT_SETTLE: Duration = Duration::from_millis(500);

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
    /// the analysis thread, only while aim mode is on
    aim: Option<Aim>,
    aim_ui: aim_panel::State,
    calibration: PathBuf,
    live: live::Live,
    /// the rectifier, from --rect or the aim panel's checkbox; it writes
    /// beside the tap while the tap is on
    rect: Option<Rectify>,
    rect_scale: f32,
    rect_textures: rect_view::Textures,
    /// the camera to get back after its capture ended on its own
    reconnect: Option<reconnect::Pending>,
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
            aim: None,
            aim_ui: aim_panel::State::default(),
            calibration: preset::dir().with_file_name("calibration.json"),
            live: live::Live::new(tap::default_dir().join("control")),
            rect: None,
            rect_scale: rect::DEFAULT_SCALE,
            rect_textures: rect_view::Textures::default(),
            reconnect: None,
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
                if let Some(aim) = &self.aim {
                    aim.set_source(Some(capture.source()));
                }
                if let Some(r) = &self.rect {
                    r.set_source(Some(capture.source()));
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
        self.aim_ui.fit = None;
        if let Some(aim) = &self.aim {
            aim.set_source(None);
        }
        if let Some(r) = &self.rect {
            r.set_source(None);
        }
    }

    pub fn aim_on(&self) -> bool {
        self.aim.is_some()
    }

    /// aim mode runs its analysis thread only while it is on
    pub fn set_aim(&mut self, on: bool) {
        if on == self.aim.is_some() {
            return;
        }
        if !on {
            self.aim = None;
            self.aim_ui.fit = None;
            return;
        }
        let aim = Aim::start(self.capture.as_ref().map(Capture::source));
        let ctx = self.ctx.clone();
        aim.set_wake(move || ctx.request_repaint());
        if let Some((ptz, _)) = self.framing() {
            aim.set_zoom(ptz.zoom);
        }
        self.aim = Some(aim);
    }

    pub fn rect_on(&self) -> bool {
        self.rect.is_some()
    }

    /// tile pixels a frame pixel for the rectified tiles, from the next
    /// start of the rectifier
    pub fn set_rect_scale(&mut self, scale: f32) {
        self.rect_scale = scale;
    }

    /// the rectifier thread; it writes rect-<screen> beside the tap while
    /// the tap is on
    pub fn set_rect(&mut self, on: bool) {
        if on == self.rect.is_some() {
            return;
        }
        if !on {
            self.rect = None;
            self.aim_ui.rectified = false;
            return;
        }
        let out = self.tap.is_some().then(|| self.tap_dir.clone());
        let r = Rectify::start(
            self.capture.as_ref().map(Capture::source),
            out,
            self.rect_scale,
        );
        let ctx = self.ctx.clone();
        r.set_wake(move || ctx.request_repaint());
        if let Some((ptz, _)) = self.framing() {
            r.set_zoom(ptz.zoom);
        }
        r.set_preview(self.aim_ui.rectified);
        self.rect = Some(r);
    }

    fn set_rectified_view(&mut self, on: bool) {
        self.aim_ui.rectified = on;
        if on {
            self.set_rect(true);
        }
        if let Some(r) = &self.rect {
            r.set_preview(on);
        }
    }

    /// where "save calibration" writes
    pub fn set_calibration_path(&mut self, path: PathBuf) {
        self.calibration = path;
    }

    fn control_id(&self, name: &str) -> Option<&ControlInfo> {
        self.control_infos
            .iter()
            .find(|c| ctl_name(&c.name) == name)
    }

    /// zoom, pan and tilt as last read, with their ranges; none unless
    /// the camera has all three
    fn framing(&self) -> Option<(Ptz, Limits)> {
        let mut ptz = Ptz {
            zoom: 100,
            pan: 0,
            tilt: 0,
        };
        let mut limits = Limits::C920;
        for (i, name) in Ptz::CONTROLS.iter().enumerate() {
            let info = self.control_id(name)?;
            ptz.set(i, info.value);
            if let ControlKind::Integer { min, max, step, .. } = info.kind {
                limits.0[i] = (min, max, step);
            }
        }
        Some((ptz, limits))
    }

    /// writes the framing controls that differ, never any other, and
    /// reads all three back
    fn set_framing(&mut self, want: Ptz) {
        let Some((now, _)) = self.framing() else {
            return;
        };
        let ids: Vec<Option<u32>> = Ptz::CONTROLS
            .iter()
            .map(|n| self.control_id(n).map(|c| c.id))
            .collect();
        if let Some(controls) = &self.controls {
            for (i, id) in ids.iter().enumerate() {
                let Some(id) = *id else {
                    continue;
                };
                if want.get(i) == now.get(i) {
                    continue;
                }
                if let Err(e) = controls.set(id, want.get(i)) {
                    self.status = format!("{}: {e:#}", Ptz::CONTROLS[i]);
                }
            }
        }
        self.query_controls();
        let got = self.framing().map(|(p, _)| p);
        self.aim_ui.readback = (0..3)
            .map(|i| (Ptz::CONTROLS[i], want.get(i), got.map(|p| p.get(i))))
            .collect();
        for (name, wanted, got) in &self.aim_ui.readback {
            tracing::info!(control = name, wanted, got = ?got, "aim framing");
        }
        if let (Some(aim), Some(p)) = (&self.aim, got) {
            aim.set_zoom(p.zoom);
        }
        if let (Some(r), Some(p)) = (&self.rect, got) {
            r.set_zoom(p.zoom);
        }
    }

    fn apply_aim_event(&mut self, event: aim_panel::Event) {
        let Some((now, _)) = self.framing() else {
            return;
        };
        match event {
            aim_panel::Event::Set(i, v) => {
                let mut want = now;
                want.set(i, v);
                self.set_framing(want);
            }
            aim_panel::Event::Fit => {
                self.aim_ui.fit = Some(FitRun::default());
                // the first step waits for an analysis newer than the click
                self.aim_ui.fit_changed = Some(Instant::now());
                self.aim_ui.fit_status = "fitting".into();
            }
            aim_panel::Event::StopFit => {
                self.aim_ui.fit = None;
                self.aim_ui.fit_status = "fit stopped".into();
            }
            aim_panel::Event::ResetFraming => {
                self.aim_ui.fit = None;
                self.set_framing(Ptz {
                    zoom: 100,
                    pan: 0,
                    tilt: 0,
                });
            }
            aim_panel::Event::Save => self.save_calibration(),
            aim_panel::Event::Rectified(on) => self.set_rectified_view(on),
            aim_panel::Event::FitLens(on) => {
                if let Some(r) = &self.rect {
                    r.set_fit_lens(on);
                }
            }
        }
    }

    /// one fit step for each analysis of a picture taken after the last
    /// write had time to land
    fn tick_fit(&mut self) {
        let Some(report) = self.aim.as_ref().and_then(Aim::report) else {
            return;
        };
        if self.aim_ui.fit.is_none()
            || self
                .aim_ui
                .fit_changed
                .is_some_and(|t| report.analysed < t + FIT_SETTLE)
        {
            return;
        }
        let Some((now, limits)) = self.framing() else {
            self.aim_ui.fit = None;
            return;
        };
        let union = (report.screens.len() == 2)
            .then(|| report.union())
            .flatten();
        let size = (report.size.0 as f32, report.size.1 as f32);
        let Some(run) = self.aim_ui.fit.as_mut() else {
            return;
        };
        match run.next(union, size, now, &limits) {
            Step::Set(want) => {
                self.set_framing(want);
                self.aim_ui.fit_changed = Some(Instant::now());
                self.aim_ui.fit_status = format!(
                    "fitting: zoom {} pan {} tilt {}",
                    want.zoom, want.pan, want.tilt
                );
            }
            Step::Done(message) => {
                self.aim_ui.fit = None;
                self.aim_ui.fit_status = message;
            }
        }
    }

    /// both screens' homographies and stamp blocks, in the format the
    /// latency tools read; the file already there is kept beside it
    fn save_calibration(&mut self) {
        let Some(report) = self.aim.as_ref().and_then(Aim::report) else {
            return;
        };
        if !report.aim_ok {
            self.aim_ui.saved = "not saved: aim is not OK".into();
            return;
        }
        let ptz = self.framing().map_or(
            Ptz {
                zoom: report.zoom,
                pan: 0,
                tilt: 0,
            },
            |f| f.0,
        );
        let screens: Vec<_> = report
            .screens
            .iter()
            .map(|s| (&s.screen, s.rate.map_or(0.0, |r| r.0)))
            .collect();
        let (when, stamp) = calibration::local_time();
        let json = calibration::to_json(&screens, report.size, ptz, &when);
        match calibration::save(&self.calibration, &json, &stamp) {
            Ok(backup) => {
                self.aim_ui.saved = match &backup {
                    Some(b) => format!("saved {when}; the old file is {}", b.display()),
                    None => format!("saved {when}"),
                };
                self.status = format!("calibration saved to {}", self.calibration.display());
                tracing::info!(path = %self.calibration.display(), backup = ?backup, "calibration saved");
            }
            Err(e) => self.aim_ui.saved = format!("save failed: {e:#}"),
        }
    }

    /// the tap keeps its sequence across capture restarts; none turns it
    /// off. the overlay file is read from the same directory
    pub fn set_tap(&mut self, dir: Option<PathBuf>) {
        let Some(dir) = dir else {
            self.tap = None;
            if let Some(capture) = &self.capture {
                capture.set_tap(None);
            }
            if let Some(r) = &self.rect {
                r.set_out(None);
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
                if let Some(r) = &self.rect {
                    r.set_out(Some(dir.clone()));
                }
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
        let mut aim_events = Vec::new();
        let aim_report = self.aim.as_ref().and_then(Aim::report);
        let rect_report = self.rect.as_ref().and_then(Rectify::report);
        let framing = self.framing();
        egui::Panel::right("side")
            .resizable(true)
            .default_size(320.0)
            .size_range(200.0..=600.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.aim.is_some() {
                            egui::CollapsingHeader::new("aim")
                                .default_open(true)
                                .show(ui, |ui| {
                                    aim_events = aim_panel::show(
                                        ui,
                                        &self.aim_ui,
                                        aim_report.as_deref(),
                                        framing,
                                        &self.calibration,
                                        self.rect.as_ref().map(|r| (r, rect_report.as_deref())),
                                    );
                                });
                        }
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
        for event in aim_events {
            self.apply_aim_event(event);
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
        let aim = self.aim.as_ref().and_then(Aim::report);
        if aim.is_some() && self.aim_ui.rectified {
            let report = self.rect.as_ref().and_then(Rectify::report);
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(12)))
                .show(ui, |ui| {
                    rect_view::show(ui, report.as_deref(), &mut self.rect_textures);
                });
            return;
        }
        // aim mode draws its own outlines in place of the file's
        let overlay = self.show_overlay && self.tap.is_some() && self.aim.is_none();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(12)))
            .show(ui, |ui| match &self.preview {
                Some(preview) => {
                    if let Some((rect, size, unbent)) = preview.show(ui, aim.is_some()) {
                        if overlay {
                            self.overlay.show(ui, rect, size, unbent);
                            // the overlay file changes without a new frame
                            ui.ctx().request_repaint_after(Duration::from_millis(250));
                        }
                        if let Some(report) = aim.as_ref().filter(|r| r.size == size) {
                            aim_panel::draw(ui, rect, size, report, unbent);
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
            if let (Some(aim), Some((ptz, _))) = (&self.aim, self.framing()) {
                aim.set_zoom(ptz.zoom);
            }
            if let (Some(r), Some((ptz, _))) = (&self.rect, self.framing()) {
                r.set_zoom(ptz.zoom);
            }
        }
        let plain_a = ui.input(|i| i.key_pressed(egui::Key::A) && i.modifiers.is_none());
        if plain_a && !ui.ctx().egui_wants_keyboard_input() {
            self.set_aim(!self.aim_on());
        }
        let plain_r = ui.input(|i| i.key_pressed(egui::Key::R) && i.modifiers.is_none());
        if plain_r && self.aim_on() && !ui.ctx().egui_wants_keyboard_input() {
            self.set_rectified_view(!self.aim_ui.rectified);
        }
        self.tick_reconnect();
        self.tick_fit();
        self.tick_live();

        egui::Panel::top("top").show(ui, |ui| {
            if let Some(event) = devices::show_top_bar(ui, self) {
                match event {
                    devices::Event::Rescan => self.rescan(),
                    devices::Event::SelectDevice(i) => {
                        self.reconnect = None;
                        self.select_device(i);
                    }
                    devices::Event::Select(selection) => {
                        self.selection = selection;
                        if self.capture.is_some() {
                            self.start();
                        }
                    }
                    devices::Event::Aim => self.set_aim(!self.aim_on()),
                    devices::Event::Toggle => {
                        self.reconnect = None;
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

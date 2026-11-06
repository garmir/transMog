use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::App;
use crate::controls::preset::{self, ctl_name, Preset};
use crate::live::{self, ws::Subscription, Report, Seen, Source, Status};

pub(crate) const POLL: Duration = Duration::from_millis(100);

/// the polled control file, the websocket subscription and what each
/// change came to. the window owns it and applies everything on the ui
/// thread, so the camera, the passes and the widgets never disagree
pub(crate) struct Live {
    file: PathBuf,
    /// set by --control-file; otherwise the file follows the tap dir
    file_fixed: bool,
    checked: Option<Instant>,
    modified: Option<SystemTime>,
    file_seen: Seen,
    ws: Option<Subscription>,
    ws_seen: Seen,
    pub qa_lock: bool,
    reports: BTreeMap<String, Report>,
    /// the last body written, to write only on a change
    written: Option<String>,
}

impl Live {
    pub fn new(file: PathBuf) -> Live {
        Live {
            file,
            file_fixed: false,
            checked: None,
            modified: None,
            file_seen: Seen::default(),
            ws: None,
            ws_seen: Seen::default(),
            qa_lock: true,
            reports: BTreeMap::new(),
            written: None,
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// control.state beside the control file
    pub fn state_path(&self) -> PathBuf {
        let mut name = self.file.file_name().unwrap_or_default().to_os_string();
        name.push(".state");
        self.file.with_file_name(name)
    }

    /// a source is there: the file exists, or a subscription runs
    pub fn active(&self) -> bool {
        self.ws.is_some() || self.modified.is_some()
    }

    /// the url and `connected`, `connecting` or `retrying (why)`
    pub fn ws_status(&self) -> Option<(String, String)> {
        self.ws.as_ref().map(|s| (s.url().to_string(), s.status()))
    }

    pub fn reports(&self) -> impl Iterator<Item = (&String, &Report)> {
        self.reports.iter()
    }

    /// the file's changed pairs, when its mtime moved; at most every POLL
    fn poll_file(&mut self) -> Vec<(String, String)> {
        if self.checked.is_some_and(|t| t.elapsed() < POLL) {
            return Vec::new();
        }
        self.checked = Some(Instant::now());
        let modified = fs::metadata(&self.file).and_then(|m| m.modified()).ok();
        if modified == self.modified {
            return Vec::new();
        }
        self.modified = modified;
        match fs::read_to_string(&self.file) {
            Ok(text) => self.file_seen.changed(&live::parse(&text)),
            Err(_) => Vec::new(),
        }
    }
}

impl App {
    /// reads `key value` lines from PATH instead of `<tap dir>/control`
    pub fn set_control_file(&mut self, path: PathBuf) {
        self.live.file = path;
        self.live.file_fixed = true;
        self.live.checked = None;
        self.live.modified = None;
    }

    /// subscribes to a viz_ctrl websocket and applies the pairs in each
    /// message's assets.camera.transmog
    pub fn subscribe(&mut self, url: &str) {
        let ctx = self.ctx.clone();
        match Subscription::start(url, move || ctx.request_repaint()) {
            Ok(sub) => self.live.ws = Some(sub),
            Err(e) => self.status = format!("control {url}: {e:#}"),
        }
    }

    /// the tap dir moved; the default control file moves with it
    pub(super) fn follow_tap_dir(&mut self, dir: &Path) {
        if !self.live.file_fixed {
            self.live.file = dir.join("control");
            self.live.checked = None;
            self.live.modified = None;
        }
    }

    pub fn qa_lock(&self) -> bool {
        self.live.qa_lock
    }

    /// whether the lock holds this control now; only while a qa preset
    /// exists to say what it holds
    pub(super) fn held(&self, key: &str) -> bool {
        self.live.qa_lock && live::is_locked(key) && preset::path(live::LOCK_PRESET).exists()
    }

    /// turning the lock on puts the held controls back to the qa preset's
    /// values
    pub(super) fn set_qa_lock(&mut self, on: bool) {
        let was = self.live.qa_lock;
        self.live.qa_lock = on;
        if on && !was {
            self.hold();
        }
    }

    /// writes the qa preset's values of the held controls
    pub(super) fn hold(&mut self) {
        let Ok(qa) = preset::load(live::LOCK_PRESET) else {
            return;
        };
        let held = Preset {
            format: None,
            controls: qa
                .controls
                .into_iter()
                .filter(|(k, _)| live::is_locked(k))
                .collect(),
        };
        if let Some(controls) = &self.controls {
            match held.apply(controls) {
                Ok(readback) => {
                    let off = readback.iter().filter(|r| !r.ok()).count();
                    self.status = format!("qa lock on: {off} held controls read back different");
                }
                Err(e) => self.status = format!("qa lock: {e:#}"),
            }
            self.query_controls();
        }
    }

    /// one ui frame's worth: take the file's and the websocket's changes,
    /// apply them, then rewrite control.state if anything in it moved
    pub(super) fn tick_live(&mut self) {
        let file = self.live.poll_file();
        let (ws, seq) = match self.live.ws.as_ref().and_then(Subscription::take) {
            Some(update) => (self.live.ws_seen.changed(&update.pairs), update.seq),
            None => (Vec::new(), None),
        };
        let changes = live::merge(file, ws, seq);
        if !changes.is_empty() {
            self.apply_live(changes);
        }
        if self.live.active() {
            self.write_state();
            // the file can change without a frame or an input event
            self.ctx.request_repaint_after(POLL);
        }
    }

    fn apply_live(&mut self, changes: Vec<(String, String, Source)>) {
        // v4l2 writes go out first and are read back together after
        let mut camera: Vec<(String, String, Source, u32, Option<String>)> = Vec::new();
        for (key, value, source) in changes {
            tracing::info!(%key, %value, %source, "live control");
            let report = match key.as_str() {
                "qa_lock" => match live::int(&value) {
                    Some(v) => {
                        self.set_qa_lock(v != 0);
                        Report::checked(
                            &value,
                            Some(u8::from(self.live.qa_lock).to_string()),
                            source,
                        )
                    }
                    None => error(&value, "qa_lock wants 0 or 1", source),
                },
                "ndi" => match live::int(&value) {
                    Some(v) => {
                        self.set_ndi(v != 0);
                        let on = self.ndi_stats().is_some();
                        Report {
                            wanted: value.clone(),
                            got: Some(if on { "on" } else { "off" }.into()),
                            status: if (v != 0) == on {
                                Status::Ok
                            } else {
                                Status::Error(self.status.clone())
                            },
                            source,
                        }
                    }
                    None => error(&value, "ndi wants on or off", source),
                },
                "preset" => Report {
                    wanted: value.clone(),
                    got: Some(value.clone()),
                    status: if self.apply_preset(&value) {
                        Status::Ok
                    } else {
                        Status::Error(self.status.clone())
                    },
                    source,
                },
                _ => match self.set_pass_param(&key, &value) {
                    // a menu sent as its index reads back as its label
                    Some(Ok(got)) if live::float(&got).is_none() => Report {
                        wanted: value.clone(),
                        got: Some(got),
                        status: Status::Ok,
                        source,
                    },
                    Some(Ok(got)) => Report::checked(&value, Some(got), source),
                    Some(Err(e)) => error(&value, &e, source),
                    None => {
                        let info = self.control_infos.iter().find(|i| ctl_name(&i.name) == key);
                        match (info, live::int(&value)) {
                            (None, _) => error(&value, "no such control or parameter", source),
                            (Some(i), _) if self.held(&key) => Report {
                                wanted: value.clone(),
                                got: Some(i.value.to_string()),
                                status: Status::Locked,
                                source,
                            },
                            (Some(_), None) => error(&value, "wants an integer", source),
                            (Some(i), Some(v)) => {
                                let id = i.id;
                                let err = self
                                    .controls
                                    .as_ref()
                                    .and_then(|c| c.set(id, v).err().map(|e| format!("{e:#}")));
                                camera.push((key, value, source, id, err));
                                continue;
                            }
                        }
                    }
                },
            };
            self.live.reports.insert(key, report);
        }
        if camera.is_empty() {
            return;
        }
        // a value is refused while its auto mode is on, so the refused ones
        // get one more go once the others have landed, as a preset does
        if let Some(controls) = &self.controls {
            for (_, value, _, id, err) in camera.iter_mut().filter(|c| c.4.is_some()) {
                if let Some(v) = live::int(value) {
                    *err = controls.set(*id, v).err().map(|e| format!("{e:#}"));
                }
            }
        }
        self.query_controls();
        for (key, value, source, id, err) in camera {
            let got = self
                .control_infos
                .iter()
                .find(|i| i.id == id)
                .map(|i| i.value.to_string());
            let mut r = Report::checked(&value, got, source);
            if let Some(e) = err {
                r.status = Status::Error(e);
            }
            self.live.reports.insert(key, r);
        }
    }

    /// `<pass>_enabled` or `<pass>_<param>`; none when no pass has the
    /// key's prefix
    fn set_pass_param(&mut self, key: &str, value: &str) -> Option<Result<String, String>> {
        let preview = self.preview.as_mut()?;
        let pass = preview
            .passes_mut()
            .iter_mut()
            .find(|p| key.starts_with(&format!("{}_", p.name())))?;
        let param = &key[pass.name().len() + 1..];
        let result = if param == "enabled" {
            match live::int(value) {
                Some(v) => {
                    pass.set_enabled(v != 0);
                    Ok(u8::from(pass.enabled()).to_string())
                }
                None => Err("enabled wants 0 or 1".into()),
            }
        } else {
            pass.set_param(param, value).and_then(|()| {
                pass.params()
                    .into_iter()
                    .find(|(n, _)| *n == param)
                    .map(|(_, v)| v)
                    .ok_or_else(|| format!("{param} not read back"))
            })
        };
        preview.invalidate();
        Some(result)
    }

    /// every pass parameter as `<pass>_<param> value`
    pub(super) fn lens_params(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for pass in self.passes() {
            out.push((
                format!("{}_enabled", pass.name()),
                u8::from(pass.enabled()).to_string(),
            ));
            for (name, value) in pass.params() {
                out.push((format!("{}_{name}", pass.name()), value));
            }
        }
        out
    }

    fn write_state(&mut self) {
        let camera: Vec<(String, String)> = self
            .control_infos
            .iter()
            .filter(|i| {
                !matches!(
                    i.kind,
                    crate::controls::ControlKind::Button | crate::controls::ControlKind::Other
                )
            })
            .map(|i| (ctl_name(&i.name), i.value.to_string()))
            .collect();
        let reports: Vec<(String, Report)> = self
            .live
            .reports
            .iter()
            .map(|(k, r)| (k.clone(), r.clone()))
            .collect();
        let snapshot = live::Snapshot {
            qa_lock: self.live.qa_lock,
            ndi: self.ndi_stats().is_some(),
            file: Some(&self.live.file),
            ws: self.live.ws_status(),
            camera,
            lens: self.lens_params(),
        };
        let body = live::render(&snapshot, &reports);
        if self.live.written.as_deref() == Some(body.as_str()) {
            return;
        }
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let path = self.live.state_path();
        let text = format!(
            "# transmog live control, `set KEY WANTED GOT STATUS SOURCE`\nt {t:.6}\n{body}"
        );
        if let Err(e) = live::write_atomic(&path, &text) {
            tracing::warn!("{}: {e}", path.display());
        }
        // on an error, the next try waits for the next change, not a frame
        self.live.written = Some(body);
    }
}

fn error(value: &str, message: &str, source: Source) -> Report {
    Report {
        wanted: value.to_string(),
        got: None,
        status: Status::Error(message.to_string()),
        source,
    }
}

/// the "live control" section: sources, the lock and the latest change
/// to each key
pub(super) fn show(ui: &mut egui::Ui, app: &mut App) {
    ui.monospace(format!("file {}", app.live.file().display()));
    match app.live.ws_status() {
        Some((url, status)) => ui.monospace(format!("ws {url}\n   {status}")),
        None => ui.weak("no websocket (--control URL)"),
    };
    let mut lock = app.live.qa_lock;
    if ui
        .checkbox(&mut lock, "qa lock")
        .on_hover_text(
            "holds exposure, gain, focus, white balance, power line, zoom, pan and tilt \
             at the qa preset",
        )
        .changed()
    {
        app.set_qa_lock(lock);
    }
    egui::Grid::new("live reports")
        .striped(true)
        .show(ui, |ui| {
            for (key, r) in app.live.reports() {
                ui.monospace(key);
                ui.monospace(&r.wanted);
                ui.monospace(r.got.as_deref().unwrap_or("-"));
                let status = match &r.status {
                    Status::Ok => "ok".to_string(),
                    Status::Differs => "differs".to_string(),
                    Status::Locked => "locked".to_string(),
                    Status::Error(e) => format!("error: {e}"),
                };
                ui.monospace(status);
                ui.end_row();
            }
        });
}

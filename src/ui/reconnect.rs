//! when the capture thread ends on its own (the camera went away, as on
//! the 2026-11-06 USB drop), the window tries the same camera again every
//! RETRY until it is back. A re-enumerated C920 comes up with every
//! control at its default and the kernel restores none of the vendor
//! ones, so the controls as last read go back on, then the qa lock's

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{devices, App};
use crate::controls::preset::{Preset, PresetFormat};

pub(super) const RETRY: Duration = Duration::from_secs(2);

pub(super) struct Pending {
    name: String,
    path: PathBuf,
    format: PresetFormat,
    controls: Preset,
    why: String,
    since: Instant,
    next: Instant,
    tries: u32,
}

impl App {
    pub(super) fn tick_reconnect(&mut self) {
        if let Some(why) = self.capture.as_ref().and_then(|c| c.finished()) {
            self.lost(why);
        }
        let Some(p) = &self.reconnect else {
            return;
        };
        let now = Instant::now();
        if now < p.next {
            self.ctx.request_repaint_after(p.next - now);
            return;
        }
        let (name, path) = (p.name.clone(), p.path.clone());
        self.rescan();
        let found = self
            .devices
            .iter()
            .position(|d| d.path == path && d.name == name)
            .or_else(|| self.devices.iter().position(|d| d.name == name));
        if let Some(i) = found {
            if self.selected_device != Some(i) {
                self.select_device(i);
            }
            if let Some(p) = &self.reconnect {
                if let Some(selection) = devices::find_selection(&self.formats, &p.format) {
                    self.selection = selection;
                }
            }
            self.start();
        }
        if self.capture.is_none() {
            if let Some(p) = &mut self.reconnect {
                p.tries += 1;
                p.next = Instant::now() + RETRY;
                self.status = format!(
                    "{} lost ({}); retry {} after {:.0} s",
                    p.name,
                    p.why,
                    p.tries,
                    p.since.elapsed().as_secs_f32()
                );
            }
            self.ctx.request_repaint_after(RETRY);
            return;
        }
        let Some(p) = self.reconnect.take() else {
            return;
        };
        let off = match &self.controls {
            Some(controls) => match p.controls.apply(controls) {
                Ok(readback) => readback.iter().filter(|r| !r.ok()).count(),
                Err(e) => {
                    tracing::warn!("reapply after reconnect: {e:#}");
                    p.controls.controls.len()
                }
            },
            None => p.controls.controls.len(),
        };
        if self.live.qa_lock {
            self.hold();
        }
        self.query_controls();
        self.status = format!(
            "{} back after {:.1} s; {} controls reapplied, {off} read back different",
            p.name,
            p.since.elapsed().as_secs_f32(),
            p.controls.controls.len()
        );
        tracing::info!("{}", self.status);
    }

    fn lost(&mut self, why: String) {
        let (Some(capture), Some(device)) = (&self.capture, self.selected_device()) else {
            return;
        };
        let pending = Pending {
            name: device.name.clone(),
            path: device.path.clone(),
            format: PresetFormat::from(capture.actual()),
            controls: Preset::from_current(None, &self.control_infos),
            why,
            since: Instant::now(),
            next: Instant::now() + RETRY,
            tries: 0,
        };
        tracing::warn!(
            "{}: capture ended ({}); reconnecting",
            pending.name,
            pending.why
        );
        self.status = format!("{} lost ({}); reconnecting", pending.name, pending.why);
        self.stop();
        self.reconnect = Some(pending);
    }
}

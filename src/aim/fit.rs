use egui::{pos2, Rect, Vec2};

use super::hint::MARGIN;

/// the c920's digital framing: zoom_absolute, pan_absolute and
/// tilt_absolute
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ptz {
    pub zoom: i64,
    pub pan: i64,
    pub tilt: i64,
}

impl Ptz {
    pub const CONTROLS: [&'static str; 3] = ["zoom_absolute", "pan_absolute", "tilt_absolute"];

    pub fn get(&self, i: usize) -> i64 {
        [self.zoom, self.pan, self.tilt][i]
    }

    pub fn set(&mut self, i: usize, v: i64) {
        match i {
            0 => self.zoom = v,
            1 => self.pan = v,
            _ => self.tilt = v,
        }
    }
}

/// min, max and step of each control, in Ptz::CONTROLS order
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits(pub [(i64, i64, i64); 3]);

impl Limits {
    /// what a c920 reports
    pub const C920: Limits = Limits([(100, 500, 1), (-36000, 36000, 3600), (-36000, 36000, 3600)]);

    fn clamp(&self, i: usize, v: i64) -> i64 {
        let (lo, hi, step) = self.0[i];
        let step = step.max(1);
        let v = lo + ((v - lo) as f64 / step as f64).round() as i64 * step;
        v.clamp(lo, hi)
    }
}

/// the largest zoom at which `union`, seen at zoom `zoom`, still sits
/// inside the margin once the camera scales the picture about its centre
pub fn zoom_to_fit(union: Rect, size: (f32, f32), zoom: i64, limits: &Limits) -> i64 {
    let c = pos2(size.0 / 2.0, size.1 / 2.0);
    let (ax, ay) = (size.0 * (0.5 - MARGIN), size.1 * (0.5 - MARGIN));
    let reach = [
        (c.x - union.min.x, ax),
        (union.max.x - c.x, ax),
        (c.y - union.min.y, ay),
        (union.max.y - c.y, ay),
    ];
    let k = reach
        .iter()
        .filter(|(d, _)| *d > 1.0)
        .map(|(d, a)| a / d)
        .fold(f32::MAX, f32::min);
    if k == f32::MAX {
        return zoom;
    }
    // floor, so the margin holds
    limits.clamp(0, (zoom as f32 * k).floor() as i64)
}

pub enum Step {
    Set(Ptz),
    Done(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Zoom,
    Centre,
    Final,
}

/// "fit both": zoom in as far as the screens allow, centre them with pan
/// and tilt, then zoom in again. the c920 does not say how far a pan
/// step moves the picture at a given zoom, so the first step of each
/// axis is a probe and the rest use what it moved
pub struct FitRun {
    phase: Phase,
    moves: u32,
    /// the offset seen before the last pan or tilt move, and that move
    probe: Option<(Vec2, i64, i64)>,
    /// picture pixels per control unit, pan and tilt
    gain: [Option<f32>; 2],
}

const MAX_MOVES: u32 = 8;

impl Default for FitRun {
    fn default() -> FitRun {
        FitRun {
            phase: Phase::Zoom,
            moves: 0,
            probe: None,
            gain: [None, None],
        }
    }
}

impl FitRun {
    /// the next control values for the screens' `union` as seen now with
    /// `now` applied; call again with a picture taken after they landed
    pub fn next(
        &mut self,
        union: Option<Rect>,
        size: (f32, f32),
        now: Ptz,
        limits: &Limits,
    ) -> Step {
        let Some(union) = union else {
            return Step::Done("fit both needs both screens found".into());
        };
        let offset = union.center() - pos2(size.0 / 2.0, size.1 / 2.0);
        match self.phase {
            Phase::Zoom => {
                self.phase = Phase::Centre;
                let zoom = zoom_to_fit(union, size, now.zoom, limits);
                if zoom != now.zoom {
                    return Step::Set(Ptz { zoom, ..now });
                }
                self.next(Some(union), size, now, limits)
            }
            Phase::Centre => {
                if let Some((before, dpan, dtilt)) = self.probe.take() {
                    let moved = offset - before;
                    if dpan != 0 && moved.x.abs() > 1.0 {
                        self.gain[0] = Some(moved.x / dpan as f32);
                    }
                    if dtilt != 0 && moved.y.abs() > 1.0 {
                        self.gain[1] = Some(moved.y / dtilt as f32);
                    }
                    if moved.length() <= 1.0 {
                        self.phase = Phase::Final;
                        return self.finish(
                            union,
                            size,
                            now,
                            limits,
                            "pan and tilt do not move the picture at this zoom",
                        );
                    }
                }
                let centred_x = offset.x.abs() < 0.02 * size.0;
                let centred_y = offset.y.abs() < 0.02 * size.1;
                if (centred_x && centred_y) || self.moves >= MAX_MOVES || now.zoom <= limits.0[0].0
                {
                    self.phase = Phase::Final;
                    return self.finish(union, size, now, limits, "");
                }
                self.moves += 1;
                let mut want = now;
                let axis = |off: f32, gain: Option<f32>, cur: i64, i: usize| {
                    let step = limits.0[i].2.max(1);
                    match gain {
                        Some(g) if g.abs() > 1e-6 => {
                            limits.clamp(i, cur - (off / g).round() as i64)
                        }
                        // one step towards the screens: positive pan is
                        // assumed to move the view right, positive tilt up
                        _ => limits.clamp(
                            i,
                            cur + if i == 1 {
                                off.signum() as i64
                            } else {
                                -(off.signum() as i64)
                            } * step,
                        ),
                    }
                };
                if !centred_x {
                    want.pan = axis(offset.x, self.gain[0], now.pan, 1);
                }
                if !centred_y {
                    want.tilt = axis(offset.y, self.gain[1], now.tilt, 2);
                }
                if want == now {
                    self.phase = Phase::Final;
                    return self.finish(union, size, now, limits, "pan and tilt at their limits");
                }
                self.probe = Some((offset, want.pan - now.pan, want.tilt - now.tilt));
                Step::Set(want)
            }
            Phase::Final => Step::Done(format!(
                "fitted at zoom {} pan {} tilt {}",
                now.zoom, now.pan, now.tilt
            )),
        }
    }

    fn finish(
        &mut self,
        union: Rect,
        size: (f32, f32),
        now: Ptz,
        limits: &Limits,
        why: &str,
    ) -> Step {
        let zoom = zoom_to_fit(union, size, now.zoom, limits);
        if zoom != now.zoom {
            return Step::Set(Ptz { zoom, ..now });
        }
        let mut msg = format!(
            "fitted at zoom {} pan {} tilt {}",
            now.zoom, now.pan, now.tilt
        );
        if !why.is_empty() {
            msg += &format!(" ({why})");
        }
        Step::Done(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a camera whose zoom scales the picture about its centre and whose
    /// pan and tilt shift it by `gain` pixels a unit at zoom 100
    fn seen(base: Rect, size: (f32, f32), p: Ptz, gain: f32) -> Rect {
        let c = pos2(size.0 / 2.0, size.1 / 2.0);
        let k = p.zoom as f32 / 100.0;
        let shift = egui::vec2(-(p.pan as f32) * gain, p.tilt as f32 * gain);
        let map = |q: egui::Pos2| c + (q - c + shift) * k;
        Rect::from_min_max(map(base.min), map(base.max))
    }

    #[test]
    fn zoom_keeps_the_margin() {
        let size = (1920.0, 1080.0);
        let union = Rect::from_min_max(pos2(560.0, 240.0), pos2(1360.0, 840.0));
        let z = zoom_to_fit(union, size, 100, &Limits::C920);
        // height limits: 486 / 300 = 1.62
        assert_eq!(z, 162);
    }

    #[test]
    fn fit_centres_then_zooms() {
        let size = (1920.0, 1080.0);
        let base = Rect::from_min_max(pos2(300.0, 100.0), pos2(1100.0, 700.0));
        let mut now = Ptz {
            zoom: 100,
            pan: 0,
            tilt: 0,
        };
        let mut run = FitRun::default();
        for _ in 0..20 {
            match run.next(Some(seen(base, size, now, 0.01)), size, now, &Limits::C920) {
                Step::Set(p) => now = p,
                Step::Done(_) => break,
            }
        }
        let end = seen(base, size, now, 0.01);
        assert!(now.zoom > 140, "{now:?}");
        let inset = Rect::from_min_max(pos2(96.0, 54.0), pos2(1824.0, 1026.0));
        assert!(inset.expand(1.0).contains_rect(end), "{end:?} at {now:?}");
    }
}

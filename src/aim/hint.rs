use egui::{pos2, vec2, Pos2, Rect, Vec2};

use super::detect::{Name, Screen};
use super::geom::{bounds, slope_deg};

/// share of the picture's width and height kept clear on every side
pub const MARGIN: f32 = 0.05;
/// roll below this is left alone
const LEVEL_DEG: f32 = 1.5;
/// a shift below this share of the picture is not worth a hint
const SHIFT: f32 = 0.02;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hint {
    TiltUp,
    TiltDown,
    PanLeft,
    PanRight,
    MoveBack,
    /// digital zoom is on, so zooming out comes before moving
    ZoomOut,
    MoveCloser,
    /// as seen from behind the camera
    Rotate {
        clockwise: bool,
        deg: f32,
    },
}

impl Hint {
    pub fn text(self) -> String {
        match self {
            Hint::TiltUp => "tilt up".into(),
            Hint::TiltDown => "tilt down".into(),
            Hint::PanLeft => "pan left".into(),
            Hint::PanRight => "pan right".into(),
            Hint::MoveBack => "move back".into(),
            Hint::ZoomOut => "zoom out".into(),
            Hint::MoveCloser => "move closer, or fit both".into(),
            Hint::Rotate { clockwise, deg } => format!(
                "rotate {}clockwise {:.0}°",
                if clockwise { "" } else { "counter-" },
                deg
            ),
        }
    }

    /// the way to point the camera, as an arrow on the preview; zero
    /// for the hints drawn another way
    pub fn arrow(self) -> Vec2 {
        match self {
            Hint::TiltUp => vec2(0.0, -1.0),
            Hint::TiltDown => vec2(0.0, 1.0),
            Hint::PanLeft => vec2(-1.0, 0.0),
            Hint::PanRight => vec2(1.0, 0.0),
            _ => Vec2::ZERO,
        }
    }
}

/// a scale about `centre` and then a shift, which takes the screens as
/// seen now to where they should sit
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    pub centre: Pos2,
    pub scale: f32,
    pub shift: Vec2,
}

impl Target {
    pub fn apply(&self, p: Pos2) -> Pos2 {
        self.centre + (p - self.centre) * self.scale + self.shift
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub hints: Vec<Hint>,
    /// why a hint was given when a screen is missing
    pub notes: Vec<String>,
    pub target: Option<Target>,
    /// mean slope of the screens' top and bottom edges, degrees,
    /// positive when their right ends sit lower
    pub level_deg: Option<f32>,
    /// the screen the level comes from: the projector when it is found
    pub level_from: Option<Name>,
    /// both screens found and wholly inside the picture
    pub both_in_view: bool,
}

/// the hints for one analysis. `size` is the camera picture, `zoom` the
/// digital zoom factor (1 at zoom_absolute 100). the projector hangs
/// above the mini, which is what a missing screen's hint assumes
pub fn plan(screens: &[Screen], size: (f32, f32), zoom: f32) -> Plan {
    let (w, h) = size;
    let mut plan = Plan::default();
    let find = |n: Name| screens.iter().find(|s| s.name == n);
    let (proj, mini) = (find(Name::Projector), find(Name::Mini));
    plan.both_in_view =
        proj.is_some_and(|s| !s.clipped.any()) && mini.is_some_and(|s| !s.clipped.any());

    // the projector's picture on the wall is the level reference; the
    // mini stands at an angle to the camera and its outline from two
    // left markers leans by several degrees, so it gives a level but no
    // rotate hint
    plan.level_from = proj.or(mini).map(|s| s.name);
    let mut edges = Vec::new();
    for s in screens.iter().filter(|s| Some(s.name) == plan.level_from) {
        let o = &s.outline;
        edges.push((slope_deg(o[0], o[1]), o[0].distance(o[1])));
        edges.push((slope_deg(o[3], o[2]), o[3].distance(o[2])));
    }
    let weight: f32 = edges.iter().map(|e| e.1).sum();
    if weight > 0.0 {
        plan.level_deg = Some(edges.iter().map(|e| e.0 * e.1).sum::<f32>() / weight);
    }
    if let Some(deg) = plan.level_deg {
        set_level(&mut plan, deg);
    }

    let Some(union) = bounds(screens.iter().flat_map(|s| s.outline)) else {
        plan.notes
            .push("no screens found: point the camera at the projector wall".into());
        return plan;
    };
    let inset = Rect::from_min_max(
        pos2(w * MARGIN, h * MARGIN),
        pos2(w * (1.0 - MARGIN), h * (1.0 - MARGIN)),
    );
    let fit = (inset.width() / union.width()).min(inset.height() / union.height());
    let scale = fit.min(1.0);
    let scaled = Rect::from_center_size(union.center(), union.size() * scale);
    let shift_axis = |lo: f32, hi: f32, min: f32, max: f32| {
        if scale < 1.0 {
            (min + max) / 2.0 - (lo + hi) / 2.0
        } else if lo < min {
            min - lo
        } else if hi > max {
            max - hi
        } else {
            0.0
        }
    };
    let shift = vec2(
        shift_axis(scaled.min.x, scaled.max.x, inset.min.x, inset.max.x),
        shift_axis(scaled.min.y, scaled.max.y, inset.min.y, inset.max.y),
    );
    plan.target = Some(Target {
        centre: union.center(),
        scale,
        shift,
    });

    if scale < 0.97 {
        plan.hints.push(if zoom > 1.01 {
            Hint::ZoomOut
        } else {
            Hint::MoveBack
        });
    }
    // while the screens have to shrink, a shift is only worth saying
    // when it is large
    let tol = if scale < 0.97 { 0.15 } else { SHIFT };
    if shift.y > tol * h {
        plan.hints.push(Hint::TiltUp);
    } else if shift.y < -tol * h {
        plan.hints.push(Hint::TiltDown);
    }
    if shift.x > tol * w {
        plan.hints.push(Hint::PanLeft);
    } else if shift.x < -tol * w {
        plan.hints.push(Hint::PanRight);
    }

    match (proj, mini) {
        (Some(p), None) => {
            plan.notes
                .push("mini not found; it sits below the projector".into());
            let bottom = p.outline.iter().map(|q| q.y).fold(f32::MIN, f32::max);
            let hint = if bottom > 0.75 * h {
                Hint::MoveBack
            } else {
                Hint::TiltDown
            };
            push_once(&mut plan.hints, hint);
        }
        (None, Some(m)) => {
            plan.notes
                .push("projector not found; it sits above the mini".into());
            let top = m.outline.iter().map(|q| q.y).fold(f32::MAX, f32::min);
            let hint = if top < 0.25 * h {
                Hint::MoveBack
            } else {
                Hint::TiltUp
            };
            push_once(&mut plan.hints, hint);
        }
        (Some(_), Some(_)) if fit > 1.6 => plan.hints.push(Hint::MoveCloser),
        _ => {}
    }
    for s in screens {
        if s.clipped.any() {
            plan.notes.push(format!(
                "{} runs past the {} edge",
                s.name.label(),
                s.clipped.names().join(" and ")
            ));
        }
    }
    plan
}

/// replaces the level and the rotate hint, for a level smoothed over
/// several analyses
pub fn set_level(plan: &mut Plan, deg: f32) {
    plan.level_deg = Some(deg);
    plan.hints.retain(|h| !matches!(h, Hint::Rotate { .. }));
    if deg.abs() > LEVEL_DEG && plan.level_from == Some(Name::Projector) {
        plan.hints.insert(
            0,
            Hint::Rotate {
                clockwise: deg > 0.0,
                deg: deg.abs(),
            },
        );
    }
}

/// adds a missing screen's hint unless it is there already; against the
/// opposite tilt it becomes move back, since the screens then need room
/// both ways
fn push_once(hints: &mut Vec<Hint>, hint: Hint) {
    let opposite = |a: Hint| {
        matches!(
            (a, hint),
            (Hint::TiltUp, Hint::TiltDown) | (Hint::TiltDown, Hint::TiltUp)
        )
    };
    if hints.contains(&hint) {
        return;
    }
    if hints.iter().any(|&h| opposite(h)) {
        hints.retain(|&h| !opposite(h));
        if !hints.contains(&Hint::MoveBack) && !hints.contains(&Hint::ZoomOut) {
            hints.push(Hint::MoveBack);
        }
        return;
    }
    hints.push(hint);
}

/// a line about the stamps' grey levels at the current exposure, none
/// when they look usable
pub fn exposure_note(s: &Screen) -> Option<String> {
    let st = s.stamps?;
    let name = s.name.label();
    if st.white >= 250.0 || s.saturated > 0.2 {
        Some(format!(
            "{name} white clips ({:.0}, {:.0}% of the stamps at 250+): lower gain or exposure",
            st.white,
            s.saturated * 100.0
        ))
    } else if st.white - st.black < 60.0 {
        Some(format!(
            "{name} too dark (white {:.0}, black {:.0}): raise gain or exposure",
            st.white, st.black
        ))
    } else {
        None
    }
}

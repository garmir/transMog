use std::path::Path;
use std::time::Instant;

use egui::{pos2, vec2, Color32, FontId, Pos2, Stroke};

use crate::aim::{FitRun, Hint, Limits, Name, Ptz, Report};

pub(crate) enum Event {
    /// one of Ptz::CONTROLS to a value
    Set(usize, i64),
    Fit,
    StopFit,
    ResetFraming,
    Save,
}

#[derive(Default)]
pub(crate) struct State {
    pub fit: Option<FitRun>,
    /// when the fit last wrote the controls; it waits for a later analysis
    pub fit_changed: Option<Instant>,
    pub fit_status: String,
    pub saved: String,
    /// wanted against read back after the last framing write
    pub readback: Vec<(&'static str, i64, Option<i64>)>,
}

const PROJ: Color32 = Color32::from_rgb(255, 150, 40);
const MINI: Color32 = Color32::from_rgb(70, 200, 255);
const TARGET: Color32 = Color32::from_rgb(90, 230, 110);
const HINT: Color32 = Color32::from_rgb(255, 225, 60);

fn colour(name: Name) -> Color32 {
    match name {
        Name::Projector => PROJ,
        Name::Mini => MINI,
    }
}

/// "in full view", "clipped top and left" or "not found"
pub(crate) fn view_text(report: &Report, name: Name) -> String {
    match report.screen(name) {
        None => "not found".into(),
        Some(s) if s.screen.clipped.any() => {
            format!("clipped {}", s.screen.clipped.names().join(" and "))
        }
        Some(_) => "in full view".into(),
    }
}

/// the headline: aim ok, or the hints in words
pub(crate) fn headline(report: &Report) -> String {
    if report.aim_ok {
        return "aim OK".into();
    }
    let hints: Vec<String> = report.plan.hints.iter().map(|h| h.text()).collect();
    if hints.is_empty() {
        report
            .waiting
            .clone()
            .unwrap_or_else(|| "hold still".into())
    } else {
        hints.join(", ")
    }
}

pub(crate) fn show(
    ui: &mut egui::Ui,
    state: &State,
    report: Option<&Report>,
    framing: Option<(Ptz, Limits)>,
    calibration: &Path,
) -> Vec<Event> {
    let mut events = Vec::new();
    match report {
        Some(r) => show_report(ui, r),
        None => {
            ui.weak("waiting for the first analysis");
        }
    }
    ui.separator();
    ui.label("digital framing (not saved in a preset)");
    match framing {
        Some((ptz, limits)) => {
            for (i, name) in Ptz::CONTROLS.iter().enumerate() {
                let (lo, hi, step) = limits.0[i];
                let mut v = ptz.get(i);
                ui.horizontal(|ui| {
                    let slider = egui::Slider::new(&mut v, lo..=hi)
                        .step_by(step.max(1) as f64)
                        .text(*name);
                    if ui.add(slider).changed() {
                        events.push(Event::Set(i, v));
                    }
                });
            }
            for (name, wanted, got) in &state.readback {
                let got_text = got.map_or("-".into(), |g| g.to_string());
                let text = format!("{name} {wanted} read back {got_text}");
                if *got == Some(*wanted) {
                    ui.monospace(text);
                } else {
                    ui.colored_label(ui.visuals().warn_fg_color, text);
                }
            }
            ui.horizontal(|ui| {
                if state.fit.is_some() {
                    if ui.button("stop fit").clicked() {
                        events.push(Event::StopFit);
                    }
                } else if ui.button("fit both").clicked() {
                    events.push(Event::Fit);
                }
                if ui.button("reset framing").clicked() {
                    events.push(Event::ResetFraming);
                }
            });
        }
        None => {
            ui.weak("no zoom, pan and tilt controls (start a capture)");
            ui.add_enabled(false, egui::Button::new("fit both"));
        }
    }
    if !state.fit_status.is_empty() {
        ui.monospace(&state.fit_status);
    }

    ui.separator();
    if ui
        .add_enabled(
            report.is_some_and(|r| r.aim_ok),
            egui::Button::new("save calibration"),
        )
        .on_disabled_hover_text("needs aim OK")
        .clicked()
    {
        events.push(Event::Save);
    }
    ui.weak(calibration.display().to_string());
    if !state.saved.is_empty() {
        ui.monospace(&state.saved);
    }
    events
}

fn show_report(ui: &mut egui::Ui, r: &Report) {
    let head = headline(r);
    if r.aim_ok {
        ui.colored_label(TARGET, egui::RichText::new(head).strong());
    } else {
        ui.colored_label(ui.visuals().warn_fg_color, head);
    }
    for note in &r.plan.notes {
        ui.label(note);
    }
    if let (false, Some(w)) = (r.aim_ok, &r.waiting) {
        ui.weak(w);
    }
    egui::Grid::new("aim screens").striped(true).show(ui, |ui| {
        ui.strong("screen");
        ui.strong("view");
        ui.strong("fids");
        ui.strong("decode 1 s");
        ui.strong("focus");
        ui.end_row();
        for name in Name::ALL {
            ui.colored_label(colour(name), name.label());
            ui.monospace(view_text(r, name));
            match r.screen(name) {
                Some(s) => {
                    let ids: Vec<String> = s.screen.seen_ids().iter().map(u8::to_string).collect();
                    ui.monospace(ids.join(" "));
                    ui.monospace(s.rate.map_or("-".into(), |(rate, n)| {
                        format!("{:.0}% of {n}", rate * 100.0)
                    }));
                    ui.monospace(s.screen.focus.map_or("-".into(), |f| format!("{f:.3}")));
                }
                None => {
                    ui.monospace("-");
                    ui.monospace("-");
                    ui.monospace("-");
                }
            }
            ui.end_row();
        }
    });
    if let Some(deg) = r.plan.level_deg {
        let from = r.plan.level_from.map_or("", |n| n.label());
        ui.monospace(format!("level {deg:+.1}° from the {from}'s edges"));
    }
    for s in &r.screens {
        if let Some(st) = s.screen.stamps {
            ui.monospace(format!(
                "{} white {:.0} black {:.0}, {:.1} px a module",
                s.screen.name.label(),
                st.white,
                st.black,
                s.screen.module_px
            ));
        }
        if let Some(e) = &s.exposure {
            ui.colored_label(ui.visuals().warn_fg_color, e);
        }
    }
    ui.weak(format!(
        "analysis {:.0} ms, 4 a second; {} frames read",
        r.analysis_ms, r.frames_read
    ));
}

fn quad(points: [Pos2; 4]) -> Vec<Pos2> {
    let mut v = points.to_vec();
    v.push(points[0]);
    v
}

/// the overlay over the preview `image`, which shows a `size` frame.
/// outlines need the unbent picture; with a lens pass showing, only the
/// text is drawn
pub(crate) fn draw(ui: &egui::Ui, image: egui::Rect, size: (u32, u32), r: &Report, outlines: bool) {
    let painter = ui.painter_at(image);
    let (sx, sy) = (
        image.width() / size.0 as f32,
        image.height() / size.1 as f32,
    );
    let map = |p: Pos2| pos2(image.min.x + p.x * sx, image.min.y + p.y * sy);
    let shadow = Color32::from_black_alpha(170);

    if outlines {
        let m = crate::aim::hint::MARGIN;
        let inset = egui::Rect::from_min_max(
            map(pos2(size.0 as f32 * m, size.1 as f32 * m)),
            map(pos2(size.0 as f32 * (1.0 - m), size.1 as f32 * (1.0 - m))),
        );
        painter.extend(egui::Shape::dashed_line(
            &quad([
                inset.left_top(),
                inset.right_top(),
                inset.right_bottom(),
                inset.left_bottom(),
            ]),
            Stroke::new(1.0, Color32::from_gray(140)),
            4.0,
            6.0,
        ));
        for s in &r.screens {
            let sc = &s.screen;
            let c = colour(sc.name);
            if let Some(t) = r.plan.target {
                painter.extend(egui::Shape::dashed_line(
                    &quad(sc.outline.map(|p| map(t.apply(p)))),
                    Stroke::new(1.5, TARGET),
                    8.0,
                    5.0,
                ));
            }
            painter.add(egui::Shape::closed_line(
                sc.outline.map(map).to_vec(),
                Stroke::new(2.0, c),
            ));
            painter.add(egui::Shape::closed_line(
                sc.stamp_block.map(map).to_vec(),
                Stroke::new(1.0, c),
            ));
            for f in &sc.fids {
                let pts = quad(f.quad.map(map));
                if f.seen {
                    painter.add(egui::Shape::line(pts, Stroke::new(2.0, Color32::WHITE)));
                } else {
                    painter.extend(egui::Shape::dashed_line(
                        &pts,
                        Stroke::new(1.0, c),
                        3.0,
                        3.0,
                    ));
                }
            }
            let label = format!("{}: {}", sc.name.label(), view_text(r, sc.name));
            let at = map(sc.outline[0])
                .clamp(image.min + vec2(4.0, 30.0), image.max - vec2(200.0, 20.0));
            let galley = painter.layout_no_wrap(label, FontId::proportional(15.0), c);
            painter.rect_filled(
                egui::Rect::from_min_size(at, galley.size()).expand(3.0),
                2.0,
                shadow,
            );
            painter.galley(at, galley, c);
        }
        if let Some(deg) = r.plan.level_deg {
            let c = image.center();
            let d = vec2(deg.to_radians().cos(), deg.to_radians().sin()) * 120.0;
            painter.line_segment(
                [c - vec2(120.0, 0.0), c + vec2(120.0, 0.0)],
                Stroke::new(1.0, Color32::from_gray(160)),
            );
            painter.line_segment([c - d, c + d], Stroke::new(2.0, HINT));
        }
    }

    // the headline with an arrow per hint, top centre
    let head = headline(r);
    let colour_head = if r.aim_ok { TARGET } else { HINT };
    let galley = painter.layout_no_wrap(head, FontId::proportional(26.0), colour_head);
    let at = pos2(image.center().x - galley.size().x / 2.0, image.min.y + 12.0);
    painter.rect_filled(
        egui::Rect::from_min_size(at, galley.size()).expand(6.0),
        4.0,
        shadow,
    );
    painter.galley(at, galley, colour_head);
    let centre = image.center();
    let stroke = Stroke::new(5.0, HINT);
    for hint in &r.plan.hints {
        match *hint {
            Hint::MoveBack | Hint::ZoomOut | Hint::MoveCloser => {
                let inward = !matches!(hint, Hint::MoveCloser);
                for corner in [
                    vec2(-1.0, -1.0),
                    vec2(1.0, -1.0),
                    vec2(1.0, 1.0),
                    vec2(-1.0, 1.0),
                ] {
                    let far = centre + corner * vec2(image.width(), image.height()) * 0.3;
                    let near = centre + corner * vec2(image.width(), image.height()) * 0.18;
                    let (from, to) = if inward { (far, near) } else { (near, far) };
                    painter.arrow(from, to - from, stroke);
                }
            }
            Hint::Rotate { clockwise, .. } => {
                let radius = 70.0;
                let sign = if clockwise { 1.0 } else { -1.0 };
                let pts: Vec<Pos2> = (0..=16)
                    .map(|i| {
                        let a = (-90.0 + sign * (i as f32) * 5.0).to_radians();
                        centre + vec2(a.cos(), a.sin()) * radius
                    })
                    .collect();
                let (a, b) = (pts[pts.len() - 2], pts[pts.len() - 1]);
                painter.add(egui::Shape::line(pts, stroke));
                painter.arrow(a, (b - a).normalized() * 18.0, stroke);
            }
            h => {
                let dir = h.arrow();
                if dir != egui::Vec2::ZERO {
                    painter.arrow(centre + dir * 40.0, dir * 110.0, stroke);
                }
            }
        }
    }

    let mut lines = Vec::new();
    for name in Name::ALL {
        let s = r.screen(name);
        let rate = s
            .and_then(|s| s.rate)
            .map_or("-".into(), |(rate, _)| format!("{:.0}%", rate * 100.0));
        let focus = s
            .and_then(|s| s.screen.focus)
            .map_or("-".into(), |f| format!("{f:.2}"));
        lines.push(format!(
            "{:<9} {:<22} decode {rate:>4}  focus {focus}",
            name.label(),
            view_text(r, name)
        ));
    }
    if let (Some(deg), Some(from)) = (r.plan.level_deg, r.plan.level_from) {
        lines.push(format!("level {deg:+.1}° from the {}", from.label()));
    }
    lines.extend(r.screens.iter().filter_map(|s| s.exposure.clone()));
    if !outlines {
        lines.push("outlines need the lens off or one pass at most".into());
    }
    let text = lines.join("\n");
    let galley = painter.layout_no_wrap(text, FontId::monospace(13.0), Color32::WHITE);
    // top right, under the headline: the mini usually sits low and the
    // projector high in the middle
    let at = pos2(image.max.x - galley.size().x - 8.0, image.min.y + 60.0);
    painter.rect_filled(
        egui::Rect::from_min_size(at, galley.size()).expand(4.0),
        2.0,
        shadow,
    );
    painter.galley(at, galley, Color32::WHITE);
}

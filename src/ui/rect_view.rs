use std::sync::Arc;

use egui::{pos2, vec2, Color32, FontId, Pos2, Stroke};

use super::aim_panel::colour;
use crate::aim::marks::{self, Rect, FIDS, FRAME, STAMPS};
use crate::aim::Name;
use crate::rect::{Report, Tile};

/// the preview textures, remade only when the worker hands a new tile
#[derive(Default)]
pub(crate) struct Textures {
    held: [Option<(Arc<Tile>, egui::TextureHandle)>; 2],
}

impl Textures {
    fn get(&mut self, ctx: &egui::Context, name: Name, tile: &Arc<Tile>) -> egui::TextureId {
        let slot = &mut self.held[name.index()];
        if !slot.as_ref().is_some_and(|(t, _)| Arc::ptr_eq(t, tile)) {
            let image = egui::ColorImage::from_gray([tile.width, tile.height], &tile.data);
            let handle = ctx.load_texture(
                format!("rect-{}", name.key()),
                image,
                egui::TextureOptions::LINEAR,
            );
            *slot = Some((Arc::clone(tile), handle));
        }
        slot.as_ref().map(|(_, h)| h.id()).expect("just set")
    }
}

fn value(v: Option<u16>) -> String {
    v.map_or("-".into(), |v| v.to_string())
}

/// each screen as the rectifier straightened it, side by side, with the
/// stamp block and the fiducials outlined where the table puts them,
/// the values it decoded and how well the corners fit
pub(crate) fn show(ui: &mut egui::Ui, report: Option<&Report>, textures: &mut Textures) {
    let Some(report) = report else {
        ui.centered_and_justified(|ui| ui.weak("rectify: no screen found yet"));
        return;
    };
    let avail = ui.available_rect_before_wrap();
    ui.allocate_rect(avail, egui::Sense::hover());
    let painter = ui.painter_at(avail);
    let gap = 12.0;
    let cell_w = (avail.width() - 3.0 * gap) / 2.0;
    for (i, name) in Name::ALL.into_iter().enumerate() {
        let left = avail.min.x + gap + i as f32 * (cell_w + gap);
        let scale = (cell_w / FRAME.width()).min((avail.height() - 140.0) / FRAME.height());
        let size = vec2(FRAME.width() * scale, FRAME.height() * scale);
        let image = egui::Rect::from_min_size(pos2(left, avail.min.y + gap), size);
        let c = colour(name);
        let Some(view) = report.screen(name) else {
            painter.rect_stroke(image, 0.0, Stroke::new(1.0, c), egui::StrokeKind::Inside);
            painter.text(
                image.center(),
                egui::Align2::CENTER_CENTER,
                format!("{}: not found", name.label()),
                FontId::proportional(16.0),
                c,
            );
            continue;
        };
        match &view.preview {
            Some(tile) => {
                let id = textures.get(ui.ctx(), name, tile);
                painter.image(
                    id,
                    image,
                    egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                painter.text(
                    image.center(),
                    egui::Align2::CENTER_CENTER,
                    "waiting for a preview",
                    FontId::proportional(14.0),
                    c,
                );
            }
        }
        let map = |p: Pos2| pos2(image.min.x + p.x * scale, image.min.y + p.y * scale);
        let outline = |r: Rect, stroke: Stroke| {
            painter.add(egui::Shape::closed_line(
                r.corners().map(map).to_vec(),
                stroke,
            ));
        };
        outline(FRAME, Stroke::new(1.5, c));
        outline(STAMPS, Stroke::new(1.0, c));
        for f in &FIDS {
            let seen = view.record.ids.contains(&f.id);
            let stroke = if seen {
                Stroke::new(1.5, Color32::WHITE)
            } else {
                Stroke::new(1.0, c.gamma_multiply(0.5))
            };
            outline(f.rect, stroke);
        }
        for (r, _) in [marks::KEY, marks::MID, marks::BOTTOM] {
            outline(r, Stroke::new(1.0, c.gamma_multiply(0.7)));
        }

        let rec = &view.record;
        let st = rec.stamps.unwrap_or_default();
        let g = &rec.geometry;
        let lines = [
            format!(
                "{}  N {}  A {}  M {}",
                name.label(),
                value(st.values[0]),
                value(st.values[1]),
                value(st.values[2])
            ),
            format!(
                "decoded over {} frames: N {:.0}%  A {:.0}%  M {:.0}%",
                crate::rect::HISTORY,
                view.rates[0] * 100.0,
                view.rates[1] * 100.0,
                view.rates[2] * 100.0
            ),
            format!(
                "corner fit {:.2} px rms, {:.2} px max, fids {:?}{}",
                rec.rms,
                rec.worst,
                rec.ids,
                if rec.tracked { "" } else { " (searched)" }
            ),
            format!(
                "scale {:.3}  rot {:+.1}°  skew {:+.1}°  keystone {:+.1}% / {:+.1}%  view {:.0}°",
                g.scale, g.rotation, g.skew, g.keystone_h, g.keystone_v, g.view
            ),
            format!("white {:.0}  black {:.0}", st.white, st.black),
        ];
        let galley =
            painter.layout_no_wrap(lines.join("\n"), FontId::monospace(13.0), Color32::WHITE);
        let at = pos2(left, image.max.y + 8.0);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size()).expand(4.0),
            2.0,
            Color32::from_black_alpha(170),
        );
        painter.galley(at, galley, Color32::WHITE);
    }
    let foot = format!(
        "lens k1 {:+.4} k2 {:+.4}   {:.1} ms cpu a frame   {} skipped",
        report.lens.k1, report.lens.k2, report.cpu_ms, report.skipped
    );
    painter.text(
        pos2(avail.min.x + gap, avail.max.y - gap),
        egui::Align2::LEFT_BOTTOM,
        foot,
        FontId::monospace(12.0),
        Color32::from_gray(180),
    );
}

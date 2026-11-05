use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

const POLL: Duration = Duration::from_millis(200);
const STALE: Duration = Duration::from_secs(5);

/// outlines and text another program writes next to the tap, drawn over
/// the preview. one item a line, coordinates in camera pixels:
///
///   quad LABEL x0 y0 x1 y1 x2 y2 x3 y3
///   text ANY WORDS
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Items {
    pub quads: Vec<(String, [egui::Pos2; 4])>,
    pub text: Vec<String>,
}

pub(crate) fn parse(text: &str) -> Items {
    let mut items = Items::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("text ") {
            items.text.push(rest.to_string());
            continue;
        }
        let mut words = line.split_whitespace();
        if words.next() != Some("quad") {
            continue;
        }
        let Some(label) = words.next() else {
            continue;
        };
        let v: Vec<f32> = words.filter_map(|w| w.parse().ok()).collect();
        if v.len() == 8 {
            let p = |i: usize| egui::pos2(v[2 * i], v[2 * i + 1]);
            items
                .quads
                .push((label.to_string(), [p(0), p(1), p(2), p(3)]));
        }
    }
    items
}

pub(crate) struct Overlay {
    path: PathBuf,
    checked: Option<Instant>,
    modified: Option<SystemTime>,
    items: Items,
}

impl Overlay {
    pub fn new(path: PathBuf) -> Overlay {
        Overlay {
            path,
            checked: None,
            modified: None,
            items: Items::default(),
        }
    }

    /// re-reads the file when its mtime moves, at most every 200 ms
    fn poll(&mut self) {
        if self.checked.is_some_and(|t| t.elapsed() < POLL) {
            return;
        }
        self.checked = Some(Instant::now());
        let modified = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if modified == self.modified {
            return;
        }
        self.modified = modified;
        self.items = fs::read_to_string(&self.path)
            .map(|t| parse(&t))
            .unwrap_or_default();
    }

    /// `image` is where the preview drew a `size` frame. outlines only
    /// line up with an unbent picture, so with a lens pass on just the
    /// text is drawn
    pub fn show(&mut self, ui: &egui::Ui, image: egui::Rect, size: (u32, u32), outlines: bool) {
        self.poll();
        let Some(modified) = self.modified else {
            return;
        };
        let painter = ui.painter_at(image);
        let sx = image.width() / size.0 as f32;
        let sy = image.height() / size.1 as f32;
        let map = |p: egui::Pos2| egui::pos2(image.min.x + p.x * sx, image.min.y + p.y * sy);
        let colour = egui::Color32::from_rgb(255, 210, 0);
        if outlines {
            for (label, quad) in &self.items.quads {
                let pts: Vec<egui::Pos2> = quad.iter().map(|p| map(*p)).collect();
                painter.add(egui::Shape::closed_line(
                    pts.clone(),
                    egui::Stroke::new(1.5, colour),
                ));
                painter.text(
                    pts[0] + egui::vec2(2.0, 2.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::monospace(12.0),
                    colour,
                );
            }
        }
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        let mut lines = self.items.text.clone();
        if age > STALE {
            lines.push(format!("overlay stale, {} s old", age.as_secs()));
        }
        if !outlines && !self.items.quads.is_empty() {
            lines.push("outlines hidden while a lens pass is on".into());
        }
        let text = lines.join("\n");
        let galley = painter.layout_no_wrap(text, egui::FontId::monospace(14.0), colour);
        let at = image.min + egui::vec2(8.0, 8.0);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size()).expand(4.0),
            2.0,
            egui::Color32::from_black_alpha(170),
        );
        painter.galley(at, galley, colour);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quads_and_text() {
        let items =
            parse("quad mini 1 2 3 4 5 6 7 8\ntext fork +2 frames\nquad short 1 2\nnoise\n");
        assert_eq!(items.quads.len(), 1);
        assert_eq!(items.quads[0].0, "mini");
        assert_eq!(items.quads[0].1[3], egui::pos2(7.0, 8.0));
        assert_eq!(items.text, vec!["fork +2 frames".to_string()]);
    }
}

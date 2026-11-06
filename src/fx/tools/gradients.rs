//! gradients: horizontal bands `step` px tall are scanned left to
//! right over their column-mean brightness; a new segment starts where
//! the value drifts more than `threshold` from the segment's first
//! column, and every segment is filled with a white-to-black ramp as
//! a rectangle or an inscribed ellipse. the scan is serial per row, so
//! the pass runs on the cpu

use crate::render::{Pass, PassCtx};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Rect,
    Ellipse,
}

impl Shape {
    pub const ALL: [Shape; 2] = [Shape::Rect, Shape::Ellipse];

    pub fn label(self) -> &'static str {
        match self {
            Shape::Rect => "rect",
            Shape::Ellipse => "ellipse",
        }
    }
}

pub const THRESHOLD_RANGE: std::ops::RangeInclusive<u32> = 0..=255;
/// the site's default of 8 sits below its slider's 15, so the slider
/// here starts at 8
pub const STEP_RANGE: std::ops::RangeInclusive<u32> = 8..=100;
const DEFAULT_THRESHOLD: u32 = 128;
const DEFAULT_STEP: u32 = 8;
const DEFAULT_SHAPE: Shape = Shape::Rect;

/// the segment starts along one band of column means
fn segments(means: &[f32], threshold: f32) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if means.is_empty() {
        return out;
    }
    let mut start = 0;
    let mut first = means[0];
    for (x, &v) in means.iter().enumerate().skip(1) {
        if (v - first).abs() > threshold {
            out.push((start, x));
            start = x;
            first = v;
        }
    }
    out.push((start, means.len()));
    out
}

pub struct Gradients {
    enabled: bool,
    threshold: u32,
    step: u32,
    shape: Shape,
    /// scratch kept across frames: one band's column sums and means
    /// and the ramp row shared by every row of the band
    sums: Vec<u32>,
    means: Vec<f32>,
    ramp: Vec<u8>,
}

impl Gradients {
    pub fn new(_device: &wgpu::Device, _input_layout: &wgpu::BindGroupLayout) -> Gradients {
        Gradients {
            enabled: true,
            threshold: DEFAULT_THRESHOLD,
            step: DEFAULT_STEP,
            shape: DEFAULT_SHAPE,
            sums: Vec::new(),
            means: Vec::new(),
            ramp: Vec::new(),
        }
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, t: u32) {
        self.threshold = t.min(*THRESHOLD_RANGE.end());
    }

    pub fn step(&self) -> u32 {
        self.step
    }

    pub fn set_step(&mut self, px: u32) {
        self.step = px.clamp(*STEP_RANGE.start(), *STEP_RANGE.end());
    }

    pub fn shape(&self) -> Shape {
        self.shape
    }

    pub fn set_shape(&mut self, shape: Shape) {
        self.shape = shape;
    }

    /// back to the site's defaults; the enabled flag is left alone
    pub fn reset(&mut self) {
        self.threshold = DEFAULT_THRESHOLD;
        self.step = DEFAULT_STEP;
        self.shape = DEFAULT_SHAPE;
    }

    /// the site reads a threshold of 0 as 20
    fn effective_threshold(&self) -> f32 {
        if self.threshold == 0 {
            20.0
        } else {
            self.threshold as f32
        }
    }
}

impl Pass for Gradients {
    fn name(&self) -> &'static str {
        "gradients"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.threshold, THRESHOLD_RANGE).text("threshold"));
        ui.add(
            egui::Slider::new(&mut self.step, STEP_RANGE)
                .suffix(" px")
                .text("step"),
        );
        ui.horizontal(|ui| {
            ui.label("shape");
            egui::ComboBox::from_id_salt("gradients shape")
                .selected_text(self.shape.label())
                .show_ui(ui, |ui| {
                    for shape in Shape::ALL {
                        ui.selectable_value(&mut self.shape, shape, shape.label());
                    }
                });
        });
        if ui.button("reset").clicked() {
            self.reset();
        }
    }

    fn cpu(&self) -> bool {
        true
    }

    fn process_cpu(&mut self, rgba: &mut Vec<u8>, width: u32, height: u32) {
        let (width, height) = (width as usize, height as usize);
        let stride = width * 4;
        if width == 0 || rgba.len() < stride * height {
            return;
        }
        let threshold = self.effective_threshold();
        let step = self.step.max(1) as usize;
        let ellipse = self.shape == Shape::Ellipse;
        self.sums.resize(width, 0);
        self.means.resize(width, 0.0);
        self.ramp.resize(stride, 255);
        let mut y0 = 0;
        while y0 < height {
            let y1 = (y0 + step).min(height);
            let rows = y1 - y0;
            self.sums.fill(0);
            for y in y0..y1 {
                let row = &rgba[y * stride..(y + 1) * stride];
                for (x, sum) in self.sums.iter_mut().enumerate() {
                    let i = x * 4;
                    *sum += u32::from(row[i]) + u32::from(row[i + 1]) + u32::from(row[i + 2]);
                }
            }
            let scale = 1.0 / (3 * rows) as f32;
            for (mean, &sum) in self.means.iter_mut().zip(&self.sums) {
                *mean = sum as f32 * scale;
            }
            let segs = segments(&self.means, threshold);
            // every row of a band shares one ramp row; an ellipse just
            // shows less of it per row
            for &(a, b) in &segs {
                let len = (b - a) as f32;
                for x in a..b {
                    let v = ((1.0 - (x - a) as f32 / len) * 255.0).round() as u8;
                    self.ramp[x * 4..x * 4 + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            for y in y0..y1 {
                let row = &mut rgba[y * stride..(y + 1) * stride];
                if !ellipse {
                    row.copy_from_slice(&self.ramp);
                    continue;
                }
                row.fill(255);
                let ry = rows as f32 * 0.5;
                let dy = ((y - y0) as f32 + 0.5 - ry) / ry;
                let s = (1.0 - dy * dy).max(0.0).sqrt();
                for &(a, b) in &segs {
                    let rx = (b - a) as f32 * 0.5;
                    let cx = a as f32 + rx;
                    let lo = (cx - rx * s - 0.5).ceil().max(a as f32) as usize;
                    let hi = (cx + rx * s - 0.5).floor().min((b - 1) as f32);
                    if hi < lo as f32 {
                        continue;
                    }
                    let hi = hi as usize + 1;
                    row[lo * 4..hi * 4].copy_from_slice(&self.ramp[lo * 4..hi * 4]);
                }
            }
            y0 = y1;
        }
    }

    fn encode(
        &mut self,
        _ctx: &PassCtx,
        _encoder: &mut wgpu::CommandEncoder,
        _input: &wgpu::TextureView,
        _output: &wgpu::TextureView,
    ) {
        unreachable!("gradients is a cpu pass");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn segments_split_where_the_value_jumps() {
        let means = [0.0, 10.0, 20.0, 200.0, 210.0, 50.0];
        assert_eq!(segments(&means, 128.0), vec![(0, 3), (3, 5), (5, 6)]);
        assert_eq!(segments(&means, 255.0), vec![(0, 6)]);
        assert!(segments(&[], 128.0).is_empty());
    }

    /// a flat frame is one segment per band: a ramp from white at the
    /// left edge to near black at the right, the same on every row
    #[test]
    fn flat_frame_becomes_one_ramp_per_band() {
        let (w, h) = (16usize, 4usize);
        let mut rgba = vec![130u8; w * h * 4];
        let mut g = Gradients {
            enabled: true,
            threshold: 128,
            step: 8,
            shape: Shape::Rect,
            sums: Vec::new(),
            means: Vec::new(),
            ramp: Vec::new(),
        };
        g.process_cpu(&mut rgba, w as u32, h as u32);
        assert_eq!(rgba[0], 255);
        assert_eq!(rgba[(w - 1) * 4], 16);
        assert_eq!(rgba[(w * 3 + 8) * 4], 128);
        assert_eq!(rgba[3], 255);
    }

    /// an ellipse leaves the band's corners white
    #[test]
    fn ellipse_leaves_the_corners_white() {
        let (w, h) = (16usize, 8usize);
        let mut rgba = vec![0u8; w * h * 4];
        let mut g = Gradients {
            enabled: true,
            threshold: 128,
            step: 8,
            shape: Shape::Ellipse,
            sums: Vec::new(),
            means: Vec::new(),
            ramp: Vec::new(),
        };
        g.process_cpu(&mut rgba, w as u32, h as u32);
        assert_eq!(rgba[0], 255, "top-left corner");
        assert_eq!(rgba[(w - 1) * 4], 255, "top-right corner");
        assert_eq!(rgba[(w * 4) * 4], 255, "the ramp starts white");
        assert!(
            rgba[(w * 4 + 1) * 4] > 200,
            "inside the ellipse near the left edge"
        );
        assert!(
            rgba[(w * 4 + 14) * 4] < 40,
            "inside the ellipse near the right edge"
        );
    }

    /// the seam at x = 32 starts a second segment, so each row holds
    /// two ramps: white at 0 and 32, near black at 31 and 63
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_half_frame_gives_two_ramps_per_row() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        renderer.push_pass(Box::new(Gradients::new(&device, renderer.input_layout())));
        renderer.upload(&device, &queue, &half_frame(64, 32));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        let at = |x: usize, y: usize| pixels[(y * 64 + x) * 4];
        for y in [0, 7, 8, 31] {
            assert_eq!(at(0, y), 255, "row {y}");
            assert_eq!(at(32, y), 255, "row {y}");
            assert!(at(31, y) < 12, "row {y}: {}", at(31, y));
            assert!(at(63, y) < 12, "row {y}: {}", at(63, y));
            assert!((i32::from(at(16, y)) - 128).abs() <= 1, "row {y}");
        }
    }
}

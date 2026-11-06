//! stippling: the site's line halftone, a rotated grid of black bars
//! whose width follows the luminance at each cell centre

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

use super::Grid;

/// mirrors `Params` in stippling.wgsl
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub threshold: f32,
    pub angle: f32,
    pub x_squares: f32,
    pub y_squares: f32,
    pub width_min: f32,
    pub width_max: f32,
    pub norm: f32,
    pub benday: u32,
    pub reach: i32,
    pub _pad: [u32; 3],
}

pub const THRESHOLD_RANGE: std::ops::RangeInclusive<u32> = 0..=255;
pub const ANGLE_RANGE: std::ops::RangeInclusive<f32> = -45.0..=45.0;
pub const SQUARES_RANGE: std::ops::RangeInclusive<u32> = 1..=100;
pub const WIDTH_RANGE: std::ops::RangeInclusive<u32> = 0..=50;
/// the most columns walked each way; a bar wider than that many cells
/// is clipped, which takes a frame under 100 px wide
const MAX_REACH: i32 = 16;

pub struct Stippling {
    enabled: bool,
    threshold: u32,
    grid: Grid,
    /// degrees
    angle: f32,
    y_squares: u32,
    x_squares: u32,
    width_min: u32,
    width_max: u32,
    pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
}

impl Stippling {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Stippling {
        let module = build_shader(device, "stippling", include_str!("stippling.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "stippling params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stippling"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "stippling", &module, "fs_main", &layout);
        let mut stippling = Stippling {
            enabled: true,
            threshold: 0,
            grid: Grid::Regular,
            angle: 0.0,
            y_squares: 1,
            x_squares: 1,
            width_min: 0,
            width_max: 0,
            pipeline,
            uniform,
            input: InputGroupCache::new(),
        };
        stippling.reset();
        stippling
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, threshold: u32) {
        self.threshold = threshold.min(*THRESHOLD_RANGE.end());
    }

    pub fn grid(&self) -> Grid {
        self.grid
    }

    pub fn set_grid(&mut self, grid: Grid) {
        self.grid = grid;
    }

    pub fn angle(&self) -> f32 {
        self.angle
    }

    pub fn set_angle(&mut self, degrees: f32) {
        self.angle = degrees.clamp(*ANGLE_RANGE.start(), *ANGLE_RANGE.end());
    }

    /// columns and rows across the frame
    pub fn squares(&self) -> (u32, u32) {
        (self.x_squares, self.y_squares)
    }

    pub fn set_squares(&mut self, x: u32, y: u32) {
        self.x_squares = x.clamp(*SQUARES_RANGE.start(), *SQUARES_RANGE.end());
        self.y_squares = y.clamp(*SQUARES_RANGE.start(), *SQUARES_RANGE.end());
    }

    /// min and max bar width in pixels
    pub fn widths(&self) -> (u32, u32) {
        (self.width_min, self.width_max)
    }

    pub fn set_widths(&mut self, min: u32, max: u32) {
        self.width_min = min.min(*WIDTH_RANGE.end());
        self.width_max = max.min(*WIDTH_RANGE.end());
    }

    pub fn reset(&mut self) {
        self.threshold = 128;
        self.grid = Grid::Regular;
        self.angle = 0.0;
        self.y_squares = 90;
        self.x_squares = 90;
        self.width_min = 1;
        self.width_max = 4;
    }

    fn norm(&self) -> f32 {
        let a = self.angle.to_radians();
        a.cos().abs() + a.sin().abs()
    }

    /// columns each way whose bar can cross a pixel: half the widest
    /// bar over the column spacing
    fn reach(&self, width: u32) -> i32 {
        let norm = self.norm();
        let col_w = width as f32 / self.x_squares as f32 / norm;
        let half = self.width_min.max(self.width_max) as f32 / norm * 0.5;
        ((half / col_w).ceil() as i32).clamp(1, MAX_REACH)
    }

    fn uniform(&self, width: u32) -> Uniform {
        Uniform {
            threshold: self.threshold as f32,
            angle: self.angle.to_radians(),
            x_squares: self.x_squares as f32,
            y_squares: self.y_squares as f32,
            width_min: self.width_min as f32,
            width_max: self.width_max as f32,
            norm: self.norm(),
            benday: u32::from(self.grid == Grid::Benday),
            reach: self.reach(width),
            _pad: [0; 3],
        }
    }
}

impl Pass for Stippling {
    fn name(&self) -> &'static str {
        "stippling"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.threshold, THRESHOLD_RANGE).text("threshold"));
        ui.horizontal(|ui| {
            ui.label("grid type");
            egui::ComboBox::from_id_salt("stippling grid")
                .selected_text(self.grid.label())
                .show_ui(ui, |ui| {
                    for grid in Grid::ALL {
                        ui.selectable_value(&mut self.grid, grid, grid.label());
                    }
                });
        });
        ui.add(
            egui::Slider::new(&mut self.angle, ANGLE_RANGE)
                .suffix("°")
                .text("angle"),
        );
        ui.add(egui::Slider::new(&mut self.y_squares, SQUARES_RANGE).text("y squares"));
        ui.add(egui::Slider::new(&mut self.x_squares, SQUARES_RANGE).text("x squares"));
        ui.add(
            egui::Slider::new(&mut self.width_min, WIDTH_RANGE)
                .suffix(" px")
                .text("min width"),
        );
        ui.add(
            egui::Slider::new(&mut self.width_max, WIDTH_RANGE)
                .suffix(" px")
                .text("max width"),
        );
        if ui.button("reset").clicked() {
            self.reset();
        }
    }

    fn encode(
        &mut self,
        ctx: &PassCtx,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
    ) {
        self.uniform.write(ctx.queue, &self.uniform(ctx.width));
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "stippling",
            &self.pipeline,
            &[input_group, &self.uniform.group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_48_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 48);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, threshold), 0);
        assert_eq!(std::mem::offset_of!(Uniform, norm), 24);
        assert_eq!(std::mem::offset_of!(Uniform, benday), 28);
        assert_eq!(std::mem::offset_of!(Uniform, reach), 32);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 48);
    }

    /// eight columns on a 64 px frame are 8 px apart: black cells draw
    /// 4 px bars, half the width, and white cells 1 px bars
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_dark_half_has_wider_bars() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let mut stippling = Stippling::new(&device, renderer.input_layout());
        stippling.set_squares(8, 4);
        assert_eq!(stippling.reach(64), 1);
        renderer.push_pass(Box::new(stippling));
        let (w, h) = (64, 32);
        renderer.upload(&device, &queue, &half_frame(w, h));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), w, h);
        // ink per half: a 1 px bar on an integer column centre lands
        // half on each of two pixels, so the coverage is summed rather
        // than thresholded
        let mut ink = [0.0f32; 2];
        for y in 0..h {
            for x in 0..w {
                let v = pixels[((y * w + x) * 4) as usize];
                ink[usize::from(x >= w / 2)] += (255 - v) as f32 / 255.0;
            }
        }
        let half = (w * h / 2) as f32;
        let (dark, bright) = (ink[0] / half, ink[1] / half);
        assert!((dark - 0.5).abs() < 0.05, "dark half is {dark} ink");
        assert!((bright - 0.125).abs() < 0.05, "bright half is {bright} ink");
        // bars run the full row height, so a column is black top to bottom
        let column = |x: u32| (0..h).all(|y| pixels[((y * w + x) * 4) as usize] < 128);
        assert!((0..w / 2).filter(|&x| column(x)).count() >= 12);
        assert!(pixels.chunks_exact(4).all(|px| px[3] == 255));
    }
}

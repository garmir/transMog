//! dots: a rotated, perlin-jittered grid of black rounded squares, each
//! sized by the luminance under it

use bytemuck::{Pod, Zeroable};

use super::Grid;
use crate::render::kit::{build_shader, InputGroupCache, OffscreenTarget, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

/// mirrors `Params` in dots.wgsl: 64 bytes, the two vec2 on 8-byte
/// boundaries
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub threshold: f32,
    pub angle: f32,
    pub size_min: f32,
    pub size_max: f32,
    pub radius: f32,
    pub step: f32,
    pub noise: f32,
    pub norm: f32,
    pub benday: u32,
    pub reach: i32,
    pub origin: [i32; 2],
    pub canvas_centre: [f32; 2],
    pub _pad: [u32; 2],
}

pub const THRESHOLD_RANGE: std::ops::RangeInclusive<u32> = 0..=255;
pub const ANGLE_RANGE: std::ops::RangeInclusive<f32> = -45.0..=45.0;
pub const SIZE_RANGE: std::ops::RangeInclusive<u32> = 0..=50;
pub const RADIUS_RANGE: std::ops::RangeInclusive<u32> = 0..=20;
pub const STEP_RANGE: std::ops::RangeInclusive<u32> = 3..=20;
pub const NOISE_RANGE: std::ops::RangeInclusive<u32> = 0..=20;
/// the widest neighbourhood fs_draw walks, 9x9 cells; a dot that
/// reaches further than this is clipped at the cell border
const MAX_REACH: i32 = 4;

pub struct Dots {
    enabled: bool,
    threshold: u32,
    grid: Grid,
    /// degrees
    angle: f32,
    size_min: u32,
    size_max: u32,
    radius: u32,
    step: u32,
    noise: u32,
    cells_pipeline: wgpu::RenderPipeline,
    draw_pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
    cells_input: InputGroupCache,
    /// one texel per grid point over the rotated frame's extent
    cells: OffscreenTarget,
}

impl Dots {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Dots {
        let module = build_shader(device, "dots", include_str!("dots.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "dots params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dots"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let cells_pipeline =
            create_fullscreen_pipeline(device, "dots cells", &module, "fs_cells", &layout);
        let draw_pipeline =
            create_fullscreen_pipeline(device, "dots draw", &module, "fs_draw", &layout);
        let mut dots = Dots {
            enabled: true,
            threshold: 0,
            grid: Grid::Regular,
            angle: 0.0,
            size_min: 0,
            size_max: 0,
            radius: 0,
            step: 3,
            noise: 0,
            cells_pipeline,
            draw_pipeline,
            uniform,
            input: InputGroupCache::new(),
            cells_input: InputGroupCache::new(),
            cells: OffscreenTarget::new(device, 1, 1, "dots cells"),
        };
        dots.reset();
        dots
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

    /// min and max dot size in pixels
    pub fn sizes(&self) -> (u32, u32) {
        (self.size_min, self.size_max)
    }

    pub fn set_sizes(&mut self, min: u32, max: u32) {
        self.size_min = min.min(*SIZE_RANGE.end());
        self.size_max = max.min(*SIZE_RANGE.end());
    }

    pub fn radius(&self) -> u32 {
        self.radius
    }

    pub fn set_radius(&mut self, radius: u32) {
        self.radius = radius.min(*RADIUS_RANGE.end());
    }

    pub fn step(&self) -> u32 {
        self.step
    }

    pub fn set_step(&mut self, step: u32) {
        self.step = step.clamp(*STEP_RANGE.start(), *STEP_RANGE.end());
    }

    pub fn noise(&self) -> u32 {
        self.noise
    }

    pub fn set_noise(&mut self, noise: u32) {
        self.noise = noise.min(*NOISE_RANGE.end());
    }

    pub fn reset(&mut self) {
        self.threshold = 128;
        self.grid = Grid::Regular;
        self.angle = 0.0;
        self.size_min = 1;
        self.size_max = 10;
        self.radius = 4;
        self.step = 8;
        self.noise = 2;
    }

    fn norm(&self) -> f32 {
        let a = self.angle.to_radians();
        a.cos().abs() + a.sin().abs()
    }

    /// cells each way whose dot can touch a pixel: half the largest
    /// dot plus the jitter, over the step, plus one for the half-step
    /// rounding and the benday shift
    fn reach(&self) -> i32 {
        let half = self.size_min.max(self.size_max) as f32 / self.norm() * 0.5;
        let cells = ((half + self.noise as f32) / self.step as f32 + 1.0).floor() as i32;
        cells.clamp(1, MAX_REACH)
    }

    /// the cell map's size and its origin texel: the rotated frame's
    /// half extents in grid space, in cells, plus the reach each way
    fn cell_layout(&self, width: u32, height: u32) -> ((u32, u32), [i32; 2]) {
        let a = self.angle.to_radians();
        let (w, h) = (width as f32, height as f32);
        let hx = (w * a.cos().abs() + h * a.sin().abs()) * 0.5;
        let hy = (w * a.sin().abs() + h * a.cos().abs()) * 0.5;
        let step = self.step as f32;
        let reach = self.reach();
        let nx = (hx / step).ceil() as i32 + reach + 1;
        let ny = (hy / step).ceil() as i32 + reach + 1;
        (((2 * nx + 1) as u32, (2 * ny + 1) as u32), [nx, ny])
    }

    fn uniform(&self, width: u32, height: u32) -> Uniform {
        let (_, origin) = self.cell_layout(width, height);
        Uniform {
            threshold: self.threshold as f32,
            angle: self.angle.to_radians(),
            size_min: self.size_min as f32,
            size_max: self.size_max as f32,
            radius: self.radius as f32,
            step: self.step as f32,
            noise: self.noise as f32,
            norm: self.norm(),
            benday: u32::from(self.grid == Grid::Benday),
            reach: self.reach(),
            origin,
            canvas_centre: [width as f32 * 0.5, height as f32 * 0.5],
            _pad: [0; 2],
        }
    }
}

impl Pass for Dots {
    fn name(&self) -> &'static str {
        "dots"
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
            egui::ComboBox::from_id_salt("dots grid")
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
        ui.add(
            egui::Slider::new(&mut self.size_min, SIZE_RANGE)
                .suffix(" px")
                .text("min size"),
        );
        ui.add(
            egui::Slider::new(&mut self.size_max, SIZE_RANGE)
                .suffix(" px")
                .text("max size"),
        );
        ui.add(
            egui::Slider::new(&mut self.radius, RADIUS_RANGE)
                .suffix(" px")
                .text("corner radius"),
        );
        ui.add(
            egui::Slider::new(&mut self.step, STEP_RANGE)
                .suffix(" px")
                .text("step"),
        );
        ui.add(
            egui::Slider::new(&mut self.noise, NOISE_RANGE)
                .suffix(" px")
                .text("noise"),
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
        self.uniform
            .write(ctx.queue, &self.uniform(ctx.width, ctx.height));
        let ((cells_w, cells_h), _) = self.cell_layout(ctx.width, ctx.height);
        self.cells
            .ensure(ctx.device, cells_w, cells_h, "dots cells");
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "dots cells",
            &self.cells_pipeline,
            &[input_group, &self.uniform.group],
            &self.cells.view,
        );
        let cells_group = self.cells_input.get(ctx, &self.cells.view);
        draw_fullscreen(
            encoder,
            "dots draw",
            &self.draw_pipeline,
            &[cells_group, &self.uniform.group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_64_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 64);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, threshold), 0);
        assert_eq!(std::mem::offset_of!(Uniform, norm), 28);
        assert_eq!(std::mem::offset_of!(Uniform, benday), 32);
        assert_eq!(std::mem::offset_of!(Uniform, reach), 36);
        assert_eq!(std::mem::offset_of!(Uniform, origin), 40);
        assert_eq!(std::mem::offset_of!(Uniform, canvas_centre), 48);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 64);
    }

    /// black cells get 10 px dots on an 8 px step and white cells 1 px
    /// dots, so the dark half is mostly black and the bright half mostly
    /// white
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_dark_half_has_more_black_than_bright_half() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let dots = Dots::new(&device, renderer.input_layout());
        assert_eq!(dots.reach(), 1);
        renderer.push_pass(Box::new(dots));
        let (w, h) = (64, 32);
        renderer.upload(&device, &queue, &half_frame(w, h));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), w, h);
        let (dark, bright) = black_fractions(&pixels, w, h);
        assert!(dark > 0.5, "dark half is {dark} black");
        assert!(bright < 0.2, "bright half is {bright} black");
        let touched = (0..h)
            .flat_map(|y| (w / 2..w).map(move |x| (x, y)))
            .filter(|&(x, y)| pixels[((y * w + x) * 4) as usize] < 250)
            .count();
        assert!(touched > 0, "bright half has no 1 px dots");
        assert!(pixels.chunks_exact(4).all(|px| px[3] == 255));

        // a smaller frame rebuilds the cell map and still renders
        renderer.upload(&device, &queue, &half_frame(40, 16));
        renderer.render(&device, &queue);
        let small = read_back(&device, &queue, renderer.output_texture().unwrap(), 40, 16);
        let (dark, bright) = black_fractions(&small, 40, 16);
        assert!(dark > bright, "small frame: dark {dark}, bright {bright}");
    }

    /// the share of pixels darker than mid grey in each half
    fn black_fractions(pixels: &[u8], width: u32, height: u32) -> (f32, f32) {
        let mut counts = [0u32; 2];
        for y in 0..height {
            for x in 0..width {
                let v = pixels[((y * width + x) * 4) as usize];
                if v < 128 {
                    counts[usize::from(x >= width / 2)] += 1;
                }
            }
        }
        let half = (width * height / 2) as f32;
        (counts[0] as f32 / half, counts[1] as f32 / half)
    }
}

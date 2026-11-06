//! edge: a black rounded square on every grid cell whose sobel
//! magnitude passes the threshold, sized by how strong the edge is

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, OffscreenTarget, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

/// mirrors `Params` in edge.wgsl
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub threshold: f32,
    pub size_min: f32,
    pub size_max: f32,
    pub radius: f32,
    pub step: f32,
    pub reach: i32,
    pub _pad0: u32,
    pub _pad1: u32,
}

pub const THRESHOLD_RANGE: std::ops::RangeInclusive<u32> = 0..=255;
pub const SIZE_RANGE: std::ops::RangeInclusive<u32> = 0..=40;
pub const RADIUS_RANGE: std::ops::RangeInclusive<u32> = 0..=20;
pub const STEP_RANGE: std::ops::RangeInclusive<u32> = 3..=20;
const DEFAULT_THRESHOLD: u32 = 255;
const DEFAULT_SIZE_MIN: u32 = 0;
const DEFAULT_SIZE_MAX: u32 = 12;
const DEFAULT_RADIUS: u32 = 8;
const DEFAULT_STEP: u32 = 5;

pub struct Edge {
    enabled: bool,
    threshold: u32,
    size_min: u32,
    size_max: u32,
    radius: u32,
    step: u32,
    cells_pipeline: wgpu::RenderPipeline,
    draw_pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
    cells_input: InputGroupCache,
    /// one texel per grid cell, so it follows the frame size and the step
    cells: OffscreenTarget,
}

impl Edge {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Edge {
        let module = build_shader(device, "edge", include_str!("edge.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "edge params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("edge"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let cells_pipeline =
            create_fullscreen_pipeline(device, "edge cells", &module, "fs_cells", &layout);
        let draw_pipeline =
            create_fullscreen_pipeline(device, "edge draw", &module, "fs_draw", &layout);
        Edge {
            enabled: true,
            threshold: DEFAULT_THRESHOLD,
            size_min: DEFAULT_SIZE_MIN,
            size_max: DEFAULT_SIZE_MAX,
            radius: DEFAULT_RADIUS,
            step: DEFAULT_STEP,
            cells_pipeline,
            draw_pipeline,
            uniform,
            input: InputGroupCache::new(),
            cells_input: InputGroupCache::new(),
            cells: OffscreenTarget::new(device, 1, 1, "edge cells"),
        }
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, threshold: u32) {
        self.threshold = threshold.min(*THRESHOLD_RANGE.end());
    }

    /// min and max square size in pixels
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

    pub fn reset(&mut self) {
        self.threshold = DEFAULT_THRESHOLD;
        self.size_min = DEFAULT_SIZE_MIN;
        self.size_max = DEFAULT_SIZE_MAX;
        self.radius = DEFAULT_RADIUS;
        self.step = DEFAULT_STEP;
    }

    /// the cell map size for a frame: one cell per step, the last one
    /// partial
    fn cell_count(&self, width: u32, height: u32) -> (u32, u32) {
        (width.div_ceil(self.step), height.div_ceil(self.step))
    }

    fn uniform(&self) -> Uniform {
        Uniform {
            threshold: self.threshold as f32,
            size_min: self.size_min as f32,
            size_max: self.size_max as f32,
            radius: self.radius as f32,
            step: self.step as f32,
            reach: self.size_min.max(self.size_max).div_ceil(self.step) as i32,
            _pad0: 0,
            _pad1: 0,
        }
    }
}

impl Pass for Edge {
    fn name(&self) -> &'static str {
        "edge"
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
        self.uniform.write(ctx.queue, &self.uniform());
        let (cells_w, cells_h) = self.cell_count(ctx.width, ctx.height);
        self.cells
            .ensure(ctx.device, cells_w, cells_h, "edge cells");
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "edge cells",
            &self.cells_pipeline,
            &[input_group, &self.uniform.group],
            &self.cells.view,
        );
        let cells_group = self.cells_input.get(ctx, &self.cells.view);
        draw_fullscreen(
            encoder,
            "edge draw",
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
    fn uniform_is_32_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 32);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, threshold), 0);
        assert_eq!(std::mem::offset_of!(Uniform, step), 16);
        assert_eq!(std::mem::offset_of!(Uniform, reach), 20);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 32);
    }

    /// a black/white step at column 32 with step 4 puts a cell's sample
    /// pixel on the first white column, where the sobel magnitude is
    /// 1020; at the default threshold of 255 the square is the max
    /// size, 12 px with its corners rounded down to a circle, so the
    /// column through the circle centres at x 38 goes black and the
    /// columns left of 32 and right of 44 stay white
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_squares_cover_the_step_edge() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let mut edge = Edge::new(&device, renderer.input_layout());
        edge.set_step(4);
        renderer.push_pass(Box::new(edge));
        let (w, h) = (64, 32);
        renderer.upload(&device, &queue, &half_frame(w, h));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), w, h);
        let at = |x: u32, y: u32| pixels[((y * w + x) * 4) as usize];
        for y in 0..h {
            assert_eq!(at(10, y), 255, "row {y} left of the edge");
            assert_eq!(at(31, y), 255, "row {y} just left of the edge");
            assert!(at(38, y) <= 8, "row {y} inside the squares: {}", at(38, y));
            assert_eq!(at(50, y), 255, "row {y} right of the squares");
        }
        assert_eq!(pixels[3], 255);

        // a smaller frame rebuilds the cell map and still renders
        renderer.upload(&device, &queue, &half_frame(40, 16));
        renderer.render(&device, &queue);
        let small = read_back(&device, &queue, renderer.output_texture().unwrap(), 40, 16);
        assert_eq!(small.len(), 40 * 16 * 4);
        assert_eq!(small[(8 * 40 + 22) * 4], 0);
        assert_eq!(small[(8 * 40 + 4) * 4], 255);
    }
}

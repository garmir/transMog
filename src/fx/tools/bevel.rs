//! bevel: the site's emboss, a one-pixel luminance difference towards
//! the light scaled by depth

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

/// mirrors `Params` in bevel.wgsl
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub depth: f32,
    pub threshold: f32,
    pub offset: [i32; 2],
}

pub const DEPTH_RANGE: std::ops::RangeInclusive<f32> = 0.0..=500.0;
pub const ANGLE_RANGE: std::ops::RangeInclusive<f32> = 0.0..=360.0;
pub const THRESHOLD_RANGE: std::ops::RangeInclusive<f32> = 0.0..=4.0;
const DEFAULT_DEPTH: f32 = 20.0;
const DEFAULT_ANGLE: f32 = 0.0;
const DEFAULT_THRESHOLD: f32 = 0.0;

pub struct Bevel {
    enabled: bool,
    depth: f32,
    /// light direction in degrees, a multiple of 45
    angle: f32,
    /// the smallest luminance step, in 0..255 units, that embosses
    threshold: f32,
    pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
}

impl Bevel {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Bevel {
        let module = build_shader(device, "bevel", include_str!("bevel.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "bevel params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("bevel"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "bevel", &module, "fs_main", &layout);
        Bevel {
            enabled: true,
            depth: DEFAULT_DEPTH,
            angle: DEFAULT_ANGLE,
            threshold: DEFAULT_THRESHOLD,
            pipeline,
            uniform,
            input: InputGroupCache::new(),
        }
    }

    pub fn depth(&self) -> f32 {
        self.depth
    }

    pub fn set_depth(&mut self, depth: f32) {
        self.depth = depth.clamp(*DEPTH_RANGE.start(), *DEPTH_RANGE.end());
    }

    pub fn angle(&self) -> f32 {
        self.angle
    }

    /// snapped to the site's 45 degree steps
    pub fn set_angle(&mut self, degrees: f32) {
        self.angle = snap_angle(degrees);
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold.clamp(*THRESHOLD_RANGE.start(), *THRESHOLD_RANGE.end());
    }

    pub fn reset(&mut self) {
        self.depth = DEFAULT_DEPTH;
        self.angle = DEFAULT_ANGLE;
        self.threshold = DEFAULT_THRESHOLD;
    }

    fn uniform(&self) -> Uniform {
        Uniform {
            depth: self.depth,
            threshold: self.threshold,
            offset: light_offset(self.angle),
        }
    }
}

fn snap_angle(degrees: f32) -> f32 {
    let snapped = (degrees / 45.0).round() * 45.0;
    snapped.clamp(*ANGLE_RANGE.start(), *ANGLE_RANGE.end())
}

/// the neighbour the site reads: round(cos a, sin a), which at 45
/// degree steps is one of the eight pixels around, y down
fn light_offset(degrees: f32) -> [i32; 2] {
    let a = degrees.to_radians();
    [a.cos().round() as i32, a.sin().round() as i32]
}

impl Pass for Bevel {
    fn name(&self) -> &'static str {
        "bevel"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.depth, DEPTH_RANGE).text("depth"));
        ui.add(
            egui::Slider::new(&mut self.angle, ANGLE_RANGE)
                .step_by(45.0)
                .suffix("°")
                .text("light angle"),
        );
        ui.add(
            egui::Slider::new(&mut self.threshold, THRESHOLD_RANGE)
                .step_by(0.01)
                .text("effect threshold"),
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
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "bevel",
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
    fn uniform_is_16_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 16);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, depth), 0);
        assert_eq!(std::mem::offset_of!(Uniform, threshold), 4);
        assert_eq!(std::mem::offset_of!(Uniform, offset), 8);
    }

    /// cos and sin of the eight light angles round to the eight
    /// neighbours, with y down like the canvas
    #[test]
    fn light_angle_picks_a_neighbour() {
        let expected = [
            (0.0, [1, 0]),
            (45.0, [1, 1]),
            (90.0, [0, 1]),
            (135.0, [-1, 1]),
            (180.0, [-1, 0]),
            (225.0, [-1, -1]),
            (270.0, [0, -1]),
            (315.0, [1, -1]),
            (360.0, [1, 0]),
        ];
        for (angle, offset) in expected {
            assert_eq!(light_offset(angle), offset, "angle {angle}");
        }
        assert_eq!(snap_angle(50.0), 45.0);
        assert_eq!(snap_angle(400.0), 360.0);
    }

    /// a frame that is black on the left and white on the right: the
    /// flat halves emboss to mid grey, the step column goes white
    /// because the neighbour to the right is brighter
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_flat_areas_are_mid_grey() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        renderer.push_pass(Box::new(Bevel::new(&device, renderer.input_layout())));
        let (w, h) = (64, 32);
        renderer.upload(&device, &queue, &half_frame(w, h));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), w, h);
        for y in 0..h {
            for x in 0..w {
                let px = &pixels[((y * w + x) * 4) as usize..][..4];
                let expected = if x == w / 2 - 1 { 255 } else { 128 };
                assert!(
                    (i32::from(px[0]) - expected).abs() <= 1,
                    "pixel ({x}, {y}) is {px:?}, expected {expected}"
                );
                assert_eq!(px[0], px[1]);
                assert_eq!(px[1], px[2]);
                assert_eq!(px[3], 255);
            }
        }
    }
}

//! the shared preprocessing every tooooools effect sees first: blur,
//! grain, gamma and levels, with defaults that leave the frame alone

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, OffscreenTarget, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

/// mirrors `Params` in preprocess.wgsl
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub blur: f32,
    pub grain: f32,
    pub gamma: f32,
    pub black: f32,
    pub white: f32,
    pub frame: u32,
    pub _pad0: u32,
    pub _pad1: u32,
}

pub const BLUR_RANGE: std::ops::RangeInclusive<f32> = 0.0..=10.0;
pub const GRAIN_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;
pub const GAMMA_RANGE: std::ops::RangeInclusive<f32> = 0.1..=2.0;
pub const LEVEL_RANGE: std::ops::RangeInclusive<u32> = 0..=255;

pub struct Preprocess {
    enabled: bool,
    blur: f32,
    grain: f32,
    gamma: f32,
    black: u32,
    white: u32,
    /// grain seed, stepped on every encode
    frame: u32,
    blur_h: wgpu::RenderPipeline,
    blur_v: wgpu::RenderPipeline,
    tone: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
    scratch_input: InputGroupCache,
    scratch: OffscreenTarget,
}

impl Preprocess {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Preprocess {
        let module = build_shader(device, "preprocess", include_str!("preprocess.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "preprocess params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("preprocess"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let pipeline =
            |label, entry| create_fullscreen_pipeline(device, label, &module, entry, &layout);
        Preprocess {
            enabled: true,
            blur: 0.0,
            grain: 0.0,
            gamma: 1.0,
            black: 0,
            white: 255,
            frame: 0,
            blur_h: pipeline("preprocess blur h", "fs_blur_h"),
            blur_v: pipeline("preprocess blur v", "fs_blur_v"),
            tone: pipeline("preprocess tone", "fs_tone"),
            uniform,
            input: InputGroupCache::new(),
            scratch_input: InputGroupCache::new(),
            scratch: OffscreenTarget::new(device, 1, 1, "preprocess scratch"),
        }
    }

    pub fn blur(&self) -> f32 {
        self.blur
    }

    pub fn set_blur(&mut self, blur: f32) {
        self.blur = blur.clamp(*BLUR_RANGE.start(), *BLUR_RANGE.end());
    }

    pub fn grain(&self) -> f32 {
        self.grain
    }

    pub fn set_grain(&mut self, grain: f32) {
        self.grain = grain.clamp(*GRAIN_RANGE.start(), *GRAIN_RANGE.end());
    }

    pub fn gamma(&self) -> f32 {
        self.gamma
    }

    pub fn set_gamma(&mut self, gamma: f32) {
        self.gamma = gamma.clamp(*GAMMA_RANGE.start(), *GAMMA_RANGE.end());
    }

    /// black and white points, 0..255
    pub fn levels(&self) -> (u32, u32) {
        (self.black, self.white)
    }

    pub fn set_levels(&mut self, black: u32, white: u32) {
        self.black = black.min(255);
        self.white = white.min(255);
    }

    pub fn reset(&mut self) {
        self.blur = 0.0;
        self.grain = 0.0;
        self.gamma = 1.0;
        self.black = 0;
        self.white = 255;
    }

    /// true when every control is at its identity value
    pub fn is_identity(&self) -> bool {
        self.blur <= 0.0
            && self.grain <= 0.0
            && (self.gamma - 1.0).abs() < 1e-6
            && self.black == 0
            && self.white == 255
    }

    fn uniform(&self) -> Uniform {
        Uniform {
            blur: self.blur,
            grain: self.grain,
            gamma: self.gamma,
            black: self.black as f32,
            white: self.white as f32,
            frame: self.frame,
            _pad0: 0,
            _pad1: 0,
        }
    }
}

impl Pass for Preprocess {
    fn name(&self) -> &'static str {
        "preprocess"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.blur, BLUR_RANGE).text("blur"));
        ui.add(
            egui::Slider::new(&mut self.grain, GRAIN_RANGE)
                .step_by(0.1)
                .text("grain"),
        );
        ui.add(
            egui::Slider::new(&mut self.gamma, GAMMA_RANGE)
                .step_by(0.1)
                .text("gamma"),
        );
        ui.add(egui::Slider::new(&mut self.black, LEVEL_RANGE).text("black point"));
        ui.add(egui::Slider::new(&mut self.white, LEVEL_RANGE).text("white point"));
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
        // the seed stays an exact f32 integer in the shader
        self.frame = (self.frame + 1) % (1 << 20);
        self.uniform.write(ctx.queue, &self.uniform());
        let input_group = self.input.get(ctx, input);
        if self.blur <= 0.0 {
            draw_fullscreen(
                encoder,
                "preprocess tone",
                &self.tone,
                &[input_group, &self.uniform.group],
                output,
            );
            return;
        }
        self.scratch
            .ensure(ctx.device, ctx.width, ctx.height, "preprocess scratch");
        draw_fullscreen(
            encoder,
            "preprocess blur h",
            &self.blur_h,
            &[input_group, &self.uniform.group],
            &self.scratch.view,
        );
        let scratch_group = self.scratch_input.get(ctx, &self.scratch.view);
        draw_fullscreen(
            encoder,
            "preprocess blur v",
            &self.blur_v,
            &[scratch_group, &self.uniform.group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_is_32_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 32);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, blur), 0);
        assert_eq!(std::mem::offset_of!(Uniform, white), 16);
        assert_eq!(std::mem::offset_of!(Uniform, frame), 20);
    }

    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_setters_clamp_to_the_site_ranges() {
        let (device, queue) = crate::render::tests::headless_device().expect("no gpu adapter");
        let renderer = crate::render::Renderer::new(&device, &queue);
        let mut p = Preprocess::new(&device, renderer.input_layout());
        assert!(p.is_identity());
        p.set_blur(99.0);
        p.set_gamma(0.0);
        p.set_levels(300, 300);
        assert_eq!(p.blur(), 10.0);
        assert_eq!(p.gamma(), 0.1);
        assert_eq!(p.levels(), (255, 255));
        assert!(!p.is_identity());
        p.reset();
        assert!(p.is_identity());
    }

    /// identity settings leave grey 130 at 130; with blur on, the
    /// two-pass path runs and a flat frame stays flat
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_identity_and_blur_keep_grey() {
        use crate::render::tests::{headless_device, render_grey};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let identity = Preprocess::new(&device, renderer.input_layout());
        let mut blurred = Preprocess::new(&device, renderer.input_layout());
        blurred.set_blur(3.0);
        renderer.push_pass(Box::new(identity));
        let pixels = render_grey(&device, &queue, &mut renderer, 8, 4);
        assert!(pixels
            .chunks_exact(4)
            .all(|px| (i32::from(px[0]) - 130).abs() <= 2));

        renderer.passes_mut()[0].pass = Box::new(blurred);
        let pixels = render_grey(&device, &queue, &mut renderer, 8, 4);
        assert!(pixels
            .chunks_exact(4)
            .all(|px| (i32::from(px[0]) - 130).abs() <= 2));
        assert_eq!(renderer.output_index(), 1);
    }
}

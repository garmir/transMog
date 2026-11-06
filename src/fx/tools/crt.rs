//! crt: the site's glsl pipeline, a bright pass, the masked and
//! distorted main pass, a separable blur of the bright pass and a
//! blend of the two, through three offscreen targets

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, OffscreenTarget, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Monitor,
    Tv,
    Lcd,
}

impl Kind {
    /// the site's menu order
    pub const ALL: [Kind; 3] = [Kind::Monitor, Kind::Tv, Kind::Lcd];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Monitor => "monitor",
            Kind::Tv => "tv",
            Kind::Lcd => "lcd",
        }
    }

    /// the site's shader ids, matched by the constants in crt.wgsl
    fn index(self) -> u32 {
        match self {
            Kind::Monitor => 0,
            Kind::Lcd => 1,
            Kind::Tv => 2,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Blend {
    Add,
    Screen,
    Soft,
    Light,
    Hdr,
}

impl Blend {
    /// the three the site offers; add and soft exist in its shader but
    /// not in its menu
    pub const OFFERED: [Blend; 3] = [Blend::Screen, Blend::Light, Blend::Hdr];

    pub fn label(self) -> &'static str {
        match self {
            Blend::Add => "add",
            Blend::Screen => "screen",
            Blend::Soft => "soft",
            Blend::Light => "light",
            Blend::Hdr => "hdr",
        }
    }

    fn index(self) -> u32 {
        match self {
            Blend::Add => 0,
            Blend::Screen => 1,
            Blend::Soft => 2,
            Blend::Light => 3,
            Blend::Hdr => 4,
        }
    }
}

/// mirrors `Params` in crt.wgsl: five rows of 16 bytes, the vec2s on
/// 8-byte boundaries
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub kind: u32,
    pub distortion: f32,
    pub dot_scale: f32,
    pub dot_pitch: f32,
    pub falloff: f32,
    pub glow_radius: f32,
    pub glow_intensity: f32,
    pub blend: u32,
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub bloom_radius: f32,
    pub brightness_boost: f32,
    pub red_conv: [f32; 2],
    pub blue_conv: [f32; 2],
    pub conv_strength: f32,
    pub aspect: f32,
    pub _pad0: f32,
    pub _pad1: f32,
}

pub const DISTORTION_RANGE: std::ops::RangeInclusive<f32> = 0.0..=0.08;
pub const DOT_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.01..=2.0;
pub const DOT_PITCH_RANGE: std::ops::RangeInclusive<f32> = 0.0..=30.0;
pub const FALLOFF_RANGE: std::ops::RangeInclusive<f32> = 0.01..=1.0;
pub const GLOW_RADIUS_RANGE: std::ops::RangeInclusive<f32> = 0.0..=0.5;
pub const GLOW_INTENSITY_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;
pub const BLOOM_THRESHOLD_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;
pub const BLOOM_INTENSITY_RANGE: std::ops::RangeInclusive<f32> = 0.0..=5.0;
pub const BLOOM_RADIUS_RANGE: std::ops::RangeInclusive<f32> = 0.0..=10.0;
pub const CONVERGENCE_RANGE: std::ops::RangeInclusive<f32> = -1.0..=1.0;

/// the site's hidden constants
const BRIGHTNESS_BOOST: f32 = 2.5;
const CONVERGENCE_STRENGTH: f32 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub kind: Kind,
    pub distortion: f32,
    pub dot_scale: f32,
    pub dot_pitch: f32,
    pub falloff: f32,
    pub glow_radius: f32,
    pub glow_intensity: f32,
    pub blend: Blend,
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    pub bloom_radius: f32,
    pub red_conv: [f32; 2],
    pub blue_conv: [f32; 2],
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            kind: Kind::Monitor,
            distortion: 0.02,
            dot_scale: 0.93,
            dot_pitch: 1.59,
            falloff: 0.12,
            glow_radius: 0.2,
            glow_intensity: 0.1,
            blend: Blend::Screen,
            bloom_threshold: 0.36,
            bloom_intensity: 0.45,
            bloom_radius: 1.0,
            red_conv: [0.01, 0.01],
            blue_conv: [-0.01, -0.01],
        }
    }
}

pub struct Crt {
    enabled: bool,
    settings: Settings,
    bright: wgpu::RenderPipeline,
    main: wgpu::RenderPipeline,
    blur_h: wgpu::RenderPipeline,
    blur_v: wgpu::RenderPipeline,
    combine: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
    bright_input: InputGroupCache,
    scratch_input: InputGroupCache,
    main_input: InputGroupCache,
    /// the bright pass, then after the vertical blur the bloom
    bright_target: OffscreenTarget,
    main_target: OffscreenTarget,
    scratch: OffscreenTarget,
}

impl Crt {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Crt {
        let module = build_shader(device, "crt", include_str!("crt.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "crt params");
        // every entry binds the bloom slot too, so one layout serves all
        // five pipelines; the single-input draws bind the input twice
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("crt"),
            bind_group_layouts: &[
                Some(input_layout),
                Some(input_layout),
                Some(&uniform.layout),
            ],
            immediate_size: 0,
        });
        let pipeline =
            |label, entry| create_fullscreen_pipeline(device, label, &module, entry, &layout);
        Crt {
            enabled: true,
            settings: Settings::default(),
            bright: pipeline("crt bright", "fs_bright"),
            main: pipeline("crt main", "fs_main"),
            blur_h: pipeline("crt blur h", "fs_blur_h"),
            blur_v: pipeline("crt blur v", "fs_blur_v"),
            combine: pipeline("crt combine", "fs_combine"),
            uniform,
            input: InputGroupCache::new(),
            bright_input: InputGroupCache::new(),
            scratch_input: InputGroupCache::new(),
            main_input: InputGroupCache::new(),
            bright_target: OffscreenTarget::new(device, 1, 1, "crt bright"),
            main_target: OffscreenTarget::new(device, 1, 1, "crt main"),
            scratch: OffscreenTarget::new(device, 1, 1, "crt scratch"),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// every value clamped to the site's ranges
    pub fn set_settings(&mut self, s: Settings) {
        let c = |v: f32, r: &std::ops::RangeInclusive<f32>| v.clamp(*r.start(), *r.end());
        self.settings = Settings {
            kind: s.kind,
            distortion: c(s.distortion, &DISTORTION_RANGE),
            dot_scale: c(s.dot_scale, &DOT_SCALE_RANGE),
            dot_pitch: c(s.dot_pitch, &DOT_PITCH_RANGE),
            falloff: c(s.falloff, &FALLOFF_RANGE),
            glow_radius: c(s.glow_radius, &GLOW_RADIUS_RANGE),
            glow_intensity: c(s.glow_intensity, &GLOW_INTENSITY_RANGE),
            blend: s.blend,
            bloom_threshold: c(s.bloom_threshold, &BLOOM_THRESHOLD_RANGE),
            bloom_intensity: c(s.bloom_intensity, &BLOOM_INTENSITY_RANGE),
            bloom_radius: c(s.bloom_radius, &BLOOM_RADIUS_RANGE),
            red_conv: s.red_conv.map(|v| c(v, &CONVERGENCE_RANGE)),
            blue_conv: s.blue_conv.map(|v| c(v, &CONVERGENCE_RANGE)),
        };
    }

    pub fn reset(&mut self) {
        self.settings = Settings::default();
    }

    fn uniform(&self, width: u32, height: u32) -> Uniform {
        let s = &self.settings;
        Uniform {
            kind: s.kind.index(),
            distortion: s.distortion,
            dot_scale: s.dot_scale,
            dot_pitch: s.dot_pitch,
            falloff: s.falloff,
            glow_radius: s.glow_radius,
            glow_intensity: s.glow_intensity,
            blend: s.blend.index(),
            bloom_threshold: s.bloom_threshold,
            bloom_intensity: s.bloom_intensity,
            bloom_radius: s.bloom_radius,
            brightness_boost: BRIGHTNESS_BOOST,
            red_conv: s.red_conv,
            blue_conv: s.blue_conv,
            conv_strength: CONVERGENCE_STRENGTH,
            aspect: width as f32 / height.max(1) as f32,
            _pad0: 0.0,
            _pad1: 0.0,
        }
    }
}

impl Pass for Crt {
    fn name(&self) -> &'static str {
        "crt"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.settings;
        ui.horizontal(|ui| {
            ui.label("type");
            egui::ComboBox::from_id_salt("crt type")
                .selected_text(s.kind.label())
                .show_ui(ui, |ui| {
                    for kind in Kind::ALL {
                        ui.selectable_value(&mut s.kind, kind, kind.label());
                    }
                });
        });
        ui.add(egui::Slider::new(&mut s.distortion, DISTORTION_RANGE).text("distortion"));
        ui.add(egui::Slider::new(&mut s.dot_scale, DOT_SCALE_RANGE).text("dot scale"));
        ui.add(
            egui::Slider::new(&mut s.dot_pitch, DOT_PITCH_RANGE)
                .suffix(" px")
                .text("dot pitch"),
        );
        ui.add(egui::Slider::new(&mut s.falloff, FALLOFF_RANGE).text("falloff"));
        ui.add(egui::Slider::new(&mut s.glow_radius, GLOW_RADIUS_RANGE).text("glow radius"));
        ui.add(
            egui::Slider::new(&mut s.glow_intensity, GLOW_INTENSITY_RANGE).text("glow intensity"),
        );
        ui.horizontal(|ui| {
            ui.label("bloom blend");
            egui::ComboBox::from_id_salt("crt blend")
                .selected_text(s.blend.label())
                .show_ui(ui, |ui| {
                    for blend in Blend::OFFERED {
                        ui.selectable_value(&mut s.blend, blend, blend.label());
                    }
                });
        });
        ui.add(
            egui::Slider::new(&mut s.bloom_threshold, BLOOM_THRESHOLD_RANGE)
                .text("bloom threshold"),
        );
        ui.add(
            egui::Slider::new(&mut s.bloom_intensity, BLOOM_INTENSITY_RANGE)
                .text("bloom intensity"),
        );
        ui.add(
            egui::Slider::new(&mut s.bloom_radius, BLOOM_RADIUS_RANGE)
                .suffix(" px")
                .text("bloom radius"),
        );
        for (label, conv) in [
            ("red convergence", &mut s.red_conv),
            ("blue convergence", &mut s.blue_conv),
        ] {
            ui.horizontal(|ui| {
                ui.label(label);
                for v in conv.iter_mut() {
                    ui.add(
                        egui::DragValue::new(v)
                            .range(CONVERGENCE_RANGE)
                            .speed(0.001)
                            .fixed_decimals(3),
                    );
                }
            });
        }
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
        let (w, h) = (ctx.width, ctx.height);
        self.bright_target.ensure(ctx.device, w, h, "crt bright");
        self.main_target.ensure(ctx.device, w, h, "crt main");
        self.scratch.ensure(ctx.device, w, h, "crt scratch");
        self.uniform.write(ctx.queue, &self.uniform(w, h));

        let input_group = self.input.get(ctx, input);
        let groups = [input_group, input_group, &self.uniform.group];
        draw_fullscreen(
            encoder,
            "crt bright",
            &self.bright,
            &groups,
            &self.bright_target.view,
        );
        draw_fullscreen(
            encoder,
            "crt main",
            &self.main,
            &groups,
            &self.main_target.view,
        );

        let bright_group = self.bright_input.get(ctx, &self.bright_target.view);
        draw_fullscreen(
            encoder,
            "crt blur h",
            &self.blur_h,
            &[bright_group, bright_group, &self.uniform.group],
            &self.scratch.view,
        );
        let scratch_group = self.scratch_input.get(ctx, &self.scratch.view);
        draw_fullscreen(
            encoder,
            "crt blur v",
            &self.blur_v,
            &[scratch_group, scratch_group, &self.uniform.group],
            &self.bright_target.view,
        );
        let main_group = self.main_input.get(ctx, &self.main_target.view);
        let bloom_group = self.bright_input.get(ctx, &self.bright_target.view);
        draw_fullscreen(
            encoder,
            "crt combine",
            &self.combine,
            &[main_group, bloom_group, &self.uniform.group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_80_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 80);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 80);
    }

    #[test]
    fn uniform_field_offsets_match_the_shader() {
        assert_eq!(std::mem::offset_of!(Uniform, kind), 0);
        assert_eq!(std::mem::offset_of!(Uniform, blend), 28);
        assert_eq!(std::mem::offset_of!(Uniform, bloom_threshold), 32);
        assert_eq!(std::mem::offset_of!(Uniform, brightness_boost), 44);
        assert_eq!(std::mem::offset_of!(Uniform, red_conv), 48);
        assert_eq!(std::mem::offset_of!(Uniform, blue_conv), 56);
        assert_eq!(std::mem::offset_of!(Uniform, conv_strength), 64);
        assert_eq!(std::mem::offset_of!(Uniform, aspect), 68);
    }

    #[test]
    fn defaults_follow_the_site() {
        let s = Settings::default();
        assert_eq!(s.kind, Kind::Monitor);
        assert_eq!(s.blend, Blend::Screen);
        assert_eq!(s.dot_pitch, 1.59);
        assert_eq!(s.red_conv, [0.01, 0.01]);
        assert_eq!(Kind::Monitor.index(), 0);
        assert_eq!(Kind::Lcd.index(), 1);
        assert_eq!(Kind::Tv.index(), 2);
        assert_eq!(Blend::Hdr.index(), 4);
    }

    /// the white half keeps lit subpixels through the mask and bloom,
    /// so it comes out brighter than the black half, which the glow
    /// barely reaches
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_white_half_stays_brighter_than_black() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        renderer.push_pass(Box::new(Crt::new(&device, renderer.input_layout())));
        renderer.upload(&device, &queue, &half_frame(64, 32));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        let (mut left, mut right) = (0u64, 0u64);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            let sum = u64::from(px[0]) + u64::from(px[1]) + u64::from(px[2]);
            if i % 64 < 32 {
                left += sum;
            } else {
                right += sum;
            }
        }
        assert!(right > 0, "the output is all black");
        assert!(right > left * 4, "left {left} right {right}");
    }
}

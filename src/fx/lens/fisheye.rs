use bytemuck::{Pod, Zeroable};

use crate::render::{
    create_fullscreen_pipeline, create_input_bind_group, draw_fullscreen, fullscreen_vertex_source,
    Pass, PassCtx,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Projection {
    /// k1 polynomial
    Barrel,
    Equidistant,
    Equisolid,
    Stereographic,
}

impl Projection {
    pub const ALL: [Projection; 4] = [
        Projection::Barrel,
        Projection::Equidistant,
        Projection::Equisolid,
        Projection::Stereographic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Projection::Barrel => "barrel",
            Projection::Equidistant => "equidistant",
            Projection::Equisolid => "equisolid",
            Projection::Stereographic => "stereographic",
        }
    }

    /// matches the constants in fisheye.wgsl
    fn index(self) -> u32 {
        match self {
            Projection::Barrel => 0,
            Projection::Equidistant => 1,
            Projection::Equisolid => 2,
            Projection::Stereographic => 3,
        }
    }
}

/// mirrors `Params` in fisheye.wgsl; the vec2 members keep it 8-byte
/// aligned there, the repr keeps the buffer 16-byte aligned here
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub strength: f32,
    pub zoom: f32,
    pub centre: [f32; 2],
    pub projection: u32,
    pub aspect: f32,
    pub pad: [f32; 2],
}

const DEFAULT_PROJECTION: Projection = Projection::Equisolid;
const DEFAULT_STRENGTH: f32 = 0.35;

/// texels that fall outside the source render black
pub struct Fisheye {
    enabled: bool,
    projection: Projection,
    /// 0 is the identity
    strength: f32,
    /// 0.5 ..= 2.0
    zoom: f32,
    centre: [f32; 2],
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    uniform_group: wgpu::BindGroup,
    /// input bind group reused while the input view stays the same
    input_group: Option<(wgpu::TextureView, wgpu::BindGroup)>,
}

impl Fisheye {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Fisheye {
        let source = format!(
            "{}\n{}",
            fullscreen_vertex_source(),
            include_str!("fisheye.wgsl")
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fisheye"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fisheye params"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<Uniform>() as u64),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fisheye"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform_layout)],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "fisheye", &module, "fs_main", &layout);
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fisheye params"),
            size: std::mem::size_of::<Uniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fisheye params"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        Fisheye {
            enabled: true,
            projection: DEFAULT_PROJECTION,
            strength: DEFAULT_STRENGTH,
            zoom: 1.0,
            centre: [0.0, 0.0],
            pipeline,
            uniform,
            uniform_group,
            input_group: None,
        }
    }

    pub fn projection(&self) -> Projection {
        self.projection
    }

    pub fn set_projection(&mut self, projection: Projection) {
        self.projection = projection;
    }

    pub fn strength(&self) -> f32 {
        self.strength
    }

    pub fn set_strength(&mut self, strength: f32) {
        self.strength = strength.clamp(0.0, 1.0);
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(0.5, 2.0);
    }

    pub fn centre(&self) -> [f32; 2] {
        self.centre
    }

    pub fn set_centre(&mut self, centre: [f32; 2]) {
        self.centre = centre.map(|c| c.clamp(-0.5, 0.5));
    }

    /// back to the launch parameters; the enabled flag is left alone
    pub fn reset(&mut self) {
        self.projection = DEFAULT_PROJECTION;
        self.strength = DEFAULT_STRENGTH;
        self.zoom = 1.0;
        self.centre = [0.0, 0.0];
    }

    fn uniform(&self, width: u32, height: u32) -> Uniform {
        Uniform {
            strength: self.strength,
            zoom: self.zoom,
            centre: self.centre,
            projection: self.projection.index(),
            aspect: width as f32 / height.max(1) as f32,
            pad: [0.0; 2],
        }
    }
}

impl Pass for Fisheye {
    fn name(&self) -> &'static str {
        "fisheye"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::ComboBox::from_id_salt("fisheye projection")
            .selected_text(self.projection.label())
            .show_ui(ui, |ui| {
                for projection in Projection::ALL {
                    ui.selectable_value(&mut self.projection, projection, projection.label());
                }
            });
        ui.add(egui::Slider::new(&mut self.strength, 0.0..=1.0).text("strength"));
        ui.add(egui::Slider::new(&mut self.zoom, 0.5..=2.0).text("zoom"));
        ui.horizontal(|ui| {
            ui.label("centre");
            ui.add(
                egui::DragValue::new(&mut self.centre[0])
                    .range(-0.5..=0.5)
                    .speed(0.002),
            );
            ui.add(
                egui::DragValue::new(&mut self.centre[1])
                    .range(-0.5..=0.5)
                    .speed(0.002),
            );
        });
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
        ctx.queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&self.uniform(ctx.width, ctx.height)),
        );
        let stale = self
            .input_group
            .as_ref()
            .is_none_or(|(view, _)| view != input);
        if stale {
            let group = create_input_bind_group(ctx.device, ctx.input_layout, input, ctx.sampler);
            self.input_group = Some((input.clone(), group));
        }
        let (_, input_group) = self.input_group.as_ref().expect("set above");
        draw_fullscreen(
            encoder,
            "fisheye",
            &self.pipeline,
            &[input_group, &self.uniform_group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 6 f32 plus 2 words of padding is 32 bytes; a uniform binding
    /// wants the struct a multiple of 16
    #[test]
    fn uniform_is_32_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 32);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        let zero = Uniform::zeroed();
        assert_eq!(bytemuck::bytes_of(&zero).len(), 32);
    }

    #[test]
    fn uniform_field_offsets_match_the_shader() {
        assert_eq!(std::mem::offset_of!(Uniform, strength), 0);
        assert_eq!(std::mem::offset_of!(Uniform, zoom), 4);
        assert_eq!(std::mem::offset_of!(Uniform, centre), 8);
        assert_eq!(std::mem::offset_of!(Uniform, projection), 16);
        assert_eq!(std::mem::offset_of!(Uniform, aspect), 20);
        assert_eq!(std::mem::offset_of!(Uniform, pad), 24);
    }

    /// strength 0 is the identity, so a grey frame stays grey; the
    /// test mostly proves that fisheye.wgsl compiles for the driver
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_identity_keeps_grey() {
        use crate::capture::{Frame, PixelLayout};
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let mut fisheye = Fisheye::new(&device, renderer.input_layout());
        fisheye.set_strength(0.0);
        renderer.passes_mut().push(Box::new(fisheye));
        let frame = Frame {
            width: 4,
            height: 2,
            layout: PixelLayout::Yuyv,
            data: vec![128; 4 * 2 * 2],
            seq: 1,
            received: std::time::Instant::now(),
        };
        renderer.upload(&device, &queue, &frame);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(&device, &queue, &mut encoder);
        let texture = renderer.output_texture().expect("output after upload");
        let pixels = read_back(&device, &queue, encoder, texture, 4, 2);
        for px in pixels.chunks_exact(4) {
            for c in &px[..3] {
                assert!((i32::from(*c) - 130).abs() <= 2, "pixel {px:?} is not grey");
            }
            assert_eq!(px[3], 255);
        }
    }
}

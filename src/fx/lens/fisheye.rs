use bytemuck::{Pod, Zeroable};

use crate::render::{
    create_fullscreen_pipeline, create_input_bind_group, draw_fullscreen, fullscreen_vertex_source,
    Pass, PassCtx,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Projection {
    Rectilinear,
    Equidistant,
    Equisolid,
    Stereographic,
    Orthographic,
}

impl Projection {
    pub const ALL: [Projection; 5] = [
        Projection::Rectilinear,
        Projection::Equidistant,
        Projection::Equisolid,
        Projection::Stereographic,
        Projection::Orthographic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Projection::Rectilinear => "rectilinear",
            Projection::Equidistant => "equidistant",
            Projection::Equisolid => "equisolid",
            Projection::Stereographic => "stereographic",
            Projection::Orthographic => "orthographic",
        }
    }

    /// matches the constants in fisheye.wgsl
    fn index(self) -> u32 {
        match self {
            Projection::Rectilinear => 0,
            Projection::Equidistant => 1,
            Projection::Equisolid => 2,
            Projection::Stereographic => 3,
            Projection::Orthographic => 4,
        }
    }

    /// the normalised output radius at which this lens, with half
    /// field of view `h`, shows a ray `theta` off axis; mirrors
    /// ray_angle in fisheye.wgsl including its clamps on h
    fn radius_for(self, h: f32, theta: f32) -> f32 {
        const NEAR_RIGHT: f32 = 1.5533;
        match self {
            Projection::Rectilinear => theta.tan() / h.min(NEAR_RIGHT).tan(),
            Projection::Equidistant => theta / h,
            Projection::Equisolid => (theta * 0.5).sin() / (h.min(PI - 1e-3) * 0.5).sin(),
            Projection::Stereographic => (theta * 0.5).tan() / (h * 0.5).min(NEAR_RIGHT).tan(),
            Projection::Orthographic => theta.sin() / h.min(PI * 0.5).sin(),
        }
    }
}

/// what a lookup beyond the source frame shows
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Black,
    Clamp,
    Mirror,
}

impl Edge {
    pub const ALL: [Edge; 3] = [Edge::Black, Edge::Clamp, Edge::Mirror];

    pub fn label(self) -> &'static str {
        match self {
            Edge::Black => "black",
            Edge::Clamp => "clamp",
            Edge::Mirror => "mirror",
        }
    }

    fn index(self) -> u32 {
        match self {
            Edge::Black => 0,
            Edge::Clamp => 1,
            Edge::Mirror => 2,
        }
    }
}

/// mirrors `Params` in fisheye.wgsl: three rows of 16 bytes, the vec2
/// on an 8-byte boundary, angles in radians
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub lens_fov: f32,
    pub source_fov: f32,
    pub zoom: f32,
    pub k1: f32,
    pub centre: [f32; 2],
    pub projection: u32,
    pub edge: u32,
    pub aspect: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
}

const PI: f32 = std::f32::consts::PI;
const DEFAULT_PROJECTION: Projection = Projection::Equisolid;
const DEFAULT_LENS_FOV: f32 = 180.0;
/// the C920's horizontal angle of view
const DEFAULT_SOURCE_FOV: f32 = 70.0;
pub const LENS_FOV_RANGE: std::ops::RangeInclusive<f32> = 1.0..=360.0;
pub const SOURCE_FOV_RANGE: std::ops::RangeInclusive<f32> = 10.0..=170.0;
pub const ZOOM_RANGE: std::ops::RangeInclusive<f32> = 0.1..=4.0;
pub const ANGLE_RANGE: std::ops::RangeInclusive<f32> = -180.0..=180.0;

/// an angle-driven lens: the output sees `lens_fov` degrees through the
/// chosen projection, the source is a rectilinear camera covering
/// `source_fov` degrees across its width, and yaw, pitch and roll turn
/// a virtual camera between the two
pub struct Fisheye {
    enabled: bool,
    projection: Projection,
    /// degrees across the output's width
    lens_fov: f32,
    /// degrees across the source's width
    source_fov: f32,
    /// scales the output so the source's horizontal edge meets the
    /// output's; the zoom slider is ignored while this is on
    fit: bool,
    zoom: f32,
    /// extra polynomial barrel (positive) or pincushion (negative)
    k1: f32,
    /// degrees
    yaw: f32,
    pitch: f32,
    roll: f32,
    edge: Edge,
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
        let mut fisheye = Fisheye {
            enabled: true,
            projection: DEFAULT_PROJECTION,
            lens_fov: DEFAULT_LENS_FOV,
            source_fov: DEFAULT_SOURCE_FOV,
            fit: true,
            zoom: 1.0,
            k1: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            edge: Edge::Black,
            centre: [0.0, 0.0],
            pipeline,
            uniform,
            uniform_group,
            input_group: None,
        };
        fisheye.reset();
        fisheye
    }

    pub fn projection(&self) -> Projection {
        self.projection
    }

    pub fn set_projection(&mut self, projection: Projection) {
        self.projection = projection;
    }

    pub fn lens_fov(&self) -> f32 {
        self.lens_fov
    }

    pub fn set_lens_fov(&mut self, degrees: f32) {
        self.lens_fov = degrees.clamp(*LENS_FOV_RANGE.start(), *LENS_FOV_RANGE.end());
    }

    pub fn source_fov(&self) -> f32 {
        self.source_fov
    }

    pub fn set_source_fov(&mut self, degrees: f32) {
        self.source_fov = degrees.clamp(*SOURCE_FOV_RANGE.start(), *SOURCE_FOV_RANGE.end());
    }

    pub fn fit(&self) -> bool {
        self.fit
    }

    pub fn set_fit(&mut self, fit: bool) {
        self.fit = fit;
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    pub fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
    }

    pub fn k1(&self) -> f32 {
        self.k1
    }

    pub fn set_k1(&mut self, k1: f32) {
        self.k1 = k1.clamp(-1.0, 1.0);
    }

    /// yaw, pitch and roll in degrees
    pub fn rotation(&self) -> [f32; 3] {
        [self.yaw, self.pitch, self.roll]
    }

    pub fn set_rotation(&mut self, degrees: [f32; 3]) {
        let [yaw, pitch, roll] = degrees.map(|a| a.clamp(*ANGLE_RANGE.start(), *ANGLE_RANGE.end()));
        self.yaw = yaw;
        self.pitch = pitch;
        self.roll = roll;
    }

    pub fn edge(&self) -> Edge {
        self.edge
    }

    pub fn set_edge(&mut self, edge: Edge) {
        self.edge = edge;
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
        self.lens_fov = DEFAULT_LENS_FOV;
        self.source_fov = DEFAULT_SOURCE_FOV;
        self.fit = true;
        self.zoom = 1.0;
        self.k1 = 0.0;
        self.yaw = 0.0;
        self.pitch = 0.0;
        self.roll = 0.0;
        self.edge = Edge::Black;
        self.centre = [0.0, 0.0];
    }

    /// rectilinear at the source's own angle with no rotation, which
    /// maps every pixel onto itself
    pub fn set_identity(&mut self) {
        self.reset();
        self.projection = Projection::Rectilinear;
        self.lens_fov = self.source_fov;
        self.fit = false;
    }

    /// the zoom that puts the source's horizontal edge on the output's
    /// horizontal edge, solving rn (1 + k1 rn^2) = radius_for(edge)
    /// for rn with a few newton steps
    fn fit_zoom(&self) -> f32 {
        let h = self.lens_fov.to_radians() * 0.5;
        let edge = self
            .projection
            .radius_for(h, self.source_fov.to_radians() * 0.5);
        if !edge.is_finite() || edge <= 0.0 {
            return 1.0;
        }
        let mut rn = edge;
        for _ in 0..4 {
            let f = rn * (1.0 + self.k1 * rn * rn) - edge;
            let df = 1.0 + 3.0 * self.k1 * rn * rn;
            if df.abs() < 1e-6 {
                break;
            }
            rn -= f / df;
        }
        if !rn.is_finite() || rn <= 0.0 {
            return 1.0;
        }
        (1.0 / rn).clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end())
    }

    pub fn effective_zoom(&self) -> f32 {
        if self.fit {
            self.fit_zoom()
        } else {
            self.zoom
        }
    }

    fn uniform(&self, width: u32, height: u32) -> Uniform {
        Uniform {
            lens_fov: self.lens_fov.to_radians(),
            source_fov: self.source_fov.to_radians(),
            zoom: self.effective_zoom(),
            k1: self.k1,
            centre: self.centre,
            projection: self.projection.index(),
            edge: self.edge.index(),
            aspect: width as f32 / height.max(1) as f32,
            yaw: self.yaw.to_radians(),
            pitch: self.pitch.to_radians(),
            roll: self.roll.to_radians(),
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
        ui.add(
            egui::Slider::new(&mut self.lens_fov, LENS_FOV_RANGE)
                .suffix("°")
                .text("lens angle"),
        );
        ui.add(
            egui::Slider::new(&mut self.source_fov, SOURCE_FOV_RANGE)
                .suffix("°")
                .text("camera angle"),
        );
        ui.checkbox(&mut self.fit, "fit source to width");
        ui.add_enabled(
            !self.fit,
            egui::Slider::new(&mut self.zoom, ZOOM_RANGE)
                .logarithmic(true)
                .text("zoom"),
        );
        ui.add(egui::Slider::new(&mut self.k1, -1.0..=1.0).text("barrel k1"));
        ui.add(
            egui::Slider::new(&mut self.yaw, ANGLE_RANGE)
                .suffix("°")
                .text("yaw"),
        );
        ui.add(
            egui::Slider::new(&mut self.pitch, ANGLE_RANGE)
                .suffix("°")
                .text("pitch"),
        );
        ui.add(
            egui::Slider::new(&mut self.roll, ANGLE_RANGE)
                .suffix("°")
                .text("roll"),
        );
        ui.horizontal(|ui| {
            ui.label("edge");
            egui::ComboBox::from_id_salt("fisheye edge")
                .selected_text(self.edge.label())
                .show_ui(ui, |ui| {
                    for edge in Edge::ALL {
                        ui.selectable_value(&mut self.edge, edge, edge.label());
                    }
                });
        });
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

    /// eleven f32 words round up to three rows of 16 bytes; a uniform
    /// binding wants the struct a multiple of 16
    #[test]
    fn uniform_is_48_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 48);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        let zero = Uniform::zeroed();
        assert_eq!(bytemuck::bytes_of(&zero).len(), 48);
    }

    #[test]
    fn uniform_field_offsets_match_the_shader() {
        assert_eq!(std::mem::offset_of!(Uniform, lens_fov), 0);
        assert_eq!(std::mem::offset_of!(Uniform, source_fov), 4);
        assert_eq!(std::mem::offset_of!(Uniform, zoom), 8);
        assert_eq!(std::mem::offset_of!(Uniform, k1), 12);
        assert_eq!(std::mem::offset_of!(Uniform, centre), 16);
        assert_eq!(std::mem::offset_of!(Uniform, projection), 24);
        assert_eq!(std::mem::offset_of!(Uniform, edge), 28);
        assert_eq!(std::mem::offset_of!(Uniform, aspect), 32);
        assert_eq!(std::mem::offset_of!(Uniform, yaw), 36);
        assert_eq!(std::mem::offset_of!(Uniform, pitch), 40);
        assert_eq!(std::mem::offset_of!(Uniform, roll), 44);
    }

    /// every projection puts the lens edge at radius 1, so a lens that
    /// sees exactly the source needs no zoom to fit
    #[test]
    fn lens_edge_sits_at_radius_one() {
        for projection in Projection::ALL {
            let h = 35f32.to_radians();
            let rn = projection.radius_for(h, h);
            assert!((rn - 1.0).abs() < 1e-5, "{projection:?} edge at {rn}");
        }
    }

    /// a 180 degree equisolid lens shows a 70 degree source inside
    /// radius sin(17.5)/sin(45) = 0.425, so fitting zooms by 2.35
    #[test]
    fn fit_zoom_fills_the_width_with_the_source() {
        let h = 90f32.to_radians();
        let rn = Projection::Equisolid.radius_for(h, 35f32.to_radians());
        assert!((rn - 0.4252).abs() < 1e-3, "edge radius {rn}");
        assert!((1.0 / rn - 2.352).abs() < 1e-2);
    }

    /// the identity lens leaves a grey frame grey; the test mostly
    /// proves that fisheye.wgsl compiles for the driver
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_identity_keeps_grey() {
        use crate::capture::{Frame, PixelLayout};
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let mut fisheye = Fisheye::new(&device, renderer.input_layout());
        fisheye.set_identity();
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

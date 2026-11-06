//! displace: the frame as a 3d point cloud, every step px a dot pushed
//! along z by its luminance, seen through the site's perspective camera
//! with yaw and pitch sliders in place of the mouse orbit

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, UniformBuffer};
use crate::render::{Pass, PassCtx, TARGET_FORMAT};

/// mirrors `Params` in displace.wgsl: the matrix, then two rows
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    /// column major
    pub view_proj: [[f32; 4]; 4],
    pub size: [f32; 2],
    pub step: f32,
    pub displacement: f32,
    pub dot_size: f32,
    pub columns: u32,
    pub _pad0: u32,
    pub _pad1: u32,
}

pub const STEP_RANGE: std::ops::RangeInclusive<u32> = 4..=20;
pub const DISPLACEMENT_RANGE: std::ops::RangeInclusive<f32> = -500.0..=500.0;
pub const DOT_SIZE_RANGE: std::ops::RangeInclusive<u32> = 4..=100;
pub const YAW_RANGE: std::ops::RangeInclusive<f32> = -180.0..=180.0;
pub const PITCH_RANGE: std::ops::RangeInclusive<f32> = -89.0..=89.0;

const DEFAULT_STEP: u32 = 8;
const DEFAULT_DISPLACEMENT: f32 = -140.0;
const DEFAULT_DOT_SIZE: u32 = 11;
/// the site's camera sits at (400, 600, 1200) in p5's y-down world,
/// 1400 from the origin; in a y-up world that is yaw atan(400 / 1200)
/// and pitch -asin(600 / 1400)
const DEFAULT_YAW: f32 = 18.435;
const DEFAULT_PITCH: f32 = -25.377;
const CAMERA_DISTANCE: f32 = 1400.0;
const FOVY: f32 = 0.5;
const NEAR: f32 = 10.0;
/// the site says 1000, which is nearer than its own camera; the cloud
/// would be clipped away, so the far plane is pushed out
const FAR: f32 = 10000.0;

/// depth attachment at the frame size
struct Depth {
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl Depth {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Depth {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("displace depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        Depth {
            view: texture.create_view(&Default::default()),
            width: width.max(1),
            height: height.max(1),
        }
    }
}

pub struct Displace {
    enabled: bool,
    step: u32,
    displacement: f32,
    dot_size: u32,
    /// degrees
    yaw: f32,
    pitch: f32,
    pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
    depth: Option<Depth>,
}

impl Displace {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Displace {
        let module = build_shader(device, "displace", include_str!("displace.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "displace params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("displace"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("displace"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_points"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_points"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Displace {
            enabled: true,
            step: DEFAULT_STEP,
            displacement: DEFAULT_DISPLACEMENT,
            dot_size: DEFAULT_DOT_SIZE,
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            pipeline,
            uniform,
            input: InputGroupCache::new(),
            depth: None,
        }
    }

    pub fn step(&self) -> u32 {
        self.step
    }

    pub fn set_step(&mut self, step: u32) {
        self.step = step.clamp(*STEP_RANGE.start(), *STEP_RANGE.end());
    }

    pub fn displacement(&self) -> f32 {
        self.displacement
    }

    pub fn set_displacement(&mut self, d: f32) {
        self.displacement = d.clamp(*DISPLACEMENT_RANGE.start(), *DISPLACEMENT_RANGE.end());
    }

    pub fn dot_size(&self) -> u32 {
        self.dot_size
    }

    pub fn set_dot_size(&mut self, size: u32) {
        self.dot_size = size.clamp(*DOT_SIZE_RANGE.start(), *DOT_SIZE_RANGE.end());
    }

    /// yaw and pitch of the camera about the origin, degrees
    pub fn orbit(&self) -> (f32, f32) {
        (self.yaw, self.pitch)
    }

    pub fn set_orbit(&mut self, yaw: f32, pitch: f32) {
        self.yaw = yaw.clamp(*YAW_RANGE.start(), *YAW_RANGE.end());
        self.pitch = pitch.clamp(*PITCH_RANGE.start(), *PITCH_RANGE.end());
    }

    pub fn reset(&mut self) {
        self.step = DEFAULT_STEP;
        self.displacement = DEFAULT_DISPLACEMENT;
        self.dot_size = DEFAULT_DOT_SIZE;
        self.yaw = DEFAULT_YAW;
        self.pitch = DEFAULT_PITCH;
    }

    /// perspective(FOVY, aspect, NEAR, FAR) times a look-at from the
    /// orbit position to the origin with y up, column major, depth 0..1
    pub fn view_proj(&self, width: u32, height: u32) -> [[f32; 4]; 4] {
        let aspect = width as f32 / height.max(1) as f32;
        let (sy, cy) = self.yaw.to_radians().sin_cos();
        let (sp, cp) = self.pitch.to_radians().sin_cos();
        let eye = [
            CAMERA_DISTANCE * cp * sy,
            CAMERA_DISTANCE * sp,
            CAMERA_DISTANCE * cp * cy,
        ];
        let f = normalize([-eye[0], -eye[1], -eye[2]]);
        let s = normalize(cross(f, [0.0, 1.0, 0.0]));
        let u = cross(s, f);
        // row-major view then projection, multiplied by hand
        let view = [
            [s[0], s[1], s[2], -dot(s, eye)],
            [u[0], u[1], u[2], -dot(u, eye)],
            [-f[0], -f[1], -f[2], dot(f, eye)],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let t = 1.0 / (FOVY * 0.5).tan();
        let proj = [
            [t / aspect, 0.0, 0.0, 0.0],
            [0.0, t, 0.0, 0.0],
            [0.0, 0.0, FAR / (NEAR - FAR), NEAR * FAR / (NEAR - FAR)],
            [0.0, 0.0, -1.0, 0.0],
        ];
        let mut m = [[0.0f32; 4]; 4];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                *cell = (0..4).map(|k| proj[r][k] * view[k][c]).sum();
            }
        }
        // wgsl mat4x4 is column major
        let mut columns = [[0.0f32; 4]; 4];
        for (c, column) in columns.iter_mut().enumerate() {
            for (r, cell) in column.iter_mut().enumerate() {
                *cell = m[r][c];
            }
        }
        columns
    }

    fn grid(&self, width: u32, height: u32) -> (u32, u32) {
        (width.div_ceil(self.step), height.div_ceil(self.step))
    }

    fn uniform(&self, width: u32, height: u32) -> Uniform {
        let (columns, _) = self.grid(width, height);
        Uniform {
            view_proj: self.view_proj(width, height),
            size: [width as f32, height as f32],
            step: self.step as f32,
            displacement: self.displacement,
            dot_size: self.dot_size as f32,
            columns,
            _pad0: 0,
            _pad1: 0,
        }
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt().max(1e-9);
    [v[0] / len, v[1] / len, v[2] / len]
}

impl Pass for Displace {
    fn name(&self) -> &'static str {
        "displace"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::Slider::new(&mut self.step, STEP_RANGE)
                .suffix(" px")
                .text("step"),
        );
        ui.add(egui::Slider::new(&mut self.displacement, DISPLACEMENT_RANGE).text("displacement"));
        ui.add(
            egui::Slider::new(&mut self.dot_size, DOT_SIZE_RANGE)
                .suffix(" px")
                .text("dot size"),
        );
        ui.add(
            egui::Slider::new(&mut self.yaw, YAW_RANGE)
                .suffix("°")
                .text("yaw"),
        );
        ui.add(
            egui::Slider::new(&mut self.pitch, PITCH_RANGE)
                .suffix("°")
                .text("pitch"),
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
        let (w, h) = (ctx.width, ctx.height);
        if self
            .depth
            .as_ref()
            .is_none_or(|d| d.width != w.max(1) || d.height != h.max(1))
        {
            self.depth = Some(Depth::new(ctx.device, w, h));
        }
        let depth = self.depth.as_ref().expect("set above");
        self.uniform.write(ctx.queue, &self.uniform(w, h));
        let (columns, rows) = self.grid(w, h);
        let input_group = self.input.get(ctx, input);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("displace"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth.view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, input_group, &[]);
        pass.set_bind_group(1, &self.uniform.group, &[]);
        pass.draw(0..6, 0..columns * rows);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_96_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 96);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, view_proj), 0);
        assert_eq!(std::mem::offset_of!(Uniform, size), 64);
        assert_eq!(std::mem::offset_of!(Uniform, step), 72);
        assert_eq!(std::mem::offset_of!(Uniform, displacement), 76);
        assert_eq!(std::mem::offset_of!(Uniform, dot_size), 80);
        assert_eq!(std::mem::offset_of!(Uniform, columns), 84);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 96);
    }

    /// the default orbit is the site's camera position
    #[test]
    fn default_orbit_is_1400_from_the_origin_at_the_site_eye() {
        let (sy, cy) = DEFAULT_YAW.to_radians().sin_cos();
        let (sp, cp) = DEFAULT_PITCH.to_radians().sin_cos();
        let eye = [
            CAMERA_DISTANCE * cp * sy,
            CAMERA_DISTANCE * sp,
            CAMERA_DISTANCE * cp * cy,
        ];
        assert!((eye[0] - 400.0).abs() < 0.5, "{eye:?}");
        assert!((eye[1] + 600.0).abs() < 0.5, "{eye:?}");
        assert!((eye[2] - 1200.0).abs() < 0.5, "{eye:?}");
    }

    /// the cloud sits at the centre of a white screen: the black half
    /// draws black dots, the rest stays white
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_draws_black_dots_on_white() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        renderer.push_pass(Box::new(Displace::new(&device, renderer.input_layout())));
        renderer.upload(&device, &queue, &half_frame(64, 32));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        let dark = pixels.chunks_exact(4).filter(|px| px[0] < 64).count();
        let white = pixels.chunks_exact(4).filter(|px| px[0] == 255).count();
        assert!(dark > 0, "no dark dots");
        assert!(white > 64 * 32 / 2, "background not white: {white}");
        assert!(pixels.chunks_exact(4).all(|px| px[3] == 255));
    }
}

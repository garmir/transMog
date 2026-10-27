use crate::capture::{Frame, PixelLayout};

pub struct PassCtx<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub width: u32,
    pub height: u32,
    pub sampler: &'a wgpu::Sampler,
    /// group 0: binding 0 texture_2d<f32>, binding 1 sampler
    pub input_layout: &'a wgpu::BindGroupLayout,
}

pub trait Pass: Send {
    fn name(&self) -> &'static str;
    fn enabled(&self) -> bool;
    fn set_enabled(&mut self, on: bool);
    /// parameter widgets only; the caller draws the enable checkbox
    fn ui(&mut self, ui: &mut egui::Ui);
    fn encode(
        &mut self,
        ctx: &PassCtx,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
    );
}

pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// vertex stage shared by every fullscreen pass; prepend it to a
/// fragment-only wgsl source before creating the module
pub fn fullscreen_vertex_source() -> &'static str {
    include_str!("fullscreen.wgsl")
}

/// the bind group layout every pass reads its input through
pub fn create_input_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("pass input"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

pub fn create_input_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pass input"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// a fullscreen-triangle pipeline that writes one rgba8 target
pub fn create_fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    module: &wgpu::ShaderModule,
    fragment_entry: &str,
    layout: &wgpu::PipelineLayout,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment_entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: TARGET_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// one render pass that draws the fullscreen triangle into `output`
pub fn draw_fullscreen(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    pipeline: &wgpu::RenderPipeline,
    bind_groups: &[&wgpu::BindGroup],
    output: &wgpu::TextureView,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: output,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    for (index, group) in bind_groups.iter().enumerate() {
        pass.set_bind_group(index as u32, *group, &[]);
    }
    pass.draw(0..3, 0..1);
}

/// bt.601 limited-range decode of one pixel, the cpu twin of
/// `ycbcr_to_rgb` in convert.wgsl
pub fn yuyv_to_rgb(y: u8, cb: u8, cr: u8) -> [f32; 3] {
    let yn = (f32::from(y) - 16.0) / 219.0;
    let cbn = (f32::from(cb) - 128.0) / 224.0;
    let crn = (f32::from(cr) - 128.0) / 224.0;
    [
        (yn + 1.402 * crn).clamp(0.0, 1.0),
        (yn - 0.344136 * cbn - 0.714136 * crn).clamp(0.0, 1.0),
        (yn + 1.772 * cbn).clamp(0.0, 1.0),
    ]
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct Source {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    layout: PixelLayout,
    seq: Option<u64>,
}

/// source texture, ping-pong targets and the pass stack
pub struct Renderer {
    sampler: wgpu::Sampler,
    input_layout: wgpu::BindGroupLayout,
    convert_yuyv: wgpu::RenderPipeline,
    convert_copy: wgpu::RenderPipeline,
    source: Option<Source>,
    targets: Option<[Target; 2]>,
    /// which target holds the final image after the last render()
    output: usize,
    passes: Vec<Box<dyn Pass>>,
}

impl Renderer {
    /// all intermediate textures are rgba8unorm with
    /// RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC
    pub fn new(device: &wgpu::Device, _queue: &wgpu::Queue) -> Renderer {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let input_layout = create_input_layout(device);
        let source = format!(
            "{}\n{}",
            fullscreen_vertex_source(),
            include_str!("convert.wgsl")
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("convert"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("convert"),
            bind_group_layouts: &[Some(&input_layout)],
            immediate_size: 0,
        });
        let convert_yuyv =
            create_fullscreen_pipeline(device, "convert yuyv", &module, "fs_yuyv", &layout);
        let convert_copy =
            create_fullscreen_pipeline(device, "convert copy", &module, "fs_copy", &layout);
        Renderer {
            sampler,
            input_layout,
            convert_yuyv,
            convert_copy,
            source: None,
            targets: None,
            output: 0,
            passes: Vec::new(),
        }
    }

    pub fn input_layout(&self) -> &wgpu::BindGroupLayout {
        &self.input_layout
    }

    /// recreates the textures when the size or layout changes
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &Frame) {
        if frame.width == 0 || frame.height == 0 {
            return;
        }
        let same_shape = self.source.as_ref().is_some_and(|s| {
            s.width == frame.width && s.height == frame.height && s.layout == frame.layout
        });
        if same_shape && self.source.as_ref().and_then(|s| s.seq) == Some(frame.seq) {
            return;
        }
        if !same_shape {
            self.recreate(device, frame);
        }
        let source = self.source.as_mut().expect("recreate sets the source");
        let (texel_width, bytes_per_row) = match frame.layout {
            PixelLayout::Yuyv => (frame.width / 2, frame.width * 2),
            PixelLayout::Rgba8 => (frame.width, frame.width * 4),
        };
        let rows = frame.height as usize;
        let row = bytes_per_row as usize;
        if frame.data.len() < row * rows {
            tracing::warn!(
                len = frame.data.len(),
                expected = row * rows,
                "frame is shorter than its size claims, skipping upload"
            );
            return;
        }
        let padded_row = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded;
        let data: &[u8] = if padded_row == bytes_per_row {
            &frame.data[..row * rows]
        } else {
            let mut buf = vec![0u8; padded_row as usize * rows];
            for (dst, src) in buf
                .chunks_exact_mut(padded_row as usize)
                .zip(frame.data.chunks_exact(row))
            {
                dst[..row].copy_from_slice(src);
            }
            padded = buf;
            &padded
        };
        queue.write_texture(
            source.texture.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(frame.height),
            },
            wgpu::Extent3d {
                width: texel_width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
        source.seq = Some(frame.seq);
    }

    fn recreate(&mut self, device: &wgpu::Device, frame: &Frame) {
        let texel_width = match frame.layout {
            PixelLayout::Yuyv => frame.width / 2,
            PixelLayout::Rgba8 => frame.width,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("source"),
            size: wgpu::Extent3d {
                width: texel_width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind_group = create_input_bind_group(device, &self.input_layout, &view, &self.sampler);
        self.source = Some(Source {
            texture,
            bind_group,
            width: frame.width,
            height: frame.height,
            layout: frame.layout,
            seq: None,
        });
        let target = |label| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TARGET_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            Target { texture, view }
        };
        self.targets = Some([target("target a"), target("target b")]);
        self.output = 0;
        tracing::debug!(
            width = frame.width,
            height = frame.height,
            layout = ?frame.layout,
            "render targets recreated"
        );
    }

    /// runs the convert pass and then every enabled pass
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let (Some(source), Some(targets)) = (&self.source, &self.targets) else {
            return;
        };
        let pipeline = match source.layout {
            PixelLayout::Yuyv => &self.convert_yuyv,
            PixelLayout::Rgba8 => &self.convert_copy,
        };
        draw_fullscreen(
            encoder,
            "convert",
            pipeline,
            &[&source.bind_group],
            &targets[0].view,
        );
        let ctx = PassCtx {
            device,
            queue,
            width: source.width,
            height: source.height,
            sampler: &self.sampler,
            input_layout: &self.input_layout,
        };
        let mut current = 0;
        for pass in self.passes.iter_mut().filter(|p| p.enabled()) {
            let next = 1 - current;
            pass.encode(&ctx, encoder, &targets[current].view, &targets[next].view);
            current = next;
        }
        self.output = current;
    }

    /// final texture, none before the first upload
    pub fn output_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t[self.output].view)
    }

    pub fn output_texture(&self) -> Option<&wgpu::Texture> {
        self.targets.as_ref().map(|t| &t[self.output].texture)
    }

    pub fn output_size(&self) -> Option<(u32, u32)> {
        self.source.as_ref().map(|s| (s.width, s.height))
    }

    pub fn passes_mut(&mut self) -> &mut Vec<Box<dyn Pass>> {
        &mut self.passes
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Instant;

    fn close(actual: [f32; 3], expected: [u8; 3]) {
        for (a, e) in actual.iter().zip(expected) {
            let a = (a * 255.0).round() as i32;
            assert!(
                (a - i32::from(e)).abs() <= 1,
                "got {actual:?}, expected {expected:?}"
            );
        }
    }

    /// mid grey: (128 - 16) / 219 = 0.5114, times 255 = 130.4
    #[test]
    fn yuyv_mid_grey_decodes_to_130() {
        close(yuyv_to_rgb(128, 128, 128), [130, 130, 130]);
    }

    /// bt.601 limited-range red is Y 81, Cb 90, Cr 240:
    /// r = 0.2968 + 1.402 * 0.5 = 0.9978 -> 254, g and b clamp to 0
    #[test]
    fn yuyv_red_decodes_to_red() {
        close(yuyv_to_rgb(81, 90, 240), [254, 0, 0]);
    }

    #[test]
    fn yuyv_black_and_white_hit_the_limits() {
        close(yuyv_to_rgb(16, 128, 128), [0, 0, 0]);
        close(yuyv_to_rgb(235, 128, 128), [255, 255, 255]);
    }

    /// wgpu's request futures resolve without an executor on native
    /// targets, so a spin on a no-op waker is enough
    pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        loop {
            if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::yield_now();
        }
    }

    pub(crate) fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = block_on(instance.request_adapter(&Default::default())).ok()?;
        eprintln!("adapter: {:?}", adapter.get_info());
        block_on(adapter.request_device(&Default::default())).ok()
    }

    /// reads the output texture back as tightly packed rgba8 rows
    pub(crate) fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let padded_row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size: u64::from(padded_row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = encoder;
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        rx.recv().expect("map callback").expect("map");
        let view = slice.get_mapped_range().expect("mapped range");
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for row in view.chunks_exact(padded_row as usize) {
            out.extend_from_slice(&row[..(width * 4) as usize]);
        }
        out
    }

    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_converts_a_yuyv_frame_to_grey() {
        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let frame = Frame {
            width: 4,
            height: 2,
            layout: PixelLayout::Yuyv,
            data: vec![128; 4 * 2 * 2],
            seq: 1,
            received: Instant::now(),
        };
        renderer.upload(&device, &queue, &frame);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(&device, &queue, &mut encoder);
        let texture = renderer.output_texture().expect("output after upload");
        let pixels = read_back(&device, &queue, encoder, texture, 4, 2);
        assert_eq!(pixels.len(), 4 * 2 * 4);
        for px in pixels.chunks_exact(4) {
            for c in &px[..3] {
                assert!((i32::from(*c) - 130).abs() <= 2, "pixel {px:?} is not grey");
            }
            assert_eq!(px[3], 255);
        }
    }
}

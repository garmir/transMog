use std::sync::atomic::{AtomicU64, Ordering};

use crate::capture::{Frame, PixelLayout};

pub mod kit;

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
    /// true for a pass that works on the frame in main memory: the
    /// renderer then reads the current target back, calls process_cpu
    /// and uploads the result instead of calling encode
    fn cpu(&self) -> bool {
        false
    }
    /// tightly packed rgba8 rows, top row first, edited in place
    fn process_cpu(&mut self, _rgba: &mut Vec<u8>, _width: u32, _height: u32) {}
}

/// a pass in the stack with an id that survives reordering, so the ui
/// can key its widgets by it
pub struct Slot {
    pub id: u64,
    pub pass: Box<dyn Pass>,
}

impl Slot {
    pub fn new(pass: Box<dyn Pass>) -> Slot {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Slot {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            pass,
        }
    }
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
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT | wgpu::ShaderStages::COMPUTE,
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

/// the row pitch a texture-to-buffer copy of `width` rgba8 texels needs
pub fn padded_row_bytes(width: u32) -> u32 {
    (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
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
    /// row-padded staging copy, kept for sizes whose row is not a
    /// multiple of the copy alignment so no upload allocates
    padded: Vec<u8>,
}

/// bytes per texture row and the row pitch write_texture needs
fn row_layout(layout: PixelLayout, width: u32) -> (u32, u32) {
    let bytes_per_row = match layout {
        PixelLayout::Yuyv => width * 2,
        PixelLayout::Rgba8 => width * 4,
    };
    let padded_row = bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    (bytes_per_row, padded_row)
}

/// the read-back buffer a cpu pass goes through, kept by size
struct Staging {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
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
    passes: Vec<Slot>,
    bypass: bool,
    staging: Option<Staging>,
    cpu_pixels: Vec<u8>,
}

impl Renderer {
    /// all intermediate textures are rgba8unorm with
    /// RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC | COPY_DST
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
            bypass: false,
            staging: None,
            cpu_pixels: Vec::new(),
        }
    }

    pub fn input_layout(&self) -> &wgpu::BindGroupLayout {
        &self.input_layout
    }

    pub fn sampler(&self) -> &wgpu::Sampler {
        &self.sampler
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
        let texel_width = match frame.layout {
            PixelLayout::Yuyv => frame.width / 2,
            PixelLayout::Rgba8 => frame.width,
        };
        let (bytes_per_row, padded_row) = row_layout(frame.layout, frame.width);
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
        let data: &[u8] = if padded_row == bytes_per_row {
            &frame.data[..row * rows]
        } else {
            for (dst, src) in source
                .padded
                .chunks_exact_mut(padded_row as usize)
                .zip(frame.data.chunks_exact(row))
            {
                dst[..row].copy_from_slice(src);
            }
            &source.padded
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
        let (bytes_per_row, padded_row) = row_layout(frame.layout, frame.width);
        let padded = if padded_row == bytes_per_row {
            Vec::new()
        } else {
            vec![0u8; padded_row as usize * frame.height as usize]
        };
        self.source = Some(Source {
            texture,
            bind_group,
            width: frame.width,
            height: frame.height,
            layout: frame.layout,
            seq: None,
            padded,
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
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
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

    /// runs the convert pass and then every enabled pass, submitting
    /// the work itself; a cpu pass splits the submission in two around
    /// a blocking read-back
    pub fn render(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let (Some(source), Some(targets)) = (&self.source, &self.targets) else {
            return;
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render"),
        });
        let pipeline = match source.layout {
            PixelLayout::Yuyv => &self.convert_yuyv,
            PixelLayout::Rgba8 => &self.convert_copy,
        };
        draw_fullscreen(
            &mut encoder,
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
        if !self.bypass {
            for slot in self.passes.iter_mut() {
                if !slot.pass.enabled() {
                    continue;
                }
                let next = 1 - current;
                if slot.pass.cpu() {
                    queue.submit([encoder.finish()]);
                    let ok = cpu_round_trip(
                        &ctx,
                        &mut self.staging,
                        &mut self.cpu_pixels,
                        slot.pass.as_mut(),
                        &targets[current].texture,
                        &targets[next].texture,
                    );
                    encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("render"),
                    });
                    if !ok {
                        continue;
                    }
                } else {
                    slot.pass.encode(
                        &ctx,
                        &mut encoder,
                        &targets[current].view,
                        &targets[next].view,
                    );
                }
                current = next;
            }
        }
        queue.submit([encoder.finish()]);
        self.output = current;
    }

    /// final texture, none before the first upload
    pub fn output_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t[self.output].view)
    }

    pub fn output_texture(&self) -> Option<&wgpu::Texture> {
        self.targets.as_ref().map(|t| &t[self.output].texture)
    }

    /// both ping-pong targets, so a viewer can bind each once and pick
    /// by output_index() after every render
    pub fn target_views(&self) -> Option<[&wgpu::TextureView; 2]> {
        self.targets.as_ref().map(|t| [&t[0].view, &t[1].view])
    }

    pub fn output_index(&self) -> usize {
        self.output
    }

    pub fn output_size(&self) -> Option<(u32, u32)> {
        self.source.as_ref().map(|s| (s.width, s.height))
    }

    pub fn passes_mut(&mut self) -> &mut Vec<Slot> {
        &mut self.passes
    }

    pub fn passes(&self) -> &[Slot] {
        &self.passes
    }

    pub fn push_pass(&mut self, pass: Box<dyn Pass>) {
        self.passes.push(Slot::new(pass));
    }

    /// with bypass on the output is the converted frame and no pass runs
    pub fn set_bypass(&mut self, on: bool) {
        self.bypass = on;
    }

    pub fn bypass(&self) -> bool {
        self.bypass
    }
}

/// reads `input` back, runs the pass on it and uploads the result into
/// `output`; false when the map failed, which leaves `output` untouched
fn cpu_round_trip(
    ctx: &PassCtx,
    staging: &mut Option<Staging>,
    pixels: &mut Vec<u8>,
    pass: &mut dyn Pass,
    input: &wgpu::Texture,
    output: &wgpu::Texture,
) -> bool {
    let (width, height) = (ctx.width, ctx.height);
    let padded_row = padded_row_bytes(width);
    let fresh = staging
        .as_ref()
        .is_none_or(|s| s.width != width || s.height != height);
    if fresh {
        let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cpu pass staging"),
            size: u64::from(padded_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        *staging = Some(Staging {
            buffer,
            width,
            height,
        });
    }
    let buffer = &staging.as_ref().expect("set above").buffer;
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("cpu pass read back"),
        });
    encoder.copy_texture_to_buffer(
        input.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer,
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
    ctx.queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).ok();
    });
    if let Err(e) = ctx.device.poll(wgpu::PollType::wait_indefinitely()) {
        tracing::warn!(pass = pass.name(), "poll for the cpu pass failed: {e}");
        return false;
    }
    match rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(
                pass = pass.name(),
                "mapping the cpu pass staging failed: {e}"
            );
            return false;
        }
        Err(_) => {
            tracing::warn!(pass = pass.name(), "the map callback never ran");
            return false;
        }
    }
    let row = (width * 4) as usize;
    {
        let view = match slice.get_mapped_range() {
            Ok(view) => view,
            Err(e) => {
                tracing::warn!(pass = pass.name(), "mapped range: {e}");
                buffer.unmap();
                return false;
            }
        };
        pixels.resize(row * height as usize, 0);
        for (dst, src) in pixels
            .chunks_exact_mut(row)
            .zip(view.chunks_exact(padded_row as usize))
        {
            dst.copy_from_slice(&src[..row]);
        }
    }
    buffer.unmap();

    pass.process_cpu(pixels, width, height);
    if pixels.len() < row * height as usize {
        tracing::warn!(
            pass = pass.name(),
            len = pixels.len(),
            "the cpu pass shrank the frame, dropping its output"
        );
        return false;
    }
    ctx.queue.write_texture(
        output.as_image_copy(),
        &pixels[..row * height as usize],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    true
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

    #[test]
    fn padded_rows_round_up_to_256() {
        assert_eq!(padded_row_bytes(4), 256);
        assert_eq!(padded_row_bytes(64), 256);
        assert_eq!(padded_row_bytes(65), 512);
        assert_eq!(padded_row_bytes(1280), 5120);
    }

    #[test]
    fn slots_get_distinct_ids() {
        let a = Slot::new(Box::new(Invert { enabled: true }));
        let b = Slot::new(Box::new(Invert { enabled: true }));
        assert_ne!(a.id, b.id);
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

    /// reads a texture back as tightly packed rgba8 rows
    pub(crate) fn read_back(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let padded_row = padded_row_bytes(width);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read back"),
            size: u64::from(padded_row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
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

    /// a grey yuyv frame of the given size, decoding to 130 everywhere
    pub(crate) fn grey_frame(width: u32, height: u32) -> Frame {
        Frame {
            width,
            height,
            layout: PixelLayout::Yuyv,
            data: vec![128; (width * height * 2) as usize],
            seq: 1,
            received: Instant::now(),
        }
    }

    /// a yuyv frame whose left half is black and right half white
    pub(crate) fn half_frame(width: u32, height: u32) -> Frame {
        let mut data = Vec::with_capacity((width * height * 2) as usize);
        for _ in 0..height {
            for x in 0..width {
                data.push(if x < width / 2 { 16 } else { 235 });
                data.push(128);
            }
        }
        Frame {
            width,
            height,
            layout: PixelLayout::Yuyv,
            data,
            seq: 1,
            received: Instant::now(),
        }
    }

    /// renders the stack over a grey frame and returns the output pixels
    pub(crate) fn render_grey(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        renderer.upload(device, queue, &grey_frame(width, height));
        renderer.render(device, queue);
        let texture = renderer.output_texture().expect("output after upload");
        read_back(device, queue, texture, width, height)
    }

    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_converts_a_yuyv_frame_to_grey() {
        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let pixels = render_grey(&device, &queue, &mut renderer, 4, 2);
        assert_eq!(pixels.len(), 4 * 2 * 4);
        for px in pixels.chunks_exact(4) {
            for c in &px[..3] {
                assert!((i32::from(*c) - 130).abs() <= 2, "pixel {px:?} is not grey");
            }
            assert_eq!(px[3], 255);
        }
    }

    struct Invert {
        enabled: bool,
    }

    impl Pass for Invert {
        fn name(&self) -> &'static str {
            "invert"
        }
        fn enabled(&self) -> bool {
            self.enabled
        }
        fn set_enabled(&mut self, on: bool) {
            self.enabled = on;
        }
        fn ui(&mut self, _ui: &mut egui::Ui) {}
        fn encode(
            &mut self,
            _ctx: &PassCtx,
            _encoder: &mut wgpu::CommandEncoder,
            _input: &wgpu::TextureView,
            _output: &wgpu::TextureView,
        ) {
            unreachable!("a cpu pass is never encoded");
        }
        fn cpu(&self) -> bool {
            true
        }
        fn process_cpu(&mut self, rgba: &mut Vec<u8>, _width: u32, _height: u32) {
            for px in rgba.chunks_exact_mut(4) {
                for c in &mut px[..3] {
                    *c = 255 - *c;
                }
            }
        }
    }

    /// a 70 px wide frame pads its rows to 512 bytes, so the unpad and
    /// repack paths both run; grey 130 inverts to 125
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_cpu_pass_inverts_the_frame() {
        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        renderer.push_pass(Box::new(Invert { enabled: true }));
        let pixels = render_grey(&device, &queue, &mut renderer, 70, 3);
        assert_eq!(renderer.output_index(), 1);
        for px in pixels.chunks_exact(4) {
            for c in &px[..3] {
                assert!(
                    (i32::from(*c) - 125).abs() <= 2,
                    "pixel {px:?} is not inverted"
                );
            }
            assert_eq!(px[3], 255);
        }
        // a second render reuses the staging buffer and the pixel vec
        renderer.render(&device, &queue);
        let again = read_back(&device, &queue, renderer.output_texture().unwrap(), 70, 3);
        assert_eq!(again, pixels);
    }

    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_bypass_skips_every_pass() {
        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        renderer.push_pass(Box::new(Invert { enabled: true }));
        renderer.set_bypass(true);
        let pixels = render_grey(&device, &queue, &mut renderer, 4, 2);
        assert_eq!(renderer.output_index(), 0);
        assert!(pixels.chunks_exact(4).all(|px| px[0] == 130));
    }
}

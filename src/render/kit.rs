//! the pieces every pass builds from: a uniform block with its bind
//! group, an offscreen texture that follows the frame size, the cached
//! input bind group and a shader builder that prepends the shared
//! vertex stage and common.wgsl

use std::marker::PhantomData;

use bytemuck::Pod;

use super::{
    create_fullscreen_pipeline, create_input_bind_group, draw_fullscreen, fullscreen_vertex_source,
    PassCtx, TARGET_FORMAT,
};

/// wgsl helpers shared by every effect shader
pub fn common_source() -> &'static str {
    include_str!("../fx/common.wgsl")
}

/// fullscreen vertex stage + common.wgsl + `body`
pub fn build_shader(device: &wgpu::Device, label: &str, body: &str) -> wgpu::ShaderModule {
    let source = format!(
        "{}\n{}\n{}",
        fullscreen_vertex_source(),
        common_source(),
        body
    );
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

/// one uniform struct at binding 0 of its own bind group, visible to
/// the vertex, fragment and compute stages
pub struct UniformBuffer<T: Pod> {
    pub buffer: wgpu::Buffer,
    pub group: wgpu::BindGroup,
    pub layout: wgpu::BindGroupLayout,
    _marker: PhantomData<T>,
}

impl<T: Pod> UniformBuffer<T> {
    pub fn new(device: &wgpu::Device, label: &str) -> UniformBuffer<T> {
        let size = std::mem::size_of::<T>() as u64;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT | wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(size),
                },
                count: None,
            }],
        });
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        UniformBuffer {
            buffer,
            group,
            layout,
            _marker: PhantomData,
        }
    }

    pub fn write(&self, queue: &wgpu::Queue, value: &T) {
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(value));
    }
}

/// an rgba8 texture a pass renders into and samples from, recreated
/// when the frame size changes
pub struct OffscreenTarget {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

impl OffscreenTarget {
    pub fn new(device: &wgpu::Device, width: u32, height: u32, label: &str) -> OffscreenTarget {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
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
        OffscreenTarget {
            texture,
            view,
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// true when the texture was recreated for a new size
    pub fn ensure(&mut self, device: &wgpu::Device, width: u32, height: u32, label: &str) -> bool {
        if self.width == width.max(1) && self.height == height.max(1) {
            return false;
        }
        *self = OffscreenTarget::new(device, width, height, label);
        true
    }
}

/// the input bind group of a pass, rebuilt only when the view changes
#[derive(Default)]
pub struct InputGroupCache {
    entry: Option<(wgpu::TextureView, wgpu::BindGroup)>,
}

impl InputGroupCache {
    pub fn new() -> InputGroupCache {
        InputGroupCache::default()
    }

    pub fn get(&mut self, ctx: &PassCtx, view: &wgpu::TextureView) -> &wgpu::BindGroup {
        self.get_with(ctx.device, ctx.input_layout, ctx.sampler, view)
    }

    /// the same for a view bound through any layout of the input shape,
    /// such as a pass's own offscreen target
    pub fn get_with(
        &mut self,
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        view: &wgpu::TextureView,
    ) -> &wgpu::BindGroup {
        let stale = self.entry.as_ref().is_none_or(|(v, _)| v != view);
        if stale {
            let group = create_input_bind_group(device, layout, view, sampler);
            self.entry = Some((view.clone(), group));
        }
        &self.entry.as_ref().expect("set above").1
    }
}

/// copies one view into another through the sampler; the identity pass
pub struct Blit {
    pipeline: wgpu::RenderPipeline,
    input: InputGroupCache,
}

impl Blit {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Blit {
        let module = build_shader(
            device,
            "blit",
            "@group(0) @binding(0) var src: texture_2d<f32>;\n\
             @group(0) @binding(1) var src_sampler: sampler;\n\
             @fragment\n\
             fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {\n\
                 return textureSample(src, src_sampler, in.uv);\n\
             }\n",
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(input_layout)],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "blit", &module, "fs_main", &layout);
        Blit {
            pipeline,
            input: InputGroupCache::new(),
        }
    }

    pub fn encode(
        &mut self,
        ctx: &PassCtx,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
    ) {
        let group = self.input.get(ctx, input);
        draw_fullscreen(encoder, "blit", &self.pipeline, &[group], output);
    }
}

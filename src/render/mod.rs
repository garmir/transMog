use crate::capture::Frame;

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

/// source texture, ping-pong targets and the pass stack
pub struct Renderer {
    passes: Vec<Box<dyn Pass>>,
    output: Option<(wgpu::TextureView, (u32, u32))>,
}

impl Renderer {
    /// all intermediate textures are rgba8unorm with
    /// RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC
    pub fn new(_device: &wgpu::Device, _queue: &wgpu::Queue) -> Renderer {
        Renderer {
            passes: Vec::new(),
            output: None,
        }
    }

    /// recreates the textures when the size or layout changes
    pub fn upload(&mut self, _device: &wgpu::Device, _queue: &wgpu::Queue, _frame: &Frame) {
        todo!("upload the frame into the source texture")
    }

    /// runs the convert pass and then every enabled pass
    pub fn render(
        &mut self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _encoder: &mut wgpu::CommandEncoder,
    ) {
        todo!("convert pass followed by the enabled passes")
    }

    /// final texture, none before the first upload
    pub fn output_view(&self) -> Option<&wgpu::TextureView> {
        self.output.as_ref().map(|(view, _)| view)
    }

    pub fn output_size(&self) -> Option<(u32, u32)> {
        self.output.as_ref().map(|(_, size)| *size)
    }

    pub fn passes_mut(&mut self) -> &mut Vec<Box<dyn Pass>> {
        &mut self.passes
    }
}

use eframe::egui_wgpu::RenderState;
use eframe::wgpu;

use crate::capture::Frame;
use crate::fx;
use crate::render::{Pass, Renderer};

pub(crate) struct Preview {
    renderer: Renderer,
    /// egui side of the renderer's output texture, re-registered when the
    /// output size changes
    texture: Option<(egui::TextureId, (u32, u32))>,
    last_seq: Option<u64>,
}

impl Preview {
    pub fn new(rs: &RenderState) -> Preview {
        let mut renderer = Renderer::new(&rs.device, &rs.queue);
        *renderer.passes_mut() = fx::default_stack(&rs.device, renderer.input_layout());
        Preview {
            renderer,
            texture: None,
            last_seq: None,
        }
    }

    /// forgets the last uploaded sequence number so the first frame of a
    /// new capture is not mistaken for an old one
    pub fn reset(&mut self) {
        self.last_seq = None;
    }

    pub fn passes_mut(&mut self) -> &mut Vec<Box<dyn Pass>> {
        self.renderer.passes_mut()
    }

    pub fn upload(&mut self, rs: &RenderState, frame: &Frame) {
        if self.last_seq == Some(frame.seq) {
            return;
        }
        self.last_seq = Some(frame.seq);
        self.renderer.upload(&rs.device, &rs.queue, frame);
    }

    /// runs the pass stack and points the egui texture at the result;
    /// the final view can alternate between the ping-pong targets as
    /// passes are toggled, so the binding is refreshed every frame
    pub fn render(&mut self, rs: &RenderState) {
        if self.renderer.output_view().is_none() {
            return;
        }
        let mut encoder = rs
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("preview"),
            });
        self.renderer.render(&rs.device, &rs.queue, &mut encoder);
        rs.queue.submit(Some(encoder.finish()));

        let (Some(view), Some(size)) = (self.renderer.output_view(), self.renderer.output_size())
        else {
            return;
        };
        let mut egui_renderer = rs.renderer.write();
        match self.texture {
            Some((id, old)) if old == size => {
                egui_renderer.update_egui_texture_from_wgpu_texture(
                    &rs.device,
                    view,
                    wgpu::FilterMode::Linear,
                    id,
                );
            }
            previous => {
                if let Some((id, _)) = previous {
                    egui_renderer.free_texture(&id);
                }
                let id = egui_renderer.register_native_texture(
                    &rs.device,
                    view,
                    wgpu::FilterMode::Linear,
                );
                self.texture = Some((id, size));
            }
        }
    }

    /// draws the output letterboxed into whatever space the panel gives
    pub fn show(&self, ui: &mut egui::Ui) {
        let Some((id, (w, h))) = self.texture else {
            ui.centered_and_justified(|ui| ui.weak("no frame yet"));
            return;
        };
        let avail = ui.available_rect_before_wrap();
        let (w, h) = (w as f32, h as f32);
        let scale = (avail.width() / w).min(avail.height() / h);
        let size = egui::vec2(w * scale, h * scale);
        let rect = egui::Rect::from_center_size(avail.center(), size);
        ui.allocate_rect(avail, egui::Sense::hover());
        ui.put(
            rect,
            egui::Image::from_texture(egui::load::SizedTexture::new(id, egui::vec2(w, h)))
                .fit_to_exact_size(size),
        );
    }
}

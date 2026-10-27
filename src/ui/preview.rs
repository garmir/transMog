use eframe::egui_wgpu::RenderState;
use eframe::wgpu;

use crate::capture::Frame;
use crate::fx;
use crate::render::{Pass, Renderer};

/// egui ids for both ping-pong targets, registered once per set of
/// targets; the views are kept to notice when the renderer swaps them
struct Bound {
    views: [wgpu::TextureView; 2],
    ids: [egui::TextureId; 2],
    size: (u32, u32),
}

pub(crate) struct Preview {
    renderer: Renderer,
    bound: Option<Bound>,
    last_seq: Option<u64>,
    /// a new frame or a parameter change since the last render
    dirty: bool,
}

impl Preview {
    pub fn new(rs: &RenderState) -> Preview {
        let mut renderer = Renderer::new(&rs.device, &rs.queue);
        *renderer.passes_mut() = fx::default_stack(&rs.device, renderer.input_layout());
        Preview {
            renderer,
            bound: None,
            last_seq: None,
            dirty: false,
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

    /// asks for a render even without a new frame, after a pass changed
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn upload(&mut self, rs: &RenderState, frame: &Frame) {
        if self.last_seq == Some(frame.seq) {
            return;
        }
        self.last_seq = Some(frame.seq);
        self.renderer.upload(&rs.device, &rs.queue, frame);
        self.dirty = true;
    }

    /// runs the pass stack when something changed and keeps the egui
    /// bindings in step with the renderer's targets; on the steady path
    /// nothing is created
    pub fn render(&mut self, rs: &RenderState) {
        if self.renderer.output_view().is_none() {
            return;
        }
        if self.dirty {
            self.dirty = false;
            let mut encoder = rs
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("preview"),
                });
            self.renderer.render(&rs.device, &rs.queue, &mut encoder);
            rs.queue.submit(Some(encoder.finish()));
        }

        let (Some(views), Some(size)) = (self.renderer.target_views(), self.renderer.output_size())
        else {
            return;
        };
        let current = self
            .bound
            .as_ref()
            .is_some_and(|b| b.views[0] == *views[0] && b.views[1] == *views[1]);
        if current {
            return;
        }
        let mut egui_renderer = rs.renderer.write();
        if let Some(old) = self.bound.take() {
            for id in old.ids {
                egui_renderer.free_texture(&id);
            }
        }
        let ids = views.map(|view| {
            egui_renderer.register_native_texture(&rs.device, view, wgpu::FilterMode::Linear)
        });
        self.bound = Some(Bound {
            views: [views[0].clone(), views[1].clone()],
            ids,
            size,
        });
    }

    /// draws the output letterboxed into whatever space the panel gives
    pub fn show(&self, ui: &mut egui::Ui) {
        let Some(bound) = &self.bound else {
            ui.centered_and_justified(|ui| ui.weak("no frame yet"));
            return;
        };
        let id = bound.ids[self.renderer.output_index()];
        let (w, h) = (bound.size.0 as f32, bound.size.1 as f32);
        let avail = ui.available_rect_before_wrap();
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

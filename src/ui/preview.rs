use eframe::egui_wgpu::RenderState;
use eframe::wgpu;

use crate::capture::{Frame, FrameMeta};
use crate::fx;
use crate::ndi;
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
    /// the uploaded frame's tap seq and timestamp, until NDI out sends it
    unsent: Option<FrameMeta>,
    ndi: Option<ndi::Output>,
    /// kept here so NDI out comes back at it after being turned off
    ndi_size: (u32, u32),
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
            unsent: None,
            ndi: None,
            ndi_size: ndi::DEFAULT_SIZE,
        }
    }

    /// starts or stops NDI out; an error leaves it off
    pub fn set_ndi(&mut self, rs: &RenderState, on: bool) -> anyhow::Result<()> {
        if !on {
            self.ndi = None;
            return Ok(());
        }
        if self.ndi.is_none() {
            let mut out = ndi::Output::new(&rs.device, self.renderer.input_layout())?;
            out.set_size(self.ndi_size);
            self.ndi = Some(out);
        }
        Ok(())
    }

    pub fn set_ndi_size(&mut self, size: (u32, u32)) {
        self.ndi_size = size;
        if let Some(out) = &mut self.ndi {
            out.set_size(size);
        }
    }

    pub fn ndi_size(&self) -> (u32, u32) {
        self.ndi_size
    }

    pub fn ndi(&self) -> Option<&ndi::Output> {
        self.ndi.as_ref()
    }

    pub fn ndi_mut(&mut self) -> Option<&mut ndi::Output> {
        self.ndi.as_mut()
    }

    /// forgets the last uploaded sequence number so the first frame of a
    /// new capture is not mistaken for an old one
    pub fn reset(&mut self) {
        self.last_seq = None;
    }

    pub fn passes(&self) -> &[Box<dyn Pass>] {
        self.renderer.passes()
    }

    pub fn passes_mut(&mut self) -> &mut Vec<Box<dyn Pass>> {
        self.renderer.passes_mut()
    }

    /// asks for a render even without a new frame, after a pass changed
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn upload(&mut self, rs: &RenderState, frame: &Frame, meta: Option<FrameMeta>) {
        if self.last_seq == Some(frame.seq) {
            return;
        }
        self.last_seq = Some(frame.seq);
        self.renderer.upload(&rs.device, &rs.queue, frame);
        self.unsent = meta.filter(|m| m.seq == frame.seq);
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
            // a parameter change re-renders a frame already sent, so only a
            // new camera frame goes out
            let pending = match (&mut self.ndi, self.unsent.take()) {
                (Some(out), Some(meta)) => {
                    let input = self.renderer.output_view();
                    let size = self.renderer.output_size();
                    match (input, size) {
                        (Some(input), Some(size)) => {
                            out.encode(&rs.device, &rs.queue, &mut encoder, input, size, &meta)
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            let submission = rs.queue.submit(Some(encoder.finish()));
            if let (Some(out), Some(pending)) = (&mut self.ndi, pending) {
                out.submitted(pending, submission);
            }
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
    /// and returns where the image went and its size in pixels
    pub fn show(&self, ui: &mut egui::Ui) -> Option<(egui::Rect, (u32, u32))> {
        let Some(bound) = &self.bound else {
            ui.centered_and_justified(|ui| ui.weak("no frame yet"));
            return None;
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
        Some((rect, bound.size))
    }
}

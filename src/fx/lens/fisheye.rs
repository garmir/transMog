use crate::render::{Pass, PassCtx};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Projection {
    /// k1 polynomial
    Barrel,
    Equidistant,
    Equisolid,
    Stereographic,
}

/// texels that fall outside the source render black
pub struct Fisheye {
    enabled: bool,
    projection: Projection,
    /// 0 is the identity
    strength: f32,
    /// 0.5 ..= 2.0
    zoom: f32,
    centre: [f32; 2],
}

impl Fisheye {
    pub fn new(_device: &wgpu::Device, _input_layout: &wgpu::BindGroupLayout) -> Fisheye {
        Fisheye {
            enabled: false,
            projection: Projection::Barrel,
            strength: 0.0,
            zoom: 1.0,
            centre: [0.0, 0.0],
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
        ui.label(format!(
            "{:?} strength {} zoom {} centre {:?}",
            self.projection, self.strength, self.zoom, self.centre
        ));
    }

    fn encode(
        &mut self,
        _ctx: &PassCtx,
        _encoder: &mut wgpu::CommandEncoder,
        _input: &wgpu::TextureView,
        _output: &wgpu::TextureView,
    ) {
        todo!("fullscreen pass with fisheye.wgsl")
    }
}

pub mod lens;

use crate::render::Pass;

/// the pass stack a fresh renderer starts with
pub fn default_stack(
    device: &wgpu::Device,
    input_layout: &wgpu::BindGroupLayout,
) -> Vec<Box<dyn Pass>> {
    vec![Box::new(lens::fisheye::Fisheye::new(device, input_layout))]
}

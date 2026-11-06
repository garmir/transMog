pub mod assets;
pub mod catalog;
pub mod lens;
pub mod preprocess;
pub mod tools;

use crate::render::Pass;

/// the pass stack a fresh renderer starts with: the shared
/// preprocessing, then the lens; TRANSMOG_STACK overrides it with a
/// comma-separated list of catalog names, so a stack can be set up
/// without touching the ui
pub fn default_stack(
    device: &wgpu::Device,
    input_layout: &wgpu::BindGroupLayout,
) -> Vec<Box<dyn Pass>> {
    if let Some(spec) = std::env::var_os("TRANSMOG_STACK") {
        let spec = spec.to_string_lossy();
        return stack_from_spec(&spec, device, input_layout);
    }
    vec![
        Box::new(preprocess::Preprocess::new(device, input_layout)),
        Box::new(lens::fisheye::Fisheye::new(device, input_layout)),
    ]
}

fn stack_from_spec(
    spec: &str,
    device: &wgpu::Device,
    input_layout: &wgpu::BindGroupLayout,
) -> Vec<Box<dyn Pass>> {
    let mut stack = Vec::new();
    for name in spec.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        match catalog::find(name) {
            Some(entry) => stack.push((entry.make)(device, input_layout)),
            None => tracing::warn!("TRANSMOG_STACK: no pass named {name:?}"),
        }
    }
    stack
}

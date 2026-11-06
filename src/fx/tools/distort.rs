//! distort: a map image's red channel shifts every pixel of the frame
//! by up to x and y pixels, above a threshold; the map is a perlin
//! noise image at the frame size or ~/.config/transmog/distort.png

use std::path::PathBuf;

use bytemuck::{Pod, Zeroable};

use crate::fx::assets::{self, RgbaImage};
use crate::render::kit::{build_shader, InputGroupCache, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx, TARGET_FORMAT};

/// mirrors `Params` in distort.wgsl
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub threshold: f32,
    pub x_shift: f32,
    pub y_shift: f32,
    pub _pad: f32,
}

/// which image the shared preprocessing would apply to on the site;
/// here the preprocessing is its own pass in the stack, so the choice
/// is kept for the ui and does nothing to the map
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreprocessTarget {
    Distortion,
    Base,
}

impl PreprocessTarget {
    pub const ALL: [PreprocessTarget; 2] = [PreprocessTarget::Distortion, PreprocessTarget::Base];

    pub fn label(self) -> &'static str {
        match self {
            PreprocessTarget::Distortion => "distortion",
            PreprocessTarget::Base => "base",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapSource {
    Noise,
    File,
}

impl MapSource {
    pub const ALL: [MapSource; 2] = [MapSource::Noise, MapSource::File];

    pub fn label(self) -> &'static str {
        match self {
            MapSource::Noise => "noise",
            MapSource::File => "distort.png",
        }
    }
}

pub const THRESHOLD_RANGE: std::ops::RangeInclusive<f32> = 0.0..=255.0;
pub const SHIFT_RANGE: std::ops::RangeInclusive<f32> = -100.0..=100.0;
const DEFAULT_THRESHOLD: f32 = 0.0;
const DEFAULT_X_SHIFT: f32 = -75.0;
const DEFAULT_Y_SHIFT: f32 = 0.0;

pub fn map_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/transmog/distort.png"))
}

/// the map on the gpu with the image it was made from, so a size
/// change can tell a noise map (regenerated at the frame size) from a
/// file (kept and stretched)
struct MapTexture {
    view: wgpu::TextureView,
    source: MapSource,
    width: u32,
    height: u32,
}

pub struct Distort {
    enabled: bool,
    target: PreprocessTarget,
    threshold: f32,
    x_shift: f32,
    y_shift: f32,
    source: MapSource,
    /// the png, when it loaded; None falls back to noise
    file: Option<RgbaImage>,
    file_error: Option<String>,
    map: Option<MapTexture>,
    pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
    map_input: InputGroupCache,
}

impl Distort {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Distort {
        let module = build_shader(device, "distort", include_str!("distort.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "distort params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("distort"),
            bind_group_layouts: &[
                Some(input_layout),
                Some(input_layout),
                Some(&uniform.layout),
            ],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "distort", &module, "fs_main", &layout);
        let mut distort = Distort {
            enabled: true,
            target: PreprocessTarget::Distortion,
            threshold: DEFAULT_THRESHOLD,
            x_shift: DEFAULT_X_SHIFT,
            y_shift: DEFAULT_Y_SHIFT,
            source: MapSource::Noise,
            file: None,
            file_error: None,
            map: None,
            pipeline,
            uniform,
            input: InputGroupCache::new(),
            map_input: InputGroupCache::new(),
        };
        distort.load_file();
        distort
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, threshold: f32) {
        self.threshold = threshold.clamp(*THRESHOLD_RANGE.start(), *THRESHOLD_RANGE.end());
    }

    /// x and y shift in pixels
    pub fn shift(&self) -> (f32, f32) {
        (self.x_shift, self.y_shift)
    }

    pub fn set_shift(&mut self, x: f32, y: f32) {
        self.x_shift = x.clamp(*SHIFT_RANGE.start(), *SHIFT_RANGE.end());
        self.y_shift = y.clamp(*SHIFT_RANGE.start(), *SHIFT_RANGE.end());
    }

    pub fn map_source(&self) -> MapSource {
        self.source
    }

    pub fn set_map_source(&mut self, source: MapSource) {
        self.source = source;
    }

    pub fn reset(&mut self) {
        self.target = PreprocessTarget::Distortion;
        self.threshold = DEFAULT_THRESHOLD;
        self.x_shift = DEFAULT_X_SHIFT;
        self.y_shift = DEFAULT_Y_SHIFT;
        self.source = MapSource::Noise;
    }

    /// reads the png; a missing or bad file leaves the noise map in use
    fn load_file(&mut self) {
        self.file = None;
        self.file_error = None;
        let Some(path) = map_path() else {
            self.file_error = Some("HOME is not set".into());
            return;
        };
        if !path.exists() {
            self.file_error = Some(format!("no {}", path.display()));
            return;
        }
        match assets::load_png(&path) {
            Ok(image) if image.width > 0 && image.height > 0 => self.file = Some(image),
            Ok(_) => self.file_error = Some("empty image".into()),
            Err(e) => {
                tracing::warn!("distort map {}: {e:#}", path.display());
                self.file_error = Some(format!("{e:#}"));
            }
        }
        // the texture is rebuilt on the next encode
        self.map = None;
    }

    /// the source actually in use: the file only when it loaded
    fn effective_source(&self) -> MapSource {
        match (self.source, &self.file) {
            (MapSource::File, Some(_)) => MapSource::File,
            _ => MapSource::Noise,
        }
    }

    fn uniform(&self) -> Uniform {
        Uniform {
            threshold: self.threshold,
            x_shift: self.x_shift,
            y_shift: self.y_shift,
            _pad: 0.0,
        }
    }

    /// uploads the map when it is missing, when the source changed or
    /// when a noise map no longer matches the frame size
    fn ensure_map(&mut self, ctx: &PassCtx) {
        let source = self.effective_source();
        let wanted = match source {
            MapSource::Noise => (ctx.width, ctx.height),
            MapSource::File => {
                let f = self
                    .file
                    .as_ref()
                    .expect("effective_source checks the file");
                (f.width, f.height)
            }
        };
        let current = self
            .map
            .as_ref()
            .is_some_and(|m| m.source == source && (m.width, m.height) == wanted);
        if current {
            return;
        }
        let image = match source {
            MapSource::Noise => assets::noise_map(ctx.width, ctx.height),
            MapSource::File => self.file.clone().expect("checked above"),
        };
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("distort map"),
            size: wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            texture.as_image_copy(),
            &image.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&Default::default());
        tracing::debug!(
            ?source,
            width = image.width,
            height = image.height,
            "distort map uploaded"
        );
        self.map = Some(MapTexture {
            view,
            source,
            width: image.width,
            height: image.height,
        });
    }
}

impl Pass for Distort {
    fn name(&self) -> &'static str {
        "distort"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("map");
            egui::ComboBox::from_id_salt("distort map")
                .selected_text(self.source.label())
                .show_ui(ui, |ui| {
                    for source in MapSource::ALL {
                        ui.selectable_value(&mut self.source, source, source.label());
                    }
                });
            if ui.small_button("reload").clicked() {
                self.load_file();
            }
        });
        if self.source == MapSource::File {
            if let Some(e) = &self.file_error {
                ui.weak(format!("{e}; using noise"));
            }
        }
        ui.horizontal(|ui| {
            ui.label("preprocess target");
            egui::ComboBox::from_id_salt("distort target")
                .selected_text(self.target.label())
                .show_ui(ui, |ui| {
                    for target in PreprocessTarget::ALL {
                        ui.selectable_value(&mut self.target, target, target.label());
                    }
                });
        });
        ui.add(egui::Slider::new(&mut self.threshold, THRESHOLD_RANGE).text("threshold"));
        ui.add(
            egui::Slider::new(&mut self.x_shift, SHIFT_RANGE)
                .suffix(" px")
                .text("x shift"),
        );
        ui.add(
            egui::Slider::new(&mut self.y_shift, SHIFT_RANGE)
                .suffix(" px")
                .text("y shift"),
        );
        if ui.button("reset").clicked() {
            self.reset();
        }
    }

    fn encode(
        &mut self,
        ctx: &PassCtx,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
    ) {
        self.ensure_map(ctx);
        self.uniform.write(ctx.queue, &self.uniform());
        let map = self.map.as_ref().expect("ensure_map sets the map");
        let map_group = self.map_input.get(ctx, &map.view);
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "distort",
            &self.pipeline,
            &[input_group, map_group, &self.uniform.group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_16_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 16);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, threshold), 0);
        assert_eq!(std::mem::offset_of!(Uniform, x_shift), 4);
        assert_eq!(std::mem::offset_of!(Uniform, y_shift), 8);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 16);
    }

    /// the default -75 px x shift drags the black and white halves
    /// across each other somewhere, and nothing but black and white
    /// comes out since the lookup is nearest
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_shifts_the_edge_without_blending() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let distort = Distort::new(&device, renderer.input_layout());
        renderer.push_pass(Box::new(distort));
        let frame = half_frame(64, 32);
        renderer.upload(&device, &queue, &frame);
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        assert_eq!(pixels.len(), 64 * 32 * 4);
        let mut moved = 0;
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            let x = i % 64;
            assert!(px[0] <= 2 || px[0] >= 253, "pixel {i} blended: {px:?}");
            let expected = if x < 32 { 0 } else { 255 };
            if (i32::from(px[0]) - expected).abs() > 2 {
                moved += 1;
            }
        }
        assert!(moved > 0, "the map shifted nothing");
    }
}

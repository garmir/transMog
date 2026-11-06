//! patterns: a grid of square cells, each filled with one of a set of
//! tiles chosen by the cell's brightness. the six built-in tiles are
//! joined by any png in ~/.config/transmog/patterns, and the set is
//! sorted darkest first so the rank the shader computes maps straight
//! onto the atlas

use std::path::PathBuf;

use bytemuck::{Pod, Zeroable};

use crate::fx::assets::{self, RgbaImage, TILE_SIZE};
use crate::render::kit::{build_shader, InputGroupCache, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

/// mirrors `Params` in patterns.wgsl
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub threshold: f32,
    pub density: f32,
    pub tile_count: u32,
    pub tile_px: u32,
}

pub const THRESHOLD_RANGE: std::ops::RangeInclusive<u32> = 0..=255;
pub const DENSITY_RANGE: std::ops::RangeInclusive<u32> = 10..=150;
const DEFAULT_THRESHOLD: u32 = 178;
const DEFAULT_DENSITY: u32 = 49;
/// a user tile larger than this is sampled down to it
const MAX_TILE_PX: u32 = 256;

pub fn user_tile_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/transmog/patterns"))
}

/// nearest-neighbour resample of a tile into a `size` square
fn resample(tile: &RgbaImage, size: u32) -> RgbaImage {
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        let sy = (y * tile.height / size).min(tile.height.saturating_sub(1));
        for x in 0..size {
            let sx = (x * tile.width / size).min(tile.width.saturating_sub(1));
            let i = ((sy * tile.width + sx) * 4) as usize;
            data.extend_from_slice(&tile.data[i..i + 4]);
        }
    }
    RgbaImage {
        width: size,
        height: size,
        data,
    }
}

/// one row of square tiles, darkest first, every tile scaled to the
/// largest side among them (at least the built-in 32)
pub fn build_atlas(tiles: &[RgbaImage]) -> (RgbaImage, u32) {
    let size = tiles
        .iter()
        .map(|t| t.width.max(t.height))
        .max()
        .unwrap_or(TILE_SIZE)
        .clamp(TILE_SIZE, MAX_TILE_PX);
    let mut sorted: Vec<RgbaImage> = tiles
        .iter()
        .filter(|t| {
            t.width > 0 && t.height > 0 && t.data.len() >= (t.width * t.height * 4) as usize
        })
        .map(|t| resample(t, size))
        .collect();
    sorted.sort_by(|a, b| a.mean_brightness().total_cmp(&b.mean_brightness()));
    let count = sorted.len().max(1) as u32;
    let width = size * count;
    let mut data = vec![255u8; (width * size * 4) as usize];
    for (i, tile) in sorted.iter().enumerate() {
        for y in 0..size {
            let src = ((y * size) * 4) as usize;
            let dst = ((y * width + i as u32 * size) * 4) as usize;
            data[dst..dst + (size * 4) as usize]
                .copy_from_slice(&tile.data[src..src + (size * 4) as usize]);
        }
    }
    (
        RgbaImage {
            width,
            height: size,
            data,
        },
        sorted.len() as u32,
    )
}

/// the built-in tiles plus the user's pngs
pub fn load_tiles() -> Vec<RgbaImage> {
    let mut tiles: Vec<RgbaImage> = assets::default_tiles()
        .iter()
        .map(|t| t.to_rgba())
        .collect();
    if let Some(dir) = user_tile_dir() {
        let user = assets::load_pngs(&dir);
        if !user.is_empty() {
            tracing::info!(
                count = user.len(),
                "loaded pattern tiles from {}",
                dir.display()
            );
        }
        tiles.extend(user);
    }
    tiles
}

struct AtlasTexture {
    view: wgpu::TextureView,
    input: InputGroupCache,
}

pub struct Patterns {
    enabled: bool,
    threshold: u32,
    density: u32,
    atlas: RgbaImage,
    tile_count: u32,
    /// the atlas image changed and the texture needs (re)uploading
    atlas_dirty: bool,
    texture: Option<AtlasTexture>,
    pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
}

impl Patterns {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Patterns {
        let module = build_shader(device, "patterns", include_str!("patterns.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "patterns params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("patterns"),
            bind_group_layouts: &[
                Some(input_layout),
                Some(&uniform.layout),
                Some(input_layout),
            ],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "patterns", &module, "fs_main", &layout);
        let (atlas, tile_count) = build_atlas(&load_tiles());
        Patterns {
            enabled: true,
            threshold: DEFAULT_THRESHOLD,
            density: DEFAULT_DENSITY,
            atlas,
            tile_count,
            atlas_dirty: true,
            texture: None,
            pipeline,
            uniform,
            input: InputGroupCache::new(),
        }
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, t: u32) {
        self.threshold = t.min(*THRESHOLD_RANGE.end());
    }

    pub fn density(&self) -> u32 {
        self.density
    }

    pub fn set_density(&mut self, d: u32) {
        self.density = d.clamp(*DENSITY_RANGE.start(), *DENSITY_RANGE.end());
    }

    pub fn tile_count(&self) -> u32 {
        self.tile_count
    }

    /// replaces the tile set; the atlas is uploaded on the next encode
    pub fn set_tiles(&mut self, tiles: &[RgbaImage]) {
        let (atlas, count) = build_atlas(tiles);
        self.atlas = atlas;
        self.tile_count = count;
        self.atlas_dirty = true;
    }

    pub fn reload_tiles(&mut self) {
        self.set_tiles(&load_tiles());
    }

    /// back to the site's defaults; the tile set and the enabled flag
    /// are left alone
    pub fn reset(&mut self) {
        self.threshold = DEFAULT_THRESHOLD;
        self.density = DEFAULT_DENSITY;
    }

    fn uniform(&self) -> Uniform {
        Uniform {
            threshold: (self.threshold as f32).max(1e-3),
            density: self.density as f32,
            tile_count: self.tile_count,
            tile_px: self.atlas.height,
        }
    }

    fn upload_atlas(&mut self, ctx: &PassCtx) {
        if !self.atlas_dirty {
            return;
        }
        self.atlas_dirty = false;
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("patterns atlas"),
            size: wgpu::Extent3d {
                width: self.atlas.width,
                height: self.atlas.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            texture.as_image_copy(),
            &self.atlas.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.atlas.width * 4),
                rows_per_image: Some(self.atlas.height),
            },
            wgpu::Extent3d {
                width: self.atlas.width,
                height: self.atlas.height,
                depth_or_array_layers: 1,
            },
        );
        self.texture = Some(AtlasTexture {
            view: texture.create_view(&Default::default()),
            input: InputGroupCache::new(),
        });
    }
}

impl Pass for Patterns {
    fn name(&self) -> &'static str {
        "patterns"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.threshold, THRESHOLD_RANGE).text("threshold"));
        ui.add(egui::Slider::new(&mut self.density, DENSITY_RANGE).text("grid density"));
        ui.horizontal(|ui| {
            ui.label(format!("{} tiles", self.tile_count));
            if ui.button("reload tiles").clicked() {
                self.reload_tiles();
            }
        });
        if let Some(dir) = user_tile_dir() {
            ui.weak(format!("pngs in {}", dir.display()));
        }
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
        self.upload_atlas(ctx);
        self.uniform.write(ctx.queue, &self.uniform());
        let atlas = self
            .texture
            .as_mut()
            .expect("upload_atlas sets the texture");
        let atlas_group =
            atlas
                .input
                .get_with(ctx.device, ctx.input_layout, ctx.sampler, &atlas.view);
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "patterns",
            &self.pipeline,
            &[input_group, &self.uniform.group, atlas_group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_16_bytes() {
        assert_eq!(std::mem::size_of::<Uniform>(), 16);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, threshold), 0);
        assert_eq!(std::mem::offset_of!(Uniform, density), 4);
        assert_eq!(std::mem::offset_of!(Uniform, tile_count), 8);
        assert_eq!(std::mem::offset_of!(Uniform, tile_px), 12);
    }

    /// the built-in set gives a 192x32 row with solid black first
    #[test]
    fn default_atlas_is_six_tiles_dark_first() {
        let tiles: Vec<RgbaImage> = assets::default_tiles()
            .iter()
            .map(|t| t.to_rgba())
            .collect();
        let (atlas, count) = build_atlas(&tiles);
        assert_eq!(count, 6);
        assert_eq!((atlas.width, atlas.height), (192, 32));
        assert!(atlas.data[..32 * 4].chunks_exact(4).all(|px| px[0] == 0));
        assert!(atlas.data[32 * 4..64 * 4]
            .chunks_exact(4)
            .any(|px| px[0] == 255));
    }

    /// a 64 px user tile raises the cell to 64 and the 32 px built-ins
    /// are scaled up with it; a 2x2 tile is scaled the other way
    #[test]
    fn atlas_scales_every_tile_to_the_largest() {
        let big = RgbaImage {
            width: 64,
            height: 64,
            data: vec![200; 64 * 64 * 4],
        };
        let tiny = RgbaImage {
            width: 2,
            height: 2,
            data: vec![
                0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
            ],
        };
        let (atlas, count) = build_atlas(&[big, tiny]);
        assert_eq!(count, 2);
        assert_eq!((atlas.width, atlas.height), (128, 64));
        // the checker sorts first; its top-left quadrant is black
        assert_eq!(atlas.data[0], 0);
        assert_eq!(atlas.data[(31 * 128 + 31) * 4], 0);
        assert_eq!(atlas.data[(31 * 128 + 32) * 4], 255);
        assert_eq!(atlas.data[64 * 4], 200);
    }

    #[test]
    fn empty_tile_set_still_builds() {
        let (atlas, count) = build_atlas(&[]);
        assert_eq!(count, 0);
        assert_eq!((atlas.width, atlas.height), (32, 32));
    }

    fn count_black(pixels: &[u8], width: usize, x_range: std::ops::Range<usize>) -> usize {
        pixels
            .chunks_exact(4)
            .enumerate()
            .filter(|(i, px)| x_range.contains(&(i % width)) && px[0] < 128)
            .count()
    }

    /// at density 10 on a 64x32 frame the cells are 3.2 px; black cells
    /// take the solid tile and white cells are above the threshold, so
    /// the left half is all ink and the right half none
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_black_half_gets_the_solid_tile() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let mut patterns = Patterns::new(&device, renderer.input_layout());
        patterns.set_density(10);
        renderer.push_pass(Box::new(patterns));
        renderer.upload(&device, &queue, &half_frame(64, 32));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        let left = count_black(&pixels, 64, 0..32);
        let right = count_black(&pixels, 64, 32..64);
        assert_eq!(left, 32 * 32, "left half has {left} black pixels");
        assert_eq!(right, 0, "right half has {right} black pixels");
    }
}

//! ascii: a cols x rows grid of glyphs from a charset, each picked by
//! the brightness under the cell's top-left pixel. the charset is
//! rasterised once into an atlas, ranked by ink, and the shader draws
//! the glyphs black on white with an optional box border

use bytemuck::{Pod, Zeroable};

use crate::fx::assets::{self, Atlas};
use crate::render::kit::{build_shader, InputGroupCache, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Border {
    Single,
    Double,
    Thick,
}

impl Border {
    pub const ALL: [Border; 3] = [Border::Single, Border::Double, Border::Thick];

    pub fn label(self) -> &'static str {
        match self {
            Border::Single => "single",
            Border::Double => "double",
            Border::Thick => "thick",
        }
    }

    /// matches the constants in ascii.wgsl; 0 is no border
    fn index(self) -> u32 {
        match self {
            Border::Single => 1,
            Border::Double => 2,
            Border::Thick => 3,
        }
    }
}

/// mirrors `Params` in ascii.wgsl: two rows of 16 bytes
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub cols: u32,
    pub rows: u32,
    pub glyphs: u32,
    pub border: u32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub atlas_w: f32,
    pub _pad: f32,
}

pub const COLS_RANGE: std::ops::RangeInclusive<u32> = 10..=200;
pub const ROWS_RANGE: std::ops::RangeInclusive<u32> = 10..=150;
pub const DEFAULT_CHARSET: &str = " .:-=+*#%@";
const DEFAULT_COLS: u32 = 48;
const DEFAULT_ROWS: u32 = 26;
const DEFAULT_BORDER: Border = Border::Single;
/// the site renders its glyphs at 16 px
const GLYPH_PX: u32 = 16;

struct AtlasTexture {
    view: wgpu::TextureView,
    input: InputGroupCache,
}

pub struct Ascii {
    enabled: bool,
    cols: u32,
    rows: u32,
    charset: String,
    borders: bool,
    border: Border,
    atlas: Atlas,
    /// the atlas was rebuilt and needs (re)uploading
    atlas_dirty: bool,
    texture: Option<AtlasTexture>,
    pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
}

impl Ascii {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Ascii {
        let module = build_shader(device, "ascii", include_str!("ascii.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "ascii params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ascii"),
            bind_group_layouts: &[
                Some(input_layout),
                Some(&uniform.layout),
                Some(input_layout),
            ],
            immediate_size: 0,
        });
        let pipeline = create_fullscreen_pipeline(device, "ascii", &module, "fs_main", &layout);
        Ascii {
            enabled: true,
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            charset: DEFAULT_CHARSET.to_owned(),
            borders: false,
            border: DEFAULT_BORDER,
            atlas: assets::glyph_atlas(DEFAULT_CHARSET, GLYPH_PX),
            atlas_dirty: true,
            texture: None,
            pipeline,
            uniform,
            input: InputGroupCache::new(),
        }
    }

    pub fn grid(&self) -> (u32, u32) {
        (self.cols, self.rows)
    }

    pub fn set_grid(&mut self, cols: u32, rows: u32) {
        self.cols = cols.clamp(*COLS_RANGE.start(), *COLS_RANGE.end());
        self.rows = rows.clamp(*ROWS_RANGE.start(), *ROWS_RANGE.end());
    }

    pub fn charset(&self) -> &str {
        &self.charset
    }

    /// rebuilds the atlas; the glyphs are ranked by ink, not kept in
    /// the order given
    pub fn set_charset(&mut self, charset: &str) {
        if charset == self.charset && !self.atlas.is_empty() {
            return;
        }
        self.charset = charset.to_owned();
        self.atlas = assets::glyph_atlas(charset, GLYPH_PX);
        self.atlas_dirty = true;
    }

    pub fn borders(&self) -> bool {
        self.borders
    }

    pub fn set_borders(&mut self, on: bool) {
        self.borders = on;
    }

    pub fn border(&self) -> Border {
        self.border
    }

    pub fn set_border(&mut self, border: Border) {
        self.border = border;
    }

    /// back to the site's defaults; the enabled flag is left alone
    pub fn reset(&mut self) {
        self.cols = DEFAULT_COLS;
        self.rows = DEFAULT_ROWS;
        self.borders = false;
        self.border = DEFAULT_BORDER;
        self.set_charset(DEFAULT_CHARSET);
    }

    fn uniform(&self) -> Uniform {
        Uniform {
            cols: self.cols,
            rows: self.rows,
            glyphs: self.atlas.len() as u32,
            border: if self.borders { self.border.index() } else { 0 },
            cell_w: self.atlas.cell_width as f32,
            cell_h: self.atlas.cell_height as f32,
            atlas_w: self.atlas.width as f32,
            _pad: 0.0,
        }
    }

    fn upload_atlas(&mut self, ctx: &PassCtx) {
        if !self.atlas_dirty {
            return;
        }
        self.atlas_dirty = false;
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ascii atlas"),
            size: wgpu::Extent3d {
                width: self.atlas.width,
                height: self.atlas.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            texture.as_image_copy(),
            &self.atlas.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.atlas.width),
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

impl Pass for Ascii {
    fn name(&self) -> &'static str {
        "ascii"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(egui::Slider::new(&mut self.cols, COLS_RANGE).text("columns"));
        ui.add(egui::Slider::new(&mut self.rows, ROWS_RANGE).text("rows"));
        ui.horizontal(|ui| {
            ui.label("charset");
            let mut charset = self.charset.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut charset).font(egui::TextStyle::Monospace))
                .changed()
            {
                self.set_charset(&charset);
            }
        });
        ui.weak(format!("ranked: {:?}", self.atlas.ranked()));
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.borders, "borders");
            ui.add_enabled_ui(self.borders, |ui| {
                egui::ComboBox::from_id_salt("ascii border")
                    .selected_text(self.border.label())
                    .show_ui(ui, |ui| {
                        for border in Border::ALL {
                            ui.selectable_value(&mut self.border, border, border.label());
                        }
                    });
            });
        });
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
            "ascii",
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
    fn uniform_is_32_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 32);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, cols), 0);
        assert_eq!(std::mem::offset_of!(Uniform, border), 12);
        assert_eq!(std::mem::offset_of!(Uniform, cell_w), 16);
        assert_eq!(std::mem::offset_of!(Uniform, atlas_w), 24);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 32);
    }

    /// pixels with any ink; a blank cell is exactly 255
    fn count_inked(pixels: &[u8], width: usize, x_range: std::ops::Range<usize>) -> usize {
        pixels
            .chunks_exact(4)
            .enumerate()
            .filter(|(i, px)| x_range.contains(&(i % width)) && px[0] < 250)
            .count()
    }

    /// 16 columns on 64 px gives 4 px cells: the white half maps to the
    /// space and stays blank while the black half draws '@'. the
    /// border run checks the box path draws ink along the top edge
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_white_half_has_no_ink() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let mut ascii = Ascii::new(&device, renderer.input_layout());
        ascii.set_grid(16, 10);
        renderer.push_pass(Box::new(ascii));
        renderer.upload(&device, &queue, &half_frame(64, 32));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        let left = count_inked(&pixels, 64, 0..32);
        let right = count_inked(&pixels, 64, 32..64);
        assert_eq!(right, 0, "right half has {right} inked pixels");
        assert!(left > 0, "left half drew nothing");

        let mut ascii = Ascii::new(&device, renderer.input_layout());
        ascii.set_grid(16, 10);
        ascii.set_borders(true);
        ascii.set_border(Border::Thick);
        renderer.passes_mut()[0].pass = Box::new(ascii);
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        // the thick line sits on the outer cells' centre row, y = 1.6
        let top_row_dark = (0..64).filter(|&x| pixels[(64 + x) * 4] < 128).count();
        assert!(
            top_row_dark > 48,
            "top border row has {top_row_dark} dark pixels"
        );
        assert_eq!(
            pixels[(64 * 5 + 40) * 4],
            255,
            "inside stays blank on the white half"
        );
    }
}

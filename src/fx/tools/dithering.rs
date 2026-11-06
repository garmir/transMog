//! dithering: the frame is box-averaged into pixel_size cells, then
//! each cell is quantised to black and white or to a palette by an
//! ordered, random or floyd-steinberg rule. bayer and random run on
//! the gpu; floyd-steinberg diffuses error serially and runs on the
//! cpu over the cell grid

use bytemuck::{Pod, Zeroable};

use crate::render::kit::{build_shader, InputGroupCache, OffscreenTarget, UniformBuffer};
use crate::render::{create_fullscreen_pipeline, draw_fullscreen, Pass, PassCtx};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pattern {
    FloydSteinberg,
    Bayer,
    Random,
}

impl Pattern {
    pub const ALL: [Pattern; 3] = [Pattern::FloydSteinberg, Pattern::Bayer, Pattern::Random];

    pub fn label(self) -> &'static str {
        match self {
            Pattern::FloydSteinberg => "floyd-steinberg",
            Pattern::Bayer => "bayer",
            Pattern::Random => "random",
        }
    }

    /// matches the constants in dithering.wgsl
    fn index(self) -> u32 {
        match self {
            Pattern::FloydSteinberg => 0,
            Pattern::Bayer => 1,
            Pattern::Random => 2,
        }
    }
}

pub const MAX_COLOURS: usize = 32;

/// mirrors `Params` in dithering.wgsl: eight words then 32 vec4 palette
/// entries, so 544 bytes
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniform {
    pub pixel_size: u32,
    pub pattern: u32,
    pub colour_mode: u32,
    pub colour_count: u32,
    pub threshold: f32,
    pub frame: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub palette: [[f32; 4]; MAX_COLOURS],
}

pub const PIXEL_SIZE_RANGE: std::ops::RangeInclusive<u32> = 1..=20;
pub const COLOUR_COUNT_RANGE: std::ops::RangeInclusive<u32> = 2..=32;
pub const THRESHOLD_RANGE: std::ops::RangeInclusive<u32> = 0..=255;
const DEFAULT_PATTERN: Pattern = Pattern::FloydSteinberg;
const DEFAULT_PIXEL_SIZE: u32 = 2;
const DEFAULT_COLOUR_COUNT: u32 = 24;
const DEFAULT_THRESHOLD: u32 = 255;

/// the site's palette: black, white, then a k³ rgb cube with
/// k = ceil((n - 2)^(1/3)) minus its black and white corners, cut to n
pub fn palette(n: u32) -> Vec<[f32; 3]> {
    let n = n.clamp(2, MAX_COLOURS as u32) as usize;
    let mut out = vec![[0.0, 0.0, 0.0], [1.0, 1.0, 1.0]];
    // a cube of side 1 has no colours between its corners; two is the
    // smallest cube that adds any
    let k = (((n - 2) as f32).cbrt().ceil() as usize).max(2);
    for r in 0..k {
        for g in 0..k {
            for b in 0..k {
                let corner =
                    (r == 0 && g == 0 && b == 0) || (r == k - 1 && g == k - 1 && b == k - 1);
                if corner {
                    continue;
                }
                let s = (k - 1) as f32;
                out.push([r as f32 / s, g as f32 / s, b as f32 / s]);
            }
        }
    }
    out.truncate(n);
    out
}

fn weighted_dist(c: [f32; 3], p: [f32; 3]) -> f32 {
    let d = [c[0] - p[0], c[1] - p[1], c[2] - p[2]];
    0.299 * d[0] * d[0] + 0.587 * d[1] * d[1] + 0.114 * d[2] * d[2]
}

fn nearest(c: [f32; 3], palette: &[[f32; 3]]) -> [f32; 3] {
    let mut best = palette[0];
    let mut best_d = f32::MAX;
    for &p in palette {
        let d = weighted_dist(c, p);
        if d < best_d {
            best = p;
            best_d = d;
        }
    }
    best
}

/// box-averages `rgba` into cells of `ps` px, channels 0..1; the
/// stack's targets are opaque, so alpha is ignored
fn downsample(
    rgba: &[u8],
    width: usize,
    height: usize,
    ps: usize,
    cells: &mut Vec<[f32; 3]>,
) -> (usize, usize) {
    let cw = width.div_ceil(ps);
    let ch = height.div_ceil(ps);
    let mut sums = vec![[0u32; 3]; cw * ch];
    for y in 0..height {
        let row = &rgba[y * width * 4..(y + 1) * width * 4];
        let base = (y / ps) * cw;
        for (cx, sum) in sums[base..base + cw].iter_mut().enumerate() {
            let x0 = cx * ps;
            let x1 = (x0 + ps).min(width);
            for x in x0..x1 {
                let i = x * 4;
                sum[0] += u32::from(row[i]);
                sum[1] += u32::from(row[i + 1]);
                sum[2] += u32::from(row[i + 2]);
            }
        }
    }
    cells.clear();
    cells.reserve(cw * ch);
    for (i, sum) in sums.iter().enumerate() {
        let (cx, cy) = (i % cw, i / cw);
        let w = ps.min(width - cx * ps);
        let h = ps.min(height - cy * ps);
        let scale = 1.0 / (255.0 * (w * h) as f32);
        cells.push([
            sum[0] as f32 * scale,
            sum[1] as f32 * scale,
            sum[2] as f32 * scale,
        ]);
    }
    (cw, ch)
}

/// the site's mono floyd-steinberg over a cell grid of brightness
/// values 0..1: scale by 255 / threshold, quantise at 127, and push the
/// error, scaled back, to 7/16 right, 3/16 down-left, 5/16 down and
/// 1/16 down-right
fn diffuse_mono(v: &mut [f32], cw: usize, ch: usize, threshold: f32) {
    let scale = 255.0 / threshold.max(1.0);
    let back = threshold.max(1.0) / 255.0 / 255.0;
    for y in 0..ch {
        let row = y * cw;
        let below = y + 1 < ch;
        for x in 0..cw {
            let i = row + x;
            let s = (v[i] * 255.0 * scale).min(255.0);
            let q = if s > 127.0 { 1.0 } else { 0.0 };
            let e = (s - q * 255.0) * back;
            v[i] = q;
            if x + 1 < cw {
                v[i + 1] += e * (7.0 / 16.0);
            }
            if below {
                if x > 0 {
                    v[i + cw - 1] += e * (3.0 / 16.0);
                }
                v[i + cw] += e * (5.0 / 16.0);
                if x + 1 < cw {
                    v[i + cw + 1] += e * (1.0 / 16.0);
                }
            }
        }
    }
}

/// floyd-steinberg against a palette with the error carried per
/// channel
fn diffuse_colour(cells: &mut [[f32; 3]], cw: usize, ch: usize, palette: &[[f32; 3]]) {
    for y in 0..ch {
        let row = y * cw;
        let below = y + 1 < ch;
        for x in 0..cw {
            let i = row + x;
            let c = cells[i];
            let p = nearest(c, palette);
            let e = [c[0] - p[0], c[1] - p[1], c[2] - p[2]];
            cells[i] = p;
            let mut push = |j: usize, w: f32| {
                let t = &mut cells[j];
                t[0] += e[0] * w;
                t[1] += e[1] * w;
                t[2] += e[2] * w;
            };
            if x + 1 < cw {
                push(i + 1, 7.0 / 16.0);
            }
            if below {
                if x > 0 {
                    push(i + cw - 1, 3.0 / 16.0);
                }
                push(i + cw, 5.0 / 16.0);
                if x + 1 < cw {
                    push(i + cw + 1, 1.0 / 16.0);
                }
            }
        }
    }
}

/// writes the cell colours back at pixel size with nearest sampling:
/// the first row of each cell row is built per pixel and the rest of
/// the block is copied from it
fn upsample(
    cells: &[[f32; 3]],
    cw: usize,
    ps: usize,
    rgba: &mut [u8],
    width: usize,
    height: usize,
) {
    let stride = width * 4;
    let mut y = 0;
    let mut cy = 0;
    while y < height {
        let row = &mut rgba[y * stride..(y + 1) * stride];
        for (cx, c) in cells[cy * cw..(cy + 1) * cw].iter().enumerate() {
            let px = [
                (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                255,
            ];
            let x0 = cx * ps;
            let x1 = (x0 + ps).min(width);
            for chunk in row[x0 * 4..x1 * 4].chunks_exact_mut(4) {
                chunk.copy_from_slice(&px);
            }
        }
        let y1 = (y + ps).min(height);
        for dy in y + 1..y1 {
            rgba.copy_within(y * stride..(y + 1) * stride, dy * stride);
        }
        y = y1;
        cy += 1;
    }
}

pub struct Dithering {
    enabled: bool,
    pattern: Pattern,
    pixel_size: u32,
    colour_mode: bool,
    colour_count: u32,
    threshold: u32,
    /// random seed, stepped on every encode
    frame: u32,
    /// scratch for the cpu path, kept across frames
    scratch: Vec<[f32; 3]>,
    mono: Vec<f32>,
    cells_pipeline: wgpu::RenderPipeline,
    dither_pipeline: wgpu::RenderPipeline,
    uniform: UniformBuffer<Uniform>,
    input: InputGroupCache,
    cells_input: InputGroupCache,
    cells: OffscreenTarget,
}

impl Dithering {
    pub fn new(device: &wgpu::Device, input_layout: &wgpu::BindGroupLayout) -> Dithering {
        let module = build_shader(device, "dithering", include_str!("dithering.wgsl"));
        let uniform = UniformBuffer::<Uniform>::new(device, "dithering params");
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dithering"),
            bind_group_layouts: &[Some(input_layout), Some(&uniform.layout)],
            immediate_size: 0,
        });
        let pipeline =
            |label, entry| create_fullscreen_pipeline(device, label, &module, entry, &layout);
        Dithering {
            enabled: true,
            pattern: DEFAULT_PATTERN,
            pixel_size: DEFAULT_PIXEL_SIZE,
            colour_mode: false,
            colour_count: DEFAULT_COLOUR_COUNT,
            threshold: DEFAULT_THRESHOLD,
            frame: 0,
            scratch: Vec::new(),
            mono: Vec::new(),
            cells_pipeline: pipeline("dithering cells", "fs_cells"),
            dither_pipeline: pipeline("dithering", "fs_dither"),
            uniform,
            input: InputGroupCache::new(),
            cells_input: InputGroupCache::new(),
            cells: OffscreenTarget::new(device, 1, 1, "dithering cells"),
        }
    }

    pub fn pattern(&self) -> Pattern {
        self.pattern
    }

    pub fn set_pattern(&mut self, pattern: Pattern) {
        self.pattern = pattern;
    }

    pub fn pixel_size(&self) -> u32 {
        self.pixel_size
    }

    pub fn set_pixel_size(&mut self, px: u32) {
        self.pixel_size = px.clamp(*PIXEL_SIZE_RANGE.start(), *PIXEL_SIZE_RANGE.end());
    }

    pub fn colour_mode(&self) -> bool {
        self.colour_mode
    }

    pub fn set_colour_mode(&mut self, on: bool) {
        self.colour_mode = on;
    }

    pub fn colour_count(&self) -> u32 {
        self.colour_count
    }

    pub fn set_colour_count(&mut self, n: u32) {
        self.colour_count = n.clamp(*COLOUR_COUNT_RANGE.start(), *COLOUR_COUNT_RANGE.end());
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    pub fn set_threshold(&mut self, t: u32) {
        self.threshold = t.min(*THRESHOLD_RANGE.end());
    }

    /// back to the site's defaults; the enabled flag is left alone
    pub fn reset(&mut self) {
        self.pattern = DEFAULT_PATTERN;
        self.pixel_size = DEFAULT_PIXEL_SIZE;
        self.colour_mode = false;
        self.colour_count = DEFAULT_COLOUR_COUNT;
        self.threshold = DEFAULT_THRESHOLD;
    }

    fn active_palette(&self) -> Vec<[f32; 3]> {
        if self.colour_mode {
            palette(self.colour_count)
        } else {
            Vec::new()
        }
    }

    fn uniform(&self) -> Uniform {
        let mut u = Uniform {
            pixel_size: self.pixel_size,
            pattern: self.pattern.index(),
            colour_mode: u32::from(self.colour_mode),
            colour_count: 0,
            threshold: self.threshold as f32,
            frame: self.frame,
            _pad0: 0,
            _pad1: 0,
            palette: [[0.0; 4]; MAX_COLOURS],
        };
        for (slot, c) in u.palette.iter_mut().zip(self.active_palette()) {
            *slot = [c[0], c[1], c[2], 1.0];
            u.colour_count += 1;
        }
        u
    }
}

impl Pass for Dithering {
    fn name(&self) -> &'static str {
        "dithering"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("pattern");
            egui::ComboBox::from_id_salt("dithering pattern")
                .selected_text(self.pattern.label())
                .show_ui(ui, |ui| {
                    for pattern in Pattern::ALL {
                        ui.selectable_value(&mut self.pattern, pattern, pattern.label());
                    }
                });
        });
        ui.add(
            egui::Slider::new(&mut self.pixel_size, PIXEL_SIZE_RANGE)
                .suffix(" px")
                .text("pixel size"),
        );
        ui.checkbox(&mut self.colour_mode, "colour mode");
        if self.colour_mode {
            ui.add(egui::Slider::new(&mut self.colour_count, COLOUR_COUNT_RANGE).text("colours"));
        } else {
            ui.add(egui::Slider::new(&mut self.threshold, THRESHOLD_RANGE).text("threshold"));
        }
        if ui.button("reset").clicked() {
            self.reset();
        }
    }

    fn cpu(&self) -> bool {
        self.pattern == Pattern::FloydSteinberg
    }

    fn process_cpu(&mut self, rgba: &mut Vec<u8>, width: u32, height: u32) {
        let (width, height) = (width as usize, height as usize);
        if rgba.len() < width * height * 4 {
            return;
        }
        let ps = self.pixel_size.max(1) as usize;
        let (cw, ch) = downsample(rgba, width, height, ps, &mut self.scratch);
        if self.colour_mode {
            let palette = palette(self.colour_count);
            diffuse_colour(&mut self.scratch, cw, ch, &palette);
        } else {
            self.mono.clear();
            self.mono
                .extend(self.scratch.iter().map(|c| (c[0] + c[1] + c[2]) / 3.0));
            diffuse_mono(&mut self.mono, cw, ch, self.threshold as f32);
            for (cell, &v) in self.scratch.iter_mut().zip(&self.mono) {
                *cell = [v; 3];
            }
        }
        upsample(&self.scratch, cw, ps, rgba, width, height);
    }

    fn encode(
        &mut self,
        ctx: &PassCtx,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
    ) {
        // the seed stays an exact f32 integer in the shader
        self.frame = (self.frame + 1) % (1 << 20);
        self.uniform.write(ctx.queue, &self.uniform());
        let ps = self.pixel_size.max(1);
        self.cells.ensure(
            ctx.device,
            ctx.width.div_ceil(ps),
            ctx.height.div_ceil(ps),
            "dithering cells",
        );
        let input_group = self.input.get(ctx, input);
        draw_fullscreen(
            encoder,
            "dithering cells",
            &self.cells_pipeline,
            &[input_group, &self.uniform.group],
            &self.cells.view,
        );
        let cells_group = self.cells_input.get(ctx, &self.cells.view);
        draw_fullscreen(
            encoder,
            "dithering",
            &self.dither_pipeline,
            &[cells_group, &self.uniform.group],
            output,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    #[test]
    fn uniform_is_544_bytes_and_16_aligned() {
        assert_eq!(std::mem::size_of::<Uniform>(), 544);
        assert_eq!(std::mem::align_of::<Uniform>(), 16);
        assert_eq!(std::mem::offset_of!(Uniform, pixel_size), 0);
        assert_eq!(std::mem::offset_of!(Uniform, threshold), 16);
        assert_eq!(std::mem::offset_of!(Uniform, frame), 20);
        assert_eq!(std::mem::offset_of!(Uniform, palette), 32);
        assert_eq!(bytemuck::bytes_of(&Uniform::zeroed()).len(), 544);
    }

    /// 24 colours is black, white and 22 of the 25 non-corner entries
    /// of a 3x3x3 cube; 2 colours is just black and white
    #[test]
    fn palette_follows_the_cube_rule() {
        let p = palette(24);
        assert_eq!(p.len(), 24);
        assert_eq!(p[0], [0.0, 0.0, 0.0]);
        assert_eq!(p[1], [1.0, 1.0, 1.0]);
        assert_eq!(p[2], [0.0, 0.0, 0.5]);
        assert!(!p[2..].contains(&[1.0, 1.0, 1.0]));
        assert_eq!(palette(2).len(), 2);
        assert_eq!(palette(32).len(), 32);
        assert_eq!(palette(4).len(), 4);
    }

    /// a flat 50 % grey diffuses to a checkerboard-ish mix: about half
    /// the cells end up white, in mono and against the 2-colour palette
    #[test]
    fn floyd_steinberg_halves_mid_grey() {
        let mut mono = vec![0.5; 16 * 16];
        diffuse_mono(&mut mono, 16, 16, 255.0);
        let white = mono.iter().filter(|&&v| v > 0.5).count();
        assert!((100..=156).contains(&white), "{white} white cells");
        assert!(mono.iter().all(|&v| v == 0.0 || v == 1.0));

        let mut cells = vec![[0.5; 3]; 16 * 16];
        diffuse_colour(&mut cells, 16, 16, &palette(2));
        let white = cells.iter().filter(|c| c[0] > 0.5).count();
        assert!((100..=156).contains(&white), "{white} white cells");
    }

    /// a lower threshold brightens: at 128 every 50 % cell scales past
    /// 127 and quantises white, with no error to spread
    #[test]
    fn mono_threshold_scales_the_input() {
        let mut mono = vec![0.5; 8];
        diffuse_mono(&mut mono, 8, 1, 128.0);
        assert!(mono.iter().all(|&v| v == 1.0));
    }

    #[test]
    fn downsample_averages_a_two_by_two_block() {
        let rgba = vec![
            0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255,
        ];
        let mut cells = Vec::new();
        let (cw, ch) = downsample(&rgba, 2, 2, 2, &mut cells);
        assert_eq!((cw, ch), (1, 1));
        assert!((cells[0][0] - 0.5).abs() < 1e-6);
    }

    /// a 3 px wide frame with 2 px cells has a half-width right cell
    /// and the block copy stops at the frame edge
    #[test]
    fn upsample_fills_partial_cells() {
        let cells = [[0.0; 3], [1.0; 3], [1.0; 3], [0.0; 3]];
        let mut rgba = vec![7u8; 3 * 3 * 4];
        upsample(&cells, 2, 2, &mut rgba, 3, 3);
        let at = |x: usize, y: usize| rgba[(y * 3 + x) * 4];
        assert_eq!((at(0, 0), at(1, 0), at(2, 0)), (0, 0, 255));
        assert_eq!((at(0, 1), at(2, 1)), (0, 255));
        assert_eq!((at(0, 2), at(1, 2), at(2, 2)), (255, 255, 0));
        assert!(rgba.chunks_exact(4).all(|px| px[3] == 255));
    }

    fn count_black(pixels: &[u8], width: usize, x_range: std::ops::Range<usize>) -> usize {
        pixels
            .chunks_exact(4)
            .enumerate()
            .filter(|(i, px)| x_range.contains(&(i % width)) && px[0] < 128)
            .count()
    }

    /// black stays black and white stays white under every pattern,
    /// at the thresholds where the site's rules leave them alone
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_keeps_the_halves_apart() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        renderer.push_pass(Box::new(Dithering::new(&device, renderer.input_layout())));
        let frame = half_frame(64, 32);
        for (pattern, threshold, colour) in [
            (Pattern::FloydSteinberg, 255, false),
            (Pattern::Bayer, 128, false),
            (Pattern::Random, 100, false),
            (Pattern::FloydSteinberg, 255, true),
        ] {
            let mut d = Dithering::new(&device, renderer.input_layout());
            d.set_pattern(pattern);
            d.set_threshold(threshold);
            d.set_colour_mode(colour);
            renderer.passes_mut()[0].pass = Box::new(d);
            renderer.upload(&device, &queue, &frame);
            renderer.render(&device, &queue);
            let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
            let left = count_black(&pixels, 64, 0..32);
            let right = count_black(&pixels, 64, 32..64);
            assert_eq!(left, 32 * 32, "{pattern:?} colour {colour}: left half");
            assert_eq!(right, 0, "{pattern:?} colour {colour}: right half");
        }
    }
}

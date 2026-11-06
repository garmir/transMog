//! scatter: random points seeded by darkness, relaxed apart, drawn as
//! textured quads largest first on white. the renderer never encodes a
//! cpu pass, so the sprites are composited on the cpu into the frame
//! the renderer hands back; the quads are not instanced on the gpu.

use std::path::PathBuf;

use crate::fx::assets::{self, RgbaImage};
use crate::render::{Pass, PassCtx};

pub const DENSITY_RANGE: std::ops::RangeInclusive<f32> = 0.0..=0.2;
pub const SIZE_RANGE: std::ops::RangeInclusive<u32> = 1..=50;
pub const ITERATIONS_RANGE: std::ops::RangeInclusive<u32> = 0..=20;
pub const STRENGTH_RANGE: std::ops::RangeInclusive<f32> = 0.0..=1.0;

const DEFAULT_DENSITY: f32 = 0.004;
const DEFAULT_MIN_SIZE: u32 = 4;
const DEFAULT_MAX_SIZE: u32 = 14;
const DEFAULT_ITERATIONS: u32 = 1;
const DEFAULT_STRENGTH: f32 = 0.16;
/// above this the density is scaled down so coverage stays even
/// instead of the spawn stopping part way down the frame
pub const MAX_POINTS: usize = 60_000;
const DISC_SIZE: u32 = 64;
/// sprites are cached per whole-pixel size up to the size slider's top
const MAX_SPRITE: usize = 50;

pub fn textures_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/transmog/scatter"))
}

#[derive(Clone, Copy, Debug)]
struct Point {
    x: f32,
    y: f32,
    size: f32,
    texture: u32,
}

/// xorshift32, re-seeded per frame
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Rng {
        Rng(seed.wrapping_mul(0x9e3779b9) | 1)
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

pub struct Scatter {
    enabled: bool,
    density: f32,
    min_size: u32,
    max_size: u32,
    iterations: u32,
    strength: f32,
    textures: Vec<RgbaImage>,
    /// [texture][size] sprites, filled as sizes are first drawn
    sprites: Vec<Vec<Option<RgbaImage>>>,
    user_textures: usize,
    frame: u32,
    points: Vec<Point>,
    /// spatial hash scratch: cell of each point, cell start offsets and
    /// the point indices sorted by cell
    cells: Vec<u32>,
    offsets: Vec<u32>,
    order: Vec<u32>,
}

impl Scatter {
    pub fn new(_device: &wgpu::Device, _input_layout: &wgpu::BindGroupLayout) -> Scatter {
        let mut scatter = Scatter {
            enabled: true,
            density: DEFAULT_DENSITY,
            min_size: DEFAULT_MIN_SIZE,
            max_size: DEFAULT_MAX_SIZE,
            iterations: DEFAULT_ITERATIONS,
            strength: DEFAULT_STRENGTH,
            textures: Vec::new(),
            sprites: Vec::new(),
            user_textures: 0,
            frame: 0,
            points: Vec::new(),
            cells: Vec::new(),
            offsets: Vec::new(),
            order: Vec::new(),
        };
        scatter.load_textures();
        scatter
    }

    pub fn density(&self) -> f32 {
        self.density
    }

    pub fn set_density(&mut self, density: f32) {
        self.density = density.clamp(*DENSITY_RANGE.start(), *DENSITY_RANGE.end());
    }

    /// min and max dot size in pixels
    pub fn sizes(&self) -> (u32, u32) {
        (self.min_size, self.max_size)
    }

    pub fn set_sizes(&mut self, min: u32, max: u32) {
        self.min_size = min.clamp(*SIZE_RANGE.start(), *SIZE_RANGE.end());
        self.max_size = max.clamp(*SIZE_RANGE.start(), *SIZE_RANGE.end());
    }

    pub fn relaxation(&self) -> (u32, f32) {
        (self.iterations, self.strength)
    }

    pub fn set_relaxation(&mut self, iterations: u32, strength: f32) {
        self.iterations = iterations.clamp(*ITERATIONS_RANGE.start(), *ITERATIONS_RANGE.end());
        self.strength = strength.clamp(*STRENGTH_RANGE.start(), *STRENGTH_RANGE.end());
    }

    pub fn reset(&mut self) {
        self.density = DEFAULT_DENSITY;
        self.min_size = DEFAULT_MIN_SIZE;
        self.max_size = DEFAULT_MAX_SIZE;
        self.iterations = DEFAULT_ITERATIONS;
        self.strength = DEFAULT_STRENGTH;
    }

    /// points drawn by the last frame
    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// the pngs in ~/.config/transmog/scatter/ when there are any,
    /// otherwise the built-in disc
    fn load_textures(&mut self) {
        let user = textures_dir()
            .map(|dir| assets::load_pngs(&dir))
            .unwrap_or_default();
        let user: Vec<RgbaImage> = user
            .into_iter()
            .filter(|t| t.width > 0 && t.height > 0)
            .collect();
        self.user_textures = user.len();
        self.textures = if user.is_empty() {
            vec![assets::disc(DISC_SIZE)]
        } else {
            user
        };
        self.sprites = vec![vec![None; MAX_SPRITE + 1]; self.textures.len()];
    }

    /// one point per pixel with probability (255 - l) / 255 * density,
    /// sized remap(l, 0, 255, max, min); past MAX_POINTS the list is
    /// thinned evenly so coverage stays uniform instead of the spawn
    /// stopping part way down the frame. the probability is a table
    /// over the pixel's darkness 0..765 compared against the raw rng
    /// word, so the per-pixel loop has no float work
    fn spawn(&mut self, rgba: &[u8], width: u32) {
        self.points.clear();
        let density = f64::from(self.density);
        let threshold: Vec<u32> = (0..=765u32)
            .map(|d| (f64::from(d) / 765.0 * density * 4_294_967_296.0).min(4_294_967_295.0) as u32)
            .collect();
        let (min, max) = (self.min_size as f32, self.max_size as f32);
        let mut rng = Rng::new(self.frame);
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            let d = darkness(px);
            if rng.next_u32() < threshold[d as usize] {
                self.points.push(Point {
                    x: (i as u32 % width) as f32 + 0.5,
                    y: (i as u32 / width) as f32 + 0.5,
                    size: max + (min - max) * (765 - d) as f32 / 765.0,
                    texture: 0,
                });
            }
        }
        let n = self.points.len();
        if n > MAX_POINTS {
            for k in 0..MAX_POINTS {
                self.points[k] = self.points[k * n / MAX_POINTS];
            }
            self.points.truncate(MAX_POINTS);
        }
        let textures = self.textures.len() as u32;
        for (i, p) in self.points.iter_mut().enumerate() {
            p.texture = i as u32 % textures;
        }
    }

    /// pairs closer than the mean of their sizes move apart by
    /// (o - d) / d * strength * delta, half each; neighbours come from
    /// a grid of max(max_size, 20) px cells rebuilt every iteration
    fn relax(&mut self, width: u32, height: u32) {
        if self.iterations == 0 || self.strength <= 0.0 || self.points.len() < 2 {
            return;
        }
        let cell = self.max_size.max(20) as f32;
        let cols = (width as f32 / cell).ceil().max(1.0) as usize;
        let rows = (height as f32 / cell).ceil().max(1.0) as usize;
        let n = self.points.len();
        for _ in 0..self.iterations {
            self.cells.clear();
            self.cells.extend(self.points.iter().map(|p| {
                let cx = ((p.x / cell).floor().max(0.0) as usize).min(cols - 1);
                let cy = ((p.y / cell).floor().max(0.0) as usize).min(rows - 1);
                (cy * cols + cx) as u32
            }));
            self.offsets.clear();
            self.offsets.resize(cols * rows + 1, 0);
            for &c in &self.cells {
                self.offsets[c as usize + 1] += 1;
            }
            for i in 1..self.offsets.len() {
                self.offsets[i] += self.offsets[i - 1];
            }
            self.order.clear();
            self.order.resize(n, 0);
            let mut fill = self.offsets.clone();
            for (i, &c) in self.cells.iter().enumerate() {
                let slot = &mut fill[c as usize];
                self.order[*slot as usize] = i as u32;
                *slot += 1;
            }
            for i in 0..n {
                let c = self.cells[i] as usize;
                let (cx, cy) = (c % cols, c / cols);
                for ny in cy.saturating_sub(1)..=(cy + 1).min(rows - 1) {
                    for nx in cx.saturating_sub(1)..=(cx + 1).min(cols - 1) {
                        let nc = ny * cols + nx;
                        let range = self.offsets[nc] as usize..self.offsets[nc + 1] as usize;
                        for &j in &self.order[range] {
                            let j = j as usize;
                            if j <= i {
                                continue;
                            }
                            let (a, b) = (self.points[i], self.points[j]);
                            let dx = b.x - a.x;
                            let dy = b.y - a.y;
                            let d = (dx * dx + dy * dy).sqrt();
                            let o = (a.size + b.size) * 0.5;
                            if d >= o || d < 1e-4 {
                                continue;
                            }
                            let k = (o - d) / d * self.strength * 0.5;
                            self.points[i].x -= dx * k;
                            self.points[i].y -= dy * k;
                            self.points[j].x += dx * k;
                            self.points[j].y += dy * k;
                        }
                    }
                }
            }
        }
    }

    /// composites every point's sprite over the white frame, largest
    /// first; a sprite is the texture box-filtered to the point's size
    /// rounded to whole pixels, cached per texture and size
    fn draw(&mut self, rgba: &mut [u8], width: u32, height: u32) {
        rgba.fill(255);
        self.points
            .sort_unstable_by(|a, b| b.size.total_cmp(&a.size));
        let (w, h) = (width as i32, height as i32);
        for p in &self.points {
            let t = p.texture as usize % self.textures.len();
            let size = (p.size.round() as usize).clamp(1, MAX_SPRITE);
            let sprite =
                self.sprites[t][size].get_or_insert_with(|| scaled(&self.textures[t], size as u32));
            let x0 = (p.x - size as f32 * 0.5).round() as i32;
            let y0 = (p.y - size as f32 * 0.5).round() as i32;
            let sw = sprite.width as i32;
            let (cx0, cy0) = (x0.max(0), y0.max(0));
            let (cx1, cy1) = ((x0 + sw).min(w), (y0 + sprite.height as i32).min(h));
            for y in cy0..cy1 {
                let src_row = ((y - y0) * sw) as usize;
                let dst_row = (y * w) as usize;
                for x in cx0..cx1 {
                    let s = &sprite.data[(src_row + (x - x0) as usize) * 4..][..4];
                    let a = u32::from(s[3]);
                    if a == 0 {
                        continue;
                    }
                    let dst = &mut rgba[(dst_row + x as usize) * 4..][..4];
                    if a == 255 {
                        dst[..3].copy_from_slice(&s[..3]);
                    } else {
                        for (d, &c) in dst[..3].iter_mut().zip(&s[..3]) {
                            *d = ((u32::from(c) * a + u32::from(*d) * (255 - a) + 127) / 255) as u8;
                        }
                    }
                    dst[3] = 255;
                }
            }
        }
    }
}

/// the site's 255 - luminance of one rgba pixel over white, as an
/// integer 0..765 (three times the 0..255 value)
fn darkness(px: &[u8]) -> u32 {
    let a = u32::from(px[3]);
    let over = |c: u8| {
        if a == 255 {
            u32::from(c)
        } else {
            (u32::from(c) * a + 255 * (255 - a) + 127) / 255
        }
    };
    765 - (over(px[0]) + over(px[1]) + over(px[2]))
}

/// the texture resampled to size x size with a box filter, rgb
/// averaged by alpha so clear texels do not bleed their colour
fn scaled(tex: &RgbaImage, size: u32) -> RgbaImage {
    let (tw, th) = (tex.width as usize, tex.height as usize);
    let n = size as usize;
    let mut data = Vec::with_capacity(n * n * 4);
    let span = |i: usize, len: usize| {
        let a = i * len / n;
        let b = ((i + 1) * len / n).max(a + 1).min(len);
        (a, b)
    };
    for dy in 0..n {
        let (y0, y1) = span(dy, th);
        for dx in 0..n {
            let (x0, x1) = span(dx, tw);
            let (mut sr, mut sg, mut sb, mut sa, mut count) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                for x in x0..x1 {
                    let px = &tex.data[(y * tw + x) * 4..][..4];
                    let a = u64::from(px[3]);
                    sr += u64::from(px[0]) * a;
                    sg += u64::from(px[1]) * a;
                    sb += u64::from(px[2]) * a;
                    sa += a;
                    count += 1;
                }
            }
            let alpha = ((sa + count / 2) / count) as u8;
            if alpha == 0 {
                data.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let avg = |sum: u64| ((sum + sa / 2) / sa) as u8;
                data.extend_from_slice(&[avg(sr), avg(sg), avg(sb), alpha]);
            }
        }
    }
    RgbaImage {
        width: size,
        height: size,
        data,
    }
}

impl Pass for Scatter {
    fn name(&self) -> &'static str {
        "scatter"
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::Slider::new(&mut self.density, DENSITY_RANGE)
                .step_by(0.01)
                .fixed_decimals(3)
                .text("density"),
        );
        ui.add(
            egui::Slider::new(&mut self.min_size, SIZE_RANGE)
                .suffix(" px")
                .text("min size"),
        );
        ui.add(
            egui::Slider::new(&mut self.max_size, SIZE_RANGE)
                .suffix(" px")
                .text("max size"),
        );
        ui.add(egui::Slider::new(&mut self.iterations, ITERATIONS_RANGE).text("relax iterations"));
        ui.add(egui::Slider::new(&mut self.strength, STRENGTH_RANGE).text("relax strength"));
        ui.horizontal(|ui| {
            if self.user_textures == 0 {
                ui.weak("texture: disc");
            } else {
                ui.weak(format!("textures: {} png", self.user_textures));
            }
            if ui.small_button("reload").clicked() {
                self.load_textures();
            }
        });
        ui.weak(format!("{} points", self.points.len()));
        if ui.button("reset").clicked() {
            self.reset();
        }
    }

    fn cpu(&self) -> bool {
        true
    }

    fn process_cpu(&mut self, rgba: &mut Vec<u8>, width: u32, height: u32) {
        let len = (width * height * 4) as usize;
        if rgba.len() < len || width == 0 || height == 0 {
            return;
        }
        self.frame = self.frame.wrapping_add(1);
        let pixels = &mut rgba[..len];
        self.spawn(pixels, width);
        self.relax(width, height);
        self.draw(pixels, width, height);
    }

    /// the renderer routes a cpu pass through process_cpu and never
    /// calls this
    fn encode(
        &mut self,
        _ctx: &PassCtx,
        _encoder: &mut wgpu::CommandEncoder,
        _input: &wgpu::TextureView,
        _output: &wgpu::TextureView,
    ) {
        tracing::warn!("scatter is a cpu pass; encode does nothing");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tests::half_frame;

    fn bare() -> Scatter {
        Scatter {
            enabled: true,
            density: DEFAULT_DENSITY,
            min_size: DEFAULT_MIN_SIZE,
            max_size: DEFAULT_MAX_SIZE,
            iterations: DEFAULT_ITERATIONS,
            strength: DEFAULT_STRENGTH,
            textures: vec![assets::disc(DISC_SIZE)],
            sprites: vec![vec![None; MAX_SPRITE + 1]],
            user_textures: 0,
            frame: 0,
            points: Vec::new(),
            cells: Vec::new(),
            offsets: Vec::new(),
            order: Vec::new(),
        }
    }

    /// a black half and a white half as tightly packed rgba
    fn half_rgba(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..height {
            for x in 0..width {
                let v = if x < width / 2 { 0 } else { 255 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        rgba
    }

    #[test]
    fn white_pixels_never_spawn_and_black_ones_get_the_max_size() {
        let mut s = bare();
        s.set_density(0.2);
        let rgba = half_rgba(64, 32);
        s.spawn(&rgba, 64);
        assert!(s.points.len() > 200, "{} points", s.points.len());
        for p in &s.points {
            assert!(p.x < 32.0, "a white pixel spawned at {}", p.x);
            assert_eq!(p.size, 14.0);
        }
    }

    #[test]
    fn the_cap_thins_evenly_instead_of_truncating() {
        let mut s = bare();
        s.set_density(0.2);
        let rgba = [0u8, 0, 0, 255].repeat(1000 * 400);
        s.spawn(&rgba, 1000);
        assert_eq!(s.points.len(), MAX_POINTS);
        let last = s.points.last().unwrap();
        assert!(last.y > 390.0, "spawn stopped at row {}", last.y);
        assert!(s.points.iter().any(|p| p.texture == 0));
    }

    #[test]
    fn relaxation_pushes_an_overlapping_pair_apart() {
        let mut s = bare();
        s.set_relaxation(1, 1.0);
        s.points = vec![
            Point {
                x: 10.0,
                y: 10.0,
                size: 10.0,
                texture: 0,
            },
            Point {
                x: 12.0,
                y: 10.0,
                size: 10.0,
                texture: 0,
            },
        ];
        s.relax(64, 32);
        let d = s.points[1].x - s.points[0].x;
        assert!((d - 10.0).abs() < 1e-3, "distance {d}");
        assert_eq!(s.points[0].y, 10.0);
    }

    #[test]
    fn the_disc_draws_black_at_its_centre_and_leaves_the_corner() {
        let mut s = bare();
        s.points = vec![Point {
            x: 16.0,
            y: 16.0,
            size: 20.0,
            texture: 0,
        }];
        let mut rgba = vec![0; 32 * 32 * 4];
        s.draw(&mut rgba, 32, 32);
        let at = |x: usize, y: usize| rgba[(y * 32 + x) * 4];
        assert_eq!(at(16, 16), 0);
        assert_eq!(at(0, 0), 255);
        assert_eq!(at(7, 7), 255, "outside the disc inside the quad");
    }

    /// through the renderer's read-back path: the black half fills with
    /// dots, the far side of the white half stays untouched since the
    /// widest unrelaxed dot reaches 7 px past the edge
    #[test]
    #[ignore = "needs a gpu"]
    fn gpu_round_trip_dots_the_dark_half() {
        use crate::render::tests::{headless_device, read_back};
        use crate::render::Renderer;

        let (device, queue) = headless_device().expect("no gpu adapter");
        let mut renderer = Renderer::new(&device, &queue);
        let mut scatter = Scatter::new(&device, renderer.input_layout());
        scatter.set_density(0.2);
        // 400 overlapping dots relaxed would spill far past the edge
        scatter.set_relaxation(0, 0.0);
        renderer.push_pass(Box::new(scatter));
        renderer.upload(&device, &queue, &half_frame(64, 32));
        renderer.render(&device, &queue);
        let pixels = read_back(&device, &queue, renderer.output_texture().unwrap(), 64, 32);
        let (mut dark_left, mut dark_right) = (0, 0);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            let x = i % 64;
            if x >= 48 {
                assert_eq!(px[0], 255, "pixel {i} dotted on the white side");
            }
            if px[0] < 128 {
                if x < 32 {
                    dark_left += 1;
                } else {
                    dark_right += 1;
                }
            }
        }
        assert!(
            dark_left > 32 * 32 / 4,
            "dark pixels on the left: {dark_left}"
        );
        assert!(dark_left > dark_right);
    }
}

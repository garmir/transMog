//! procedural textures the effects start from: the default pattern
//! tiles, the scatter disc, a noise map for distort, png loading for
//! user tiles and the ascii glyph atlas

use std::path::Path;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use anyhow::{Context, Result};

/// one byte per pixel, rows top first, 255 white
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrayImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl GrayImage {
    pub fn new(width: u32, height: u32, fill: u8) -> GrayImage {
        GrayImage {
            width,
            height,
            data: vec![fill; (width * height) as usize],
        }
    }

    pub fn from_fn(width: u32, height: u32, f: impl Fn(u32, u32) -> u8) -> GrayImage {
        let mut data = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                data.push(f(x, y));
            }
        }
        GrayImage {
            width,
            height,
            data,
        }
    }

    /// the site's tile brightness: mean of sqrt(.299 r² + .587 g² + .114 b²),
    /// which for grey is the value itself
    pub fn mean(&self) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        self.data.iter().map(|&v| f32::from(v)).sum::<f32>() / self.data.len() as f32
    }

    /// the image as rgba8 with full alpha
    pub fn to_rgba(&self) -> RgbaImage {
        let mut data = Vec::with_capacity(self.data.len() * 4);
        for &v in &self.data {
            data.extend_from_slice(&[v, v, v, 255]);
        }
        RgbaImage {
            width: self.width,
            height: self.height,
            data,
        }
    }
}

/// four bytes per pixel, rows top first, straight alpha
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// mean of sqrt(.299 r² + .587 g² + .114 b²) after compositing over
    /// white, the order the site sorts tiles by
    pub fn mean_brightness(&self) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        let sum: f32 = self
            .data
            .chunks_exact(4)
            .map(|px| {
                let a = f32::from(px[3]) / 255.0;
                let over = |c: u8| f32::from(c) * a + 255.0 * (1.0 - a);
                let (r, g, b) = (over(px[0]), over(px[1]), over(px[2]));
                (0.299 * r * r + 0.587 * g * g + 0.114 * b * b).sqrt()
            })
            .sum();
        sum / (self.data.len() / 4) as f32
    }
}

pub const TILE_SIZE: u32 = 32;

/// six 32x32 tiles, black ink on white, sorted by mean brightness so
/// index 0 is the darkest: solid, crosshatch, diagonal lines, dots,
/// checker and horizontal lines in whatever order their ink coverage puts
/// them
pub fn default_tiles() -> Vec<GrayImage> {
    let n = TILE_SIZE;
    let ink = |on: bool| if on { 0 } else { 255 };
    let mut tiles = vec![
        GrayImage::new(n, n, 0),
        GrayImage::from_fn(n, n, |x, y| ink((x + y) % 8 < 2)),
        GrayImage::from_fn(n, n, |x, y| ink((x + y) % 8 < 2 || (x + n - y) % 8 < 2)),
        GrayImage::from_fn(n, n, |x, y| {
            let (dx, dy) = ((x % 8) as i32 - 4, (y % 8) as i32 - 4);
            ink(dx * dx + dy * dy <= 4)
        }),
        GrayImage::from_fn(n, n, |x, y| ink((x / 4 + y / 4) % 2 == 0)),
        GrayImage::from_fn(n, n, |_, y| ink(y % 8 < 2)),
    ];
    tiles.sort_by(|a, b| a.mean().total_cmp(&b.mean()));
    tiles
}

/// an antialiased black disc on transparent, `size` px across
pub fn disc(size: u32) -> RgbaImage {
    let r = size as f32 / 2.0;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - r).powi(2) + (y as f32 + 0.5 - r).powi(2)).sqrt();
            let coverage = (r - d + 0.5).clamp(0.0, 1.0);
            data.extend_from_slice(&[0, 0, 0, (coverage * 255.0).round() as u8]);
        }
    }
    RgbaImage {
        width: size,
        height: size,
        data,
    }
}

fn hash_u32(x: u32) -> u32 {
    let mut h = x;
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846ca68b);
    h ^= h >> 16;
    h
}

fn gradient(ix: i32, iy: i32) -> (f32, f32) {
    let h = hash_u32((ix as u32) ^ hash_u32((iy as u32).wrapping_add(0x9e3779b9)));
    let a = h as f32 / 4294967296.0 * std::f32::consts::TAU;
    (a.cos(), a.sin())
}

fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// gradient noise in 0..1, 0.5 on the lattice, the cpu twin of perlin2
/// in common.wgsl (same shape, not the same values)
pub fn perlin2(x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (x - ix, y - iy);
    let (ix, iy) = (ix as i32, iy as i32);
    let dot = |gx: i32, gy: i32, dx: f32, dy: f32| {
        let (gx, gy) = gradient(gx, gy);
        gx * dx + gy * dy
    };
    let n00 = dot(ix, iy, fx, fy);
    let n10 = dot(ix + 1, iy, fx - 1.0, fy);
    let n01 = dot(ix, iy + 1, fx, fy - 1.0);
    let n11 = dot(ix + 1, iy + 1, fx - 1.0, fy - 1.0);
    let (ux, uy) = (fade(fx), fade(fy));
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let n = lerp(lerp(n00, n10, ux), lerp(n01, n11, ux), uy);
    (0.5 + n * std::f32::consts::FRAC_1_SQRT_2).clamp(0.0, 1.0)
}

/// the default distortion map: three octaves of perlin noise with a
/// base feature of a sixth of the shorter side, in the red channel and
/// repeated into green and blue
pub fn noise_map(width: u32, height: u32) -> RgbaImage {
    let base = (width.min(height) as f32 / 6.0).max(1.0);
    GrayImage::from_fn(width, height, |x, y| {
        let mut v = 0.0;
        let mut amp = 0.5;
        let mut freq = 1.0 / base;
        for _ in 0..3 {
            v += (perlin2(x as f32 * freq, y as f32 * freq) - 0.5) * amp;
            amp *= 0.5;
            freq *= 2.0;
        }
        ((0.5 + v).clamp(0.0, 1.0) * 255.0).round() as u8
    })
    .to_rgba()
}

/// decodes one png into straight rgba8
pub fn load_png(path: &Path) -> Result<RgbaImage> {
    let file = std::fs::File::open(path).context("open")?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().context("read header")?;
    let mut buf = vec![0; reader.output_buffer_size().context("size")?];
    let info = reader.next_frame(&mut buf).context("decode")?;
    let (width, height) = (info.width, info.height);
    let bytes = &buf[..info.buffer_size()];
    let data = match info.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .flat_map(|px| [px[0], px[1], px[2], 255])
            .collect(),
        png::ColorType::Grayscale => bytes.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .chunks_exact(2)
            .flat_map(|px| [px[0], px[0], px[0], px[1]])
            .collect(),
        other => anyhow::bail!("unsupported colour type {other:?}"),
    };
    Ok(RgbaImage {
        width,
        height,
        data,
    })
}

/// every *.png in `dir` in name order; a missing directory is an empty
/// set and a bad file is logged and skipped
pub fn load_pngs(dir: &Path) -> Vec<RgbaImage> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|path| match load_png(path) {
            Ok(image) => Some(image),
            Err(e) => {
                tracing::warn!("skip {}: {e:#}", path.display());
                None
            }
        })
        .collect()
}

/// black glyphs on white in one row of cells, lightest glyph first
pub struct Atlas {
    pub width: u32,
    pub height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    /// (char, x offset of its cell) in darkness order
    pub glyphs: Vec<(char, u32)>,
    /// one byte per pixel, 255 white
    pub data: Vec<u8>,
}

impl Atlas {
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// the characters in darkness order
    pub fn ranked(&self) -> String {
        self.glyphs.iter().map(|(c, _)| *c).collect()
    }
}

/// egui's bundled monospace face
fn mono_font_bytes() -> Vec<u8> {
    let defs = egui::FontDefinitions::default();
    let data = defs
        .font_data
        .get("Hack")
        .expect("egui ships Hack with default_fonts");
    data.font.as_ref().as_ref().to_vec()
}

/// rasterises every char of `charset` at `cell` px in a monospace
/// cell and ranks them by mean darkness ascending; duplicates are kept
pub fn glyph_atlas(charset: &str, cell: u32) -> Atlas {
    let cell = cell.max(4);
    let bytes = mono_font_bytes();
    let font = FontRef::try_from_slice(&bytes).expect("Hack parses");
    let scale = PxScale::from(cell as f32);
    let scaled = font.as_scaled(scale);
    let cell_width = scaled.h_advance(font.glyph_id('M')).ceil().max(1.0) as u32;
    let cell_height = cell;
    let ascent = scaled.ascent();

    let chars: Vec<char> = charset.chars().collect();
    let mut cells: Vec<(char, GrayImage)> = chars
        .iter()
        .map(|&c| {
            let mut image = GrayImage::new(cell_width, cell_height, 255);
            let glyph = font
                .glyph_id(c)
                .with_scale_and_position(scale, ab_glyph::point(0.0, ascent));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|x, y, v| {
                    let px = bounds.min.x as i32 + x as i32;
                    let py = bounds.min.y as i32 + y as i32;
                    if px < 0 || py < 0 || px >= cell_width as i32 || py >= cell_height as i32 {
                        return;
                    }
                    let i = (py as u32 * cell_width + px as u32) as usize;
                    let ink = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                    image.data[i] = image.data[i].min(255 - ink);
                });
            }
            (c, image)
        })
        .collect();
    // darkness ascending is brightness descending
    cells.sort_by(|a, b| b.1.mean().total_cmp(&a.1.mean()));

    let width = cell_width * cells.len().max(1) as u32;
    let mut data = vec![255u8; (width * cell_height) as usize];
    let mut glyphs = Vec::with_capacity(cells.len());
    for (i, (c, image)) in cells.iter().enumerate() {
        let x0 = i as u32 * cell_width;
        for y in 0..cell_height {
            let src = &image.data[(y * cell_width) as usize..((y + 1) * cell_width) as usize];
            let dst = (y * width + x0) as usize;
            data[dst..dst + cell_width as usize].copy_from_slice(src);
        }
        glyphs.push((*c, x0));
    }
    Atlas {
        width,
        height: cell_height,
        cell_width,
        cell_height,
        glyphs,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tiles_are_six_32px_tiles_dark_first() {
        let tiles = default_tiles();
        assert_eq!(tiles.len(), 6);
        for t in &tiles {
            assert_eq!((t.width, t.height), (TILE_SIZE, TILE_SIZE));
        }
        assert_eq!(tiles[0].mean(), 0.0, "solid black sorts first");
        for pair in tiles.windows(2) {
            assert!(pair[0].mean() <= pair[1].mean());
        }
        assert!(tiles[5].mean() < 255.0, "no tile is blank");
    }

    #[test]
    fn disc_is_opaque_at_the_centre_and_clear_at_the_corner() {
        let d = disc(16);
        assert_eq!((d.width, d.height), (16, 16));
        let at = |x: u32, y: u32| d.data[((y * 16 + x) * 4 + 3) as usize];
        assert_eq!(at(8, 8), 255);
        assert_eq!(at(0, 0), 0);
        assert_eq!(d.data[0..3], [0, 0, 0]);
    }

    #[test]
    fn perlin_is_half_on_the_lattice_and_bounded() {
        assert!((perlin2(3.0, 7.0) - 0.5).abs() < 1e-6);
        for i in 0..200 {
            let v = perlin2(i as f32 * 0.37, i as f32 * 0.11);
            assert!((0.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn noise_map_has_the_requested_size_and_grey_channels() {
        let m = noise_map(64, 48);
        assert_eq!((m.width, m.height), (64, 48));
        assert_eq!(m.data.len(), 64 * 48 * 4);
        assert!(m
            .data
            .chunks_exact(4)
            .all(|px| px[0] == px[1] && px[1] == px[2]));
        let (min, max) = m
            .data
            .chunks_exact(4)
            .fold((255, 0), |(lo, hi), px| (lo.min(px[0]), hi.max(px[0])));
        assert!(max - min > 30, "flat noise map: {min}..{max}");
    }

    #[test]
    fn load_pngs_on_a_missing_dir_is_empty() {
        assert!(load_pngs(Path::new("/nonexistent/transmog-tiles")).is_empty());
    }

    #[test]
    fn rgba_brightness_composites_over_white() {
        let clear = RgbaImage {
            width: 1,
            height: 1,
            data: vec![0, 0, 0, 0],
        };
        let black = RgbaImage {
            width: 1,
            height: 1,
            data: vec![0, 0, 0, 255],
        };
        assert!((clear.mean_brightness() - 255.0).abs() < 1e-3);
        assert_eq!(black.mean_brightness(), 0.0);
    }

    #[test]
    fn atlas_ranks_space_first_and_at_last() {
        let atlas = glyph_atlas(" .:-=+*#%@", 16);
        assert_eq!(atlas.len(), 10);
        assert_eq!(atlas.height, 16);
        assert_eq!(atlas.width, atlas.cell_width * 10);
        assert_eq!(atlas.data.len(), (atlas.width * atlas.height) as usize);
        assert_eq!(atlas.glyphs[0].0, ' ');
        assert_eq!(atlas.glyphs[9].0, '@');
        for (i, (_, x)) in atlas.glyphs.iter().enumerate() {
            assert_eq!(*x, i as u32 * atlas.cell_width);
        }
        // a cell's mean darkness never decreases along the row
        let mean = |x0: u32| {
            let mut sum = 0u32;
            for y in 0..atlas.height {
                for x in x0..x0 + atlas.cell_width {
                    sum += u32::from(atlas.data[(y * atlas.width + x) as usize]);
                }
            }
            sum
        };
        for pair in atlas.glyphs.windows(2) {
            assert!(mean(pair[0].1) >= mean(pair[1].1));
        }
    }
}

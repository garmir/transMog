use egui::pos2;

use super::map::ScreenMap;
use crate::aim::luma::Sample;
use crate::aim::marks::{self, Rect, FIDS, RULER, STAMPS, STAMP_QUIET};

/// a part of one screen in frame pixels, resampled at `scale` pixels a
/// frame pixel, one byte of grey a pixel
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub name: &'static str,
    /// the frame rectangle the tile covers
    pub rect: Rect,
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Tile {
    pub fn scale(&self) -> f32 {
        self.width as f32 / self.rect.width()
    }

    /// the mean grey of a frame rectangle that lies inside the tile,
    /// over the whole pixels it covers
    pub fn level(&self, r: Rect) -> Option<f32> {
        let s = self.scale();
        let x0 = ((r.x0 - self.rect.x0) * s).round().max(0.0) as usize;
        let y0 = ((r.y0 - self.rect.y0) * s).round().max(0.0) as usize;
        let x1 = (((r.x1 - self.rect.x0) * s).round() as usize).min(self.width);
        let y1 = (((r.y1 - self.rect.y0) * s).round() as usize).min(self.height);
        let inside = r.x0 >= self.rect.x0
            && r.y0 >= self.rect.y0
            && r.x1 <= self.rect.x1
            && r.y1 <= self.rect.y1;
        let (x1, y1) = (
            x1.max(x0 + 1).min(self.width),
            y1.max(y0 + 1).min(self.height),
        );
        if !inside || x1 <= x0 || y1 <= y0 {
            return None;
        }
        let mut sum = 0u32;
        for y in y0..y1 {
            sum += self.data[y * self.width + x0..y * self.width + x1]
                .iter()
                .map(|&v| u32::from(v))
                .sum::<u32>();
        }
        Some(sum as f32 / ((x1 - x0) * (y1 - y0)) as f32)
    }
}

/// the parts of a screen the decoders read, each with its quiet border:
/// the ruler's row 3 (its left 512 px), the stamps, the toggles and
/// fiducial 0, which tells the marks layout from the old one
pub fn regions() -> Vec<(&'static str, Rect)> {
    let pad = |(r, q): (Rect, f32)| r.grow(q);
    vec![
        (
            "ruler",
            Rect {
                x0: 0.0,
                y0: RULER.y0 - 8.0,
                x1: 520.0,
                y1: RULER.y1 + 8.0,
            },
        ),
        ("stamps", STAMPS.grow(STAMP_QUIET)),
        ("key", pad(marks::KEY)),
        ("mid", pad(marks::MID)),
        ("bottom", pad(marks::BOTTOM)),
        ("fid0", FIDS[0].rect),
    ]
}

/// most taps a side per output pixel when the camera sees the screen
/// finer than the tile samples it
const MAX_TAPS: usize = 4;

/// `rect` of the screen resampled through `map` at `scale` tile pixels a
/// frame pixel. each tile pixel is the mean of n x n bilinear taps, n
/// the camera pixels one tile pixel spans (1 when the camera sees the
/// screen coarser than the tile, as the mini at 0.13 camera px a frame
/// px always is). outside the camera picture reads 0
pub fn warp<S: Sample>(s: &S, map: &ScreenMap, name: &'static str, rect: Rect, scale: f32) -> Tile {
    let width = (rect.width() * scale).round().max(1.0) as usize;
    let height = (rect.height() * scale).round().max(1.0) as usize;
    let (sx, sy) = map.scales_at(rect.centre());
    let span = sx.max(sy) / scale;
    let n = (span.ceil() as usize).clamp(1, MAX_TAPS);
    let step = 1.0 / (scale * n as f32);
    let mut data = Vec::with_capacity(width * height);
    let h = map.h.0;
    let lens = map.lens;
    let plain = lens.is_none();
    for j in 0..height {
        for i in 0..width {
            let mut sum = 0.0;
            for b in 0..n {
                let fy = rect.y0 + (j as f32 + (b as f32 + 0.5) / n as f32) / scale;
                for a in 0..n {
                    let fx = rect.x0 + i as f32 / scale + (a as f32 + 0.5) * step;
                    let (x, y) = (f64::from(fx), f64::from(fy));
                    let w = h[6] * x + h[7] * y + h[8];
                    let p = pos2(
                        ((h[0] * x + h[1] * y + h[2]) / w) as f32,
                        ((h[3] * x + h[4] * y + h[5]) / w) as f32,
                    );
                    let p = if plain { p } else { lens.distort(p) };
                    sum += s.at(p.x, p.y).unwrap_or(0.0);
                }
            }
            data.push((sum / (n * n) as f32).round().clamp(0.0, 255.0) as u8);
        }
    }
    Tile {
        name,
        rect,
        width,
        height,
        data,
    }
}

/// the mean grey of a frame rectangle from whichever tile holds it
pub fn level(tiles: &[Tile], r: Rect) -> Option<f32> {
    tiles.iter().find_map(|t| t.level(r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aim::geom::Homography;
    use crate::aim::luma::Luma;
    use crate::rect::map::Lens;

    #[test]
    fn identity_warp_copies_the_picture() {
        let mut l = Luma::new(64, 32, 0);
        for (i, v) in l.data.iter_mut().enumerate() {
            *v = (i % 64) as u8 * 3;
        }
        let map = ScreenMap {
            h: Homography::IDENTITY,
            lens: Lens::none((64, 32)),
        };
        let t = warp(&l, &map, "t", Rect::new(8.0, 4.0, 32.0, 16.0), 1.0);
        assert_eq!((t.width, t.height), (32, 16));
        assert_eq!(t.data[0], 24);
        assert_eq!(t.data[31], (8 + 31) * 3);
        assert_eq!(t.level(Rect::new(8.0, 4.0, 1.0, 16.0)), Some(24.0));
        assert_eq!(t.level(Rect::new(0.0, 0.0, 4.0, 4.0)), None);
    }

    #[test]
    fn a_coarse_tile_averages_the_camera_pixels() {
        // stripes one camera pixel wide, sampled at a quarter scale: one
        // tap a pixel would alias to all black or all white
        let mut l = Luma::new(64, 64, 0);
        for (i, v) in l.data.iter_mut().enumerate() {
            *v = if i % 2 == 0 { 200 } else { 0 };
        }
        let map = ScreenMap {
            h: Homography::IDENTITY,
            lens: Lens::none((64, 64)),
        };
        let t = warp(&l, &map, "t", Rect::new(8.0, 8.0, 32.0, 32.0), 0.25);
        assert!(
            t.data.iter().all(|&v| (90..=110).contains(&v)),
            "{:?}",
            t.data
        );
    }
}

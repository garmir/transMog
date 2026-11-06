//! the stamp band burned into the top of every NDI frame: 16 cells of
//! width / 16 over the top 32/1080 of the height, most significant bit on
//! the left, white for 1 and black for 0, carrying seq & 0xffff. the
//! visualiser's stamp rows use the same scheme, so one decoder reads both.
//! pack.wgsl draws it on the gpu; `burn_uyvy` is its cpu twin for tests
//! and receivers.

pub const CELLS: u32 = 16;
/// the band at the 1080 line reference height; `band_height` scales it
pub const BAND_HEIGHT: u32 = 32;
pub const REFERENCE_HEIGHT: u32 = 1080;
/// bt.709 limited range, the levels pack.wgsl writes
pub const WHITE: u8 = 235;
pub const BLACK: u8 = 16;

pub fn value(seq: u64) -> u16 {
    (seq & 0xffff) as u16
}

/// pixel x is in cell `x * 16 / width`, rounded down, which is what
/// pack.wgsl computes; a width that is not a multiple of 16 spreads the
/// remainder over the cells
pub fn cell_of(width: u32, x: u32) -> u32 {
    x * CELLS / width
}

/// the x range `[x0, x1)` of cell `i`, the inverse of `cell_of`
pub fn cell_span(width: u32, i: u32) -> (u32, u32) {
    (
        (i * width).div_ceil(CELLS),
        ((i + 1) * width).div_ceil(CELLS),
    )
}

/// the band's rows at a frame height, rounded up: a receiver that takes
/// the top 32/1080 of the picture as the band then never reads a picture
/// row, and 1080 gives the 32 rows it always had
pub fn band_height(height: u32) -> u32 {
    (BAND_HEIGHT * height).div_ceil(REFERENCE_HEIGHT)
}

pub fn bit(value: u16, cell: u32) -> bool {
    (value >> (CELLS - 1 - cell)) & 1 == 1
}

/// writes the band into a uyvy frame (U Y0 V Y1 per two pixels) of
/// `height` rows
pub fn burn_uyvy(frame: &mut [u8], stride: usize, (width, height): (u32, u32), value: u16) {
    for row in frame.chunks_mut(stride).take(band_height(height) as usize) {
        for x in 0..width {
            let luma = if bit(value, cell_of(width, x)) {
                WHITE
            } else {
                BLACK
            };
            let base = (x / 2 * 4) as usize;
            row[base] = 128;
            row[base + 2] = 128;
            row[base + 1 + (x % 2) as usize * 2] = luma;
        }
    }
}

/// reads the band back from rows of 8-bit luma, `luma(x, y)` for y in
/// 0..band. each cell is averaged over its middle half in x and in y
/// (rows 8..24 of a 32 row band), so a scaler's soft edges and a codec's
/// ringing stay out of it. the
/// threshold sits halfway between the darkest and brightest cell; a band
/// with less than 64 levels between them reads as all ones above 192, all
/// zeros below 64, and as no stamp in between
pub fn decode(width: u32, band: u32, luma: impl Fn(u32, u32) -> u8) -> Option<u16> {
    if width < CELLS * 4 || band < 4 {
        return None;
    }
    let (y0, y1) = (band / 4, band - band / 4);
    let mut means = [0u32; CELLS as usize];
    for (cell, mean) in means.iter_mut().enumerate() {
        let (x0, x1) = cell_span(width, cell as u32);
        let quarter = (x1 - x0) / 4;
        let (x0, x1) = (x0 + quarter, x1 - quarter);
        let (mut sum, mut n) = (0u32, 0u32);
        for y in y0..y1 {
            for x in x0..x1 {
                sum += u32::from(luma(x, y));
                n += 1;
            }
        }
        *mean = sum / n.max(1);
    }
    let lo = *means.iter().min()?;
    let hi = *means.iter().max()?;
    let threshold = if hi - lo >= 64 {
        (lo + hi) / 2
    } else if lo >= 192 {
        return Some(0xffff);
    } else if hi < 64 {
        return Some(0);
    } else {
        return None;
    };
    Some(
        means
            .iter()
            .fold(0u16, |acc, &m| (acc << 1) | u16::from(m > threshold)),
    )
}

/// `decode` over a uyvy frame of `height` rows
pub fn decode_uyvy(frame: &[u8], stride: usize, (width, height): (u32, u32)) -> Option<u16> {
    let band = band_height(height);
    if frame.len() < stride * band as usize {
        return None;
    }
    decode(width, band, |x, y| {
        frame[y as usize * stride + (x / 2 * 4) as usize + 1 + (x % 2) as usize * 2]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, value: u16) -> Vec<u8> {
        let stride = width as usize * 2;
        let mut f = vec![0x80; stride * 40];
        burn_uyvy(&mut f, stride, (width, 1080), value);
        f
    }

    #[test]
    fn value_keeps_the_low_16_bits() {
        assert_eq!(value(0x1_2345), 0x2345);
        assert_eq!(value(u64::MAX), 0xffff);
    }

    #[test]
    fn msb_is_the_left_cell() {
        assert!(bit(0x8000, 0));
        assert!(!bit(0x8000, 1));
        assert!(bit(0x0001, 15));
        let f = frame(1920, 0x8001);
        // cell 0 is x 0..120, cell 15 is x 1800..1920
        assert_eq!(f[1], WHITE);
        assert_eq!(f[(120 / 2 * 4) + 1], BLACK);
        assert_eq!(f[1918 * 2 + 3], WHITE);
        // chroma stays neutral and rows below the band are left alone
        assert_eq!(f[0], 128);
        assert_eq!(f[2], 128);
        assert!(f[32 * 3840..].iter().all(|&b| b == 0x80));
    }

    #[test]
    fn spans_and_cells_agree() {
        for width in [1920, 1000, 642] {
            for x in 0..width {
                let (x0, x1) = cell_span(width, cell_of(width, x));
                assert!(x0 <= x && x < x1, "{width} {x}");
            }
            assert_eq!(cell_span(width, 15).1, width);
        }
    }

    #[test]
    fn round_trips_at_capture_sizes() {
        for width in [1920, 1280, 640, 1000] {
            for v in [0, 1, 0x8000, 0xffff, 0xa5a5, 0x1234, 54321] {
                let f = frame(width, v);
                assert_eq!(
                    decode_uyvy(&f, width as usize * 2, (width, 1080)),
                    Some(v),
                    "{width} {v:#x}"
                );
            }
        }
    }

    #[test]
    fn survives_noise_and_soft_edges() {
        let width = 1920;
        let v = 0x5a3c;
        let mut f = frame(width, v);
        let mut seed = 7u32;
        for (i, b) in f.iter_mut().enumerate() {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let noise = (seed >> 16) as i32 % 41 - 20;
            if i % 2 == 1 {
                *b = (i32::from(*b) + noise).clamp(0, 255) as u8;
            }
        }
        // smear the first and last 10 px of every cell, as a scaler would
        let blurred = decode(width, BAND_HEIGHT, |x, y| {
            let (x0, x1) = cell_span(width, cell_of(width, x));
            if x < x0 + 10 || x + 10 >= x1 {
                128
            } else {
                f[y as usize * 3840 + (x / 2 * 4) as usize + 1 + (x % 2) as usize * 2]
            }
        });
        assert_eq!(blurred, Some(v));
    }

    #[test]
    fn a_flat_grey_band_is_not_a_stamp() {
        assert_eq!(decode(1920, 32, |_, _| 128), None);
        assert_eq!(decode(1920, 32, |_, _| 200), Some(0xffff));
        assert_eq!(decode(1920, 32, |_, _| 20), Some(0));
        assert_eq!(decode(32, 32, |_, _| 20), None);
        assert_eq!(decode(1920, 3, |_, _| 20), None);
    }

    #[test]
    fn band_scales_with_the_height() {
        assert_eq!(band_height(1080), 32);
        assert_eq!(band_height(540), 16);
        assert_eq!(band_height(720), 22);
        assert_eq!(band_height(360), 11);
        for (width, height) in [(640, 360), (960, 540), (1280, 720)] {
            let band = band_height(height);
            // never short of the 32/1080 a receiver takes as the band
            assert!(band * REFERENCE_HEIGHT >= BAND_HEIGHT * height);
            let stride = width as usize * 2;
            for v in [0, 0x8001, 0xa5a5, 54321] {
                let mut f = vec![0x80; stride * height as usize];
                burn_uyvy(&mut f, stride, (width, height), v);
                assert_eq!(
                    decode_uyvy(&f, stride, (width, height)),
                    Some(v),
                    "{width} {v:#x}"
                );
                // the band stops at its last row, the picture is untouched
                let below = &f[band as usize * stride..];
                assert!(below.iter().all(|&b| b == 0x80), "{width}x{height}");
                // cells are width / 16 wide: cell 0 lights for the msb only
                let cell = (width / 16) as usize;
                let y = |x: usize| f[x / 2 * 4 + 1 + (x % 2) * 2];
                assert_eq!(y(cell - 1), if v & 0x8000 != 0 { WHITE } else { BLACK });
            }
        }
    }
}

use crate::capture::{Frame, PixelLayout};

/// something the analysis can read grey levels from: a copied luma
/// plane, or a captured frame read in place
pub trait Sample {
    fn size(&self) -> (usize, usize);
    /// grey at an integer pixel, which must be inside
    fn grey(&self, x: usize, y: usize) -> u8;

    /// bilinear grey at a camera pixel position (pixel centres at +0.5);
    /// none outside the picture
    fn at(&self, x: f32, y: f32) -> Option<f32> {
        let (w, h) = self.size();
        let (fx, fy) = (x - 0.5, y - 0.5);
        if !(fx >= 0.0 && fy >= 0.0 && fx <= (w - 1) as f32 && fy <= (h - 1) as f32) {
            return None;
        }
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
        let g = |x, y| f32::from(self.grey(x, y));
        let top = g(x0, y0) * (1.0 - ax) + g(x1, y0) * ax;
        let bottom = g(x0, y1) * (1.0 - ax) + g(x1, y1) * ax;
        Some(top * (1.0 - ay) + bottom * ay)
    }
}

/// one byte of grey a pixel
#[derive(Clone, Debug)]
pub struct Luma {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// rec.601 weights in 1/256, the same grey cv2 and the python tools see
fn grey_rgb(r: u8, g: u8, b: u8) -> u8 {
    ((77 * u32::from(r) + 150 * u32::from(g) + 29 * u32::from(b) + 128) >> 8) as u8
}

impl Luma {
    pub fn new(width: usize, height: usize, fill: u8) -> Luma {
        Luma {
            width,
            height,
            data: vec![fill; width * height],
        }
    }

    pub fn from_frame(frame: &Frame) -> Option<Luma> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let view = FrameView::new(frame)?;
        let mut data = Vec::with_capacity(w * h);
        match frame.layout {
            PixelLayout::Rgba8 => data.extend(
                frame.data[..w * h * 4]
                    .chunks_exact(4)
                    .map(|p| grey_rgb(p[0], p[1], p[2])),
            ),
            PixelLayout::Yuyv => {
                for y in 0..h {
                    data.extend((0..w).map(|x| view.grey(x, y)));
                }
            }
        }
        Some(Luma {
            width: w,
            height: h,
            data,
        })
    }

    /// the mean of every 2x2 block; an odd last row or column is dropped
    pub fn half(&self) -> Luma {
        let (w, h) = (self.width / 2, self.height / 2);
        let mut data = Vec::with_capacity(w * h);
        for y in 0..h {
            let a = &self.data[2 * y * self.width..];
            let b = &self.data[(2 * y + 1) * self.width..];
            data.extend((0..w).map(|x| {
                let s = u16::from(a[2 * x])
                    + u16::from(a[2 * x + 1])
                    + u16::from(b[2 * x])
                    + u16::from(b[2 * x + 1]);
                ((s + 2) / 4) as u8
            }));
        }
        Luma {
            width: w,
            height: h,
            data,
        }
    }

    /// the grey level that `q` (0..1) of the pixels do not exceed
    pub fn percentile(&self, q: f32) -> u8 {
        let mut hist = [0usize; 256];
        for &v in &self.data {
            hist[usize::from(v)] += 1;
        }
        let want = (q * self.data.len() as f32) as usize;
        let mut seen = 0;
        for (v, n) in hist.iter().enumerate() {
            seen += n;
            if seen > want {
                return v as u8;
            }
        }
        255
    }
}

impl Sample for Luma {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn grey(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.width + x]
    }
}

/// a captured frame read without a copy, for the per-frame stamp read
pub struct FrameView<'a> {
    frame: &'a Frame,
}

impl<'a> FrameView<'a> {
    /// none when the buffer is shorter than its size says
    pub fn new(frame: &'a Frame) -> Option<FrameView<'a>> {
        let px = frame.width as usize * frame.height as usize;
        let need = match frame.layout {
            PixelLayout::Rgba8 => px * 4,
            PixelLayout::Yuyv => px * 2,
        };
        (px > 0 && frame.data.len() >= need).then_some(FrameView { frame })
    }
}

impl Sample for FrameView<'_> {
    fn size(&self) -> (usize, usize) {
        (self.frame.width as usize, self.frame.height as usize)
    }

    fn grey(&self, x: usize, y: usize) -> u8 {
        let w = self.frame.width as usize;
        match self.frame.layout {
            PixelLayout::Rgba8 => {
                let i = (y * w + x) * 4;
                let p = &self.frame.data[i..i + 3];
                grey_rgb(p[0], p[1], p[2])
            }
            // limited range y, stretched to full range like the jpeg grey
            PixelLayout::Yuyv => {
                let v = i32::from(self.frame.data[(y * w + x) * 2]);
                ((v - 16) * 255 / 219).clamp(0, 255) as u8
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_averages_blocks() {
        let l = Luma {
            width: 4,
            height: 2,
            data: vec![0, 4, 100, 100, 8, 12, 100, 100],
        };
        assert_eq!(l.half().data, vec![6, 100]);
    }

    #[test]
    fn bilinear_is_none_outside() {
        let l = Luma::new(4, 4, 50);
        assert_eq!(l.at(2.0, 2.0), Some(50.0));
        assert_eq!(l.at(-1.0, 2.0), None);
        assert_eq!(l.at(2.0, 9.0), None);
    }
}

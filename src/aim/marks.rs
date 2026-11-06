//! the timing marks the latency visualiser draws, in pixels of a
//! 1920x1080 frame. a copy of the marks table in atlas-node's
//! bench/seat-loop/audio-guest.c (gray-coded stamps, deployed
//! 2026-11-05 22:55), whose comment gives every cell's meaning; `decode`
//! follows marks.py beside it

use egui::{pos2, Pos2};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect {
            x0: x,
            y0: y,
            x1: x + w,
            y1: y + h,
        }
    }

    /// shrunk by `f` of its size on every side; 0.25 is the middle half
    pub fn inner(self, f: f32) -> Rect {
        let (dx, dy) = ((self.x1 - self.x0) * f, (self.y1 - self.y0) * f);
        Rect {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 - dx,
            y1: self.y1 - dy,
        }
    }

    /// top left, top right, bottom right, bottom left
    pub fn corners(self) -> [Pos2; 4] {
        [
            pos2(self.x0, self.y0),
            pos2(self.x1, self.y0),
            pos2(self.x1, self.y1),
            pos2(self.x0, self.y1),
        ]
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }

    /// grown by `d` frame pixels on every side
    pub fn grow(self, d: f32) -> Rect {
        Rect {
            x0: self.x0 - d,
            y0: self.y0 - d,
            x1: self.x1 + d,
            y1: self.y1 + d,
        }
    }

    pub fn centre(self) -> Pos2 {
        pos2((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }
}

pub const FRAME: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);

/// the ruler session's stamp row; nothing here draws or reads it
pub const RULER: Rect = Rect::new(0.0, 96.0, 1920.0, 32.0);

pub const STAMPS: Rect = Rect::new(336.0, 160.0, 500.0, 240.0);
pub const STAMP_QUIET: f32 = 16.0;

/// the two-cell toggles, each with its quiet border: the key mark flips
/// on every note-on, mid and bottom on every frame
pub const KEY: (Rect, f32) = (Rect::new(336.0, 440.0, 224.0, 56.0), 24.0);
pub const MID: (Rect, f32) = (Rect::new(316.0, 544.0, 104.0, 40.0), 8.0);
pub const BOTTOM: (Rect, f32) = (Rect::new(1264.0, 992.0, 104.0, 40.0), 8.0);
const COLS: usize = 10;
const LINES: usize = 6;

/// an ArUco DICT_4X4_50 marker: a white square one module wide, a black
/// border, then the 4x4 payload, 8 modules across
#[derive(Clone, Copy, Debug)]
pub struct Fid {
    pub id: u8,
    pub rect: Rect,
    pub payload: u16,
}

pub const FIDS: [Fid; 4] = [
    Fid {
        id: 0,
        rect: Rect::new(72.0, 144.0, 224.0, 224.0),
        payload: 0xb532,
    },
    Fid {
        id: 1,
        rect: Rect::new(1680.0, 136.0, 192.0, 192.0),
        payload: 0x0f9a,
    },
    Fid {
        id: 2,
        rect: Rect::new(1624.0, 808.0, 224.0, 224.0),
        payload: 0x332d,
    },
    Fid {
        id: 3,
        rect: Rect::new(72.0, 392.0, 224.0, 224.0),
        payload: 0x9946,
    },
];

pub fn stamp_cell(line: usize, col: usize) -> Rect {
    let (cw, ch) = (
        STAMPS.width() / COLS as f32,
        (STAMPS.y1 - STAMPS.y0) / LINES as f32,
    );
    Rect::new(
        STAMPS.x0 + col as f32 * cw,
        STAMPS.y0 + line as f32 * ch,
        cw,
        ch,
    )
}

/// what one stamp read gives: N, the audio packet and the MIDI sequence,
/// each none when its start cell is dark or its parity fails, and the
/// two level reference cells
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stamps {
    pub values: [Option<u16>; 3],
    pub white: f32,
    pub black: f32,
}

impl Stamps {
    pub fn ok(&self) -> bool {
        self.values.iter().all(Option::is_some)
    }

    pub fn decoded(&self) -> usize {
        self.values.iter().filter(|v| v.is_some()).count()
    }
}

/// a cell this near the middle of the white-to-black span is a blend
const GREY: f32 = 0.25;

fn from_gray(mut g: u16) -> u16 {
    let mut v = 0;
    while g != 0 {
        v ^= g;
        g >>= 1;
    }
    v
}

/// `level` is the mean grey of a frame rectangle; only the middle half
/// of each cell is asked for. the values are gray-coded, so when the
/// exposure spans a flip a parity failure with one cell near mid-grey
/// is resolved by flipping that cell, either reading being N or N+1
pub fn decode(level: impl Fn(Rect) -> f32) -> Stamps {
    let g = |line, col| level(stamp_cell(line, col).inner(0.25));
    let (white, black) = (g(4, 9), g(5, 9));
    let thr = (white + black) / 2.0;
    let span = (white - black).max(1.0);
    let mut values = [None; 3];
    for (v, value) in values.iter_mut().enumerate() {
        let (hi, lo) = (2 * v, 2 * v + 1);
        // 16 data cells, then the parity cell
        let cells: Vec<f32> = (1..9)
            .map(|c| g(hi, c))
            .chain((1..9).map(|c| g(lo, c)))
            .chain([g(lo, 0)])
            .collect();
        let mut bits: Vec<bool> = cells.iter().map(|&x| x > thr).collect();
        if g(hi, 0) <= thr || span <= 30.0 {
            continue;
        }
        if bits.iter().filter(|&&b| b).count() % 2 == 0 {
            let (k, d) = cells
                .iter()
                .map(|x| (x - thr).abs() / span)
                .enumerate()
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((0, 1.0));
            if d >= GREY {
                continue;
            }
            bits[k] = !bits[k];
        }
        let gray = bits[..16].iter().fold(0u16, |a, &b| a << 1 | u16::from(b));
        *value = Some(from_gray(gray));
    }
    Stamps {
        values,
        white,
        black,
    }
}

/// whether frame pixel (x, y) is white, black or not part of any mark,
/// as the visualiser draws it with the toggles at rest; builds synthetic
/// frames for the tests
pub fn mark_at(x: f32, y: f32, values: [u16; 3]) -> Option<bool> {
    let q = STAMP_QUIET;
    let quiet = Rect {
        x0: STAMPS.x0 - q,
        y0: STAMPS.y0 - q,
        x1: STAMPS.x1 + q,
        y1: STAMPS.y1 + q,
    };
    if quiet.contains(x, y) {
        if !STAMPS.contains(x, y) {
            return Some(false);
        }
        let col = (((x - STAMPS.x0) / STAMPS.width() * COLS as f32) as usize).min(COLS - 1);
        let line =
            (((y - STAMPS.y0) / (STAMPS.y1 - STAMPS.y0) * LINES as f32) as usize).min(LINES - 1);
        let val = values[line / 2] ^ values[line / 2] >> 1;
        let on = if col == 9 {
            [false, true, false, true, true, false][line]
        } else if col == 0 {
            line.is_multiple_of(2) || val.count_ones().is_multiple_of(2)
        } else {
            let shift = if line % 2 == 1 { 8 - col } else { 16 - col };
            (val >> shift) & 1 == 1
        };
        return Some(on);
    }
    for f in &FIDS {
        if !f.rect.contains(x, y) {
            continue;
        }
        let u = f.rect.width() / 8.0;
        let (mx, my) = (
            ((x - f.rect.x0) / u) as usize,
            ((y - f.rect.y0) / u) as usize,
        );
        let on = match (mx, my) {
            (0 | 7, _) | (_, 0 | 7) => true,
            (1 | 6, _) | (_, 1 | 6) => false,
            (c, r) => (f.payload >> (15 - 4 * (r - 2) - (c - 2))) & 1 == 1,
        };
        return Some(on);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level_of(values: [u16; 3]) -> impl Fn(Rect) -> f32 {
        move |r: Rect| {
            let (x, y) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
            match mark_at(x, y, values) {
                Some(true) => 220.0,
                _ => 10.0,
            }
        }
    }

    #[test]
    fn decode_reads_what_mark_at_draws() {
        for values in [[0, 0, 0], [0xffff, 1, 0x8000], [12345, 54321, 7]] {
            let got = decode(level_of(values));
            assert_eq!(got.values, values.map(Some), "{values:?}");
        }
    }

    /// N and N+1 differ in one cell; drawn half way between, that cell
    /// is grey and the read still gives one of the two
    #[test]
    fn decode_resolves_a_blend_of_two_frames() {
        let (a, b) = ([1000, 7, 7], [1001, 7, 7]);
        let blend = move |r: Rect| {
            let (x, y) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
            let v = |values| match mark_at(x, y, values) {
                Some(true) => 220.0,
                _ => 10.0,
            };
            (v(a) + v(b)) / 2.0
        };
        let n = decode(blend).values[0];
        assert!(n == Some(1000) || n == Some(1001), "{n:?}");
    }

    #[test]
    fn decode_rejects_a_flat_picture() {
        assert_eq!(decode(|_| 128.0).decoded(), 0);
    }

    #[test]
    fn fiducial_corners_are_white_and_border_black() {
        let f = FIDS[2].rect;
        let u = f.width() / 8.0;
        assert_eq!(mark_at(f.x0 + 1.0, f.y0 + 1.0, [0; 3]), Some(true));
        assert_eq!(mark_at(f.x0 + 1.5 * u, f.y0 + 1.5 * u, [0; 3]), Some(false));
        assert_eq!(mark_at(5.0, 5.0, [0; 3]), None);
    }
}

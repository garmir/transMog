use egui::{pos2, Pos2};

use super::geom::Homography;
use super::luma::{Luma, Sample};
use super::marks::{self, Rect, Stamps, FIDS, FRAME, STAMPS};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Name {
    Projector,
    Mini,
}

impl Name {
    pub const ALL: [Name; 2] = [Name::Projector, Name::Mini];

    pub fn label(self) -> &'static str {
        match self {
            Name::Projector => "projector",
            Name::Mini => "mini",
        }
    }

    /// the key calibration.json and the python tools use
    pub fn key(self) -> &'static str {
        match self {
            Name::Projector => "proj",
            Name::Mini => "mini",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

/// which picture edges a screen runs past
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub top: bool,
    pub bottom: bool,
    pub left: bool,
    pub right: bool,
}

impl Edges {
    pub fn any(self) -> bool {
        self.top || self.bottom || self.left || self.right
    }

    pub fn names(self) -> Vec<&'static str> {
        [
            (self.top, "top"),
            (self.bottom, "bottom"),
            (self.left, "left"),
            (self.right, "right"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect()
    }
}

/// a bright square with a dark border inside: a fiducial's outer white
/// frame, in camera pixels
#[derive(Clone, Debug)]
pub struct Square {
    /// top left, top right, bottom right, bottom left
    pub corners: [Pos2; 4],
    pub side: f32,
}

impl Square {
    pub fn centre(&self) -> Pos2 {
        let s = self
            .corners
            .iter()
            .fold(egui::Vec2::ZERO, |a, p| a + p.to_vec2());
        (s / 4.0).to_pos2()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FidView {
    pub id: u8,
    /// where the fit puts the marker, found or not
    pub quad: [Pos2; 4],
    pub seen: bool,
}

/// one visualiser picture found in the camera frame
#[derive(Clone, Debug)]
pub struct Screen {
    pub name: Name,
    /// frame pixels to camera pixels
    pub h: Homography,
    /// the whole 1920x1080 frame
    pub outline: [Pos2; 4],
    pub stamp_block: [Pos2; 4],
    pub fids: Vec<FidView>,
    pub clipped: Edges,
    /// camera pixels per frame pixel at the stamps
    pub scale: f32,
    pub module_px: f32,
    pub fit_err: f32,
    /// none when the stamp block is not wholly in the picture
    pub stamps: Option<Stamps>,
    /// laplacian variance over variance in the stamp block
    pub focus: Option<f32>,
    /// share of the stamp block's pixels at 250 or more
    pub saturated: f32,
}

impl Screen {
    pub fn seen_ids(&self) -> Vec<u8> {
        self.fids.iter().filter(|f| f.seen).map(|f| f.id).collect()
    }
}

/// the mean grey of a frame rectangle through `h`, from a 3x3 grid in
/// it; none when a sample falls outside the picture
pub fn level<S: Sample>(s: &S, h: &Homography, r: Rect) -> Option<f32> {
    let mut sum = 0.0;
    for j in 0..3 {
        for i in 0..3 {
            let fx = r.x0 + (r.x1 - r.x0) * (i as f32 + 0.5) / 3.0;
            let fy = r.y0 + (r.y1 - r.y0) * (j as f32 + 0.5) / 3.0;
            let p = h.apply(pos2(fx, fy));
            sum += s.at(p.x, p.y)?;
        }
    }
    Some(sum / 9.0)
}

/// the stamps through `h`, none when the block is not wholly in view
pub fn read_stamps<S: Sample>(s: &S, h: &Homography) -> Option<Stamps> {
    let (w, ht) = s.size();
    let inside = h
        .apply_all(STAMPS.corners())
        .iter()
        .all(|p| p.x >= 0.0 && p.y >= 0.0 && p.x <= w as f32 && p.y <= ht as f32);
    inside.then(|| marks::decode(|r| level(s, h, r).unwrap_or(0.0)))
}

/// bright components of the half-size copy that look like a fiducial's
/// white frame, checked against the full-size picture for the dark
/// border one module in
pub fn squares(full: &Luma) -> Vec<Square> {
    let half = full.half();
    let thr = (0.45 * f32::from(half.percentile(0.999))).max(50.0) as u8;
    let mut out = Vec::new();
    for c in components(&half, thr) {
        let (w, h) = (c.x1 - c.x0 + 1, c.y1 - c.y0 + 1);
        if w < 5 || h < 5 || w > half.height / 2 || h > half.height / 2 {
            continue;
        }
        let aspect = w as f32 / h as f32;
        let fill = c.area as f32 / (w * h) as f32;
        if !(0.6..=1.67).contains(&aspect) || !(0.3..=0.95).contains(&fill) {
            continue;
        }
        let corners = [c.tl, c.tr, c.br, c.bl].map(|p| pos2(p.x * 2.0, p.y * 2.0));
        let sides: Vec<f32> = (0..4)
            .map(|i| corners[i].distance(corners[(i + 1) % 4]))
            .collect();
        let (lo, hi) = sides
            .iter()
            .fold((f32::MAX, 0f32), |(lo, hi), &s| (lo.min(s), hi.max(s)));
        if lo < 0.5 * hi {
            continue;
        }
        let Some(levels) = framed(full, &corners) else {
            continue;
        };
        let corners = refine(full, &corners, levels).unwrap_or(corners);
        let side = (0..4)
            .map(|i| corners[i].distance(corners[(i + 1) % 4]))
            .sum::<f32>()
            / 4.0;
        out.push(Square { corners, side });
    }
    out
}

/// a point of the quad at (u, v), each 0..1 from its top left corner
fn quad_point(c: &[Pos2; 4], u: f32, v: f32) -> Pos2 {
    let top = c[0] + (c[1] - c[0]) * u;
    let bottom = c[3] + (c[2] - c[3]) * u;
    top + (bottom - top) * v
}

/// white along the outer module, darker along the next one in; gives
/// the white level and the level just outside the square
fn framed(full: &Luma, c: &[Pos2; 4]) -> Option<(f32, f32)> {
    let ring = |inset: f32, from: usize, to: usize| {
        let mut sum = 0.0;
        let mut n = 0;
        for k in from..to {
            let t = (k as f32 + 0.5) / 8.0;
            for (u, v) in [(t, inset), (t, 1.0 - inset), (inset, t), (1.0 - inset, t)] {
                let p = quad_point(c, u, v);
                if let Some(g) = full.at(p.x, p.y) {
                    sum += g;
                    n += 1;
                }
            }
        }
        (n > 0).then(|| sum / n as f32)
    };
    let (white, black) = (ring(0.5 / 8.0, 0, 8)?, ring(1.5 / 8.0, 1, 7)?);
    let outside = ring(-0.5 / 8.0, 0, 8).unwrap_or(black);
    (white - black > (0.3 * white).max(20.0)).then_some((white, outside))
}

/// the square's corners again at full size, from the pixels above the
/// level halfway between its white frame and what surrounds it. the
/// half-size threshold follows the brightest thing in the picture, the
/// projector, and so cuts a dimmer mini's squares about a pixel short on
/// every side, which is too much for a fit from two of them
fn refine(full: &Luma, c: &[Pos2; 4], (white, outside): (f32, f32)) -> Option<[Pos2; 4]> {
    let b = super::geom::bounds(*c)?;
    let pad = 0.2 * b.width().max(b.height());
    let x0 = (b.min.x - pad).max(0.0) as usize;
    let y0 = (b.min.y - pad).max(0.0) as usize;
    let x1 = ((b.max.x + pad) as usize).min(full.width - 1);
    let y1 = ((b.max.y + pad) as usize).min(full.height - 1);
    let thr = (white + outside) / 2.0;
    let seed = quad_point(c, 0.5, 0.5 / 8.0);
    let (sx, sy) = (seed.x as usize, seed.y as usize);
    if !(x0..=x1).contains(&sx) || !(y0..=y1).contains(&sy) {
        return None;
    }
    let bright = |x: usize, y: usize| f32::from(full.grey(x, y)) > thr;
    if !bright(sx, sy) {
        return None;
    }
    let w = x1 - x0 + 1;
    let mut seen = vec![false; w * (y1 - y0 + 1)];
    let mut stack = vec![(sx, sy)];
    seen[(sy - y0) * w + sx - x0] = true;
    let mut keys = [f32::MAX, f32::MIN, f32::MIN, f32::MAX];
    let mut out = [Pos2::ZERO; 4];
    while let Some((x, y)) = stack.pop() {
        if x == x0 || y == y0 || x == x1 || y == y1 {
            // ran into something else bright: keep the coarse corners
            return None;
        }
        let (fx, fy) = (x as f32, y as f32);
        let cand = [
            (fx + fy, pos2(fx, fy)),
            (fx + 1.0 - fy, pos2(fx + 1.0, fy)),
            (fx + fy + 2.0, pos2(fx + 1.0, fy + 1.0)),
            (fx - fy - 1.0, pos2(fx, fy + 1.0)),
        ];
        for (i, (k, p)) in cand.into_iter().enumerate() {
            let better = if i == 1 || i == 2 {
                k > keys[i]
            } else {
                k < keys[i]
            };
            if better {
                keys[i] = k;
                out[i] = p;
            }
        }
        for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
            let i = (ny - y0) * w + nx - x0;
            if !seen[i] && bright(nx, ny) {
                seen[i] = true;
                stack.push((nx, ny));
            }
        }
    }
    Some(out)
}

struct Component {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
    area: usize,
    tl: Pos2,
    tr: Pos2,
    br: Pos2,
    bl: Pos2,
    // the extremes the corners were taken from: min x+y, max x-y,
    // max x+y, min x-y
    keys: [f32; 4],
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// 4-connected components of pixels above `thr`, from runs per row
fn components(l: &Luma, thr: u8) -> Vec<Component> {
    // (y, first x, last x, label)
    let mut runs: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut parent: Vec<usize> = Vec::new();
    let mut prev = 0..0;
    for y in 0..l.height {
        let row = &l.data[y * l.width..(y + 1) * l.width];
        let start = runs.len();
        let mut x = 0;
        while x < l.width {
            if row[x] <= thr {
                x += 1;
                continue;
            }
            let a = x;
            while x < l.width && row[x] > thr {
                x += 1;
            }
            let label = parent.len();
            parent.push(label);
            runs.push((y, a, x - 1, label));
        }
        let mut p = prev.start;
        for r in start..runs.len() {
            let (_, a, b, label) = runs[r];
            while p < prev.end && runs[p].2 < a {
                p += 1;
            }
            let mut q = p;
            while q < prev.end && runs[q].1 <= b {
                let (ra, rb) = (find(&mut parent, label), find(&mut parent, runs[q].3));
                if ra != rb {
                    parent[ra.max(rb)] = ra.min(rb);
                }
                q += 1;
            }
        }
        prev = start..runs.len();
    }

    let mut index = vec![usize::MAX; parent.len()];
    let mut out: Vec<Component> = Vec::new();
    for &(y, a, b, label) in &runs {
        let root = find(&mut parent, label);
        if index[root] == usize::MAX {
            index[root] = out.len();
            out.push(Component {
                x0: a,
                y0: y,
                x1: b,
                y1: y,
                area: 0,
                tl: Pos2::ZERO,
                tr: Pos2::ZERO,
                br: Pos2::ZERO,
                bl: Pos2::ZERO,
                keys: [f32::MAX, f32::MIN, f32::MIN, f32::MAX],
            });
        }
        let c = &mut out[index[root]];
        c.x0 = c.x0.min(a);
        c.x1 = c.x1.max(b);
        c.y1 = c.y1.max(y);
        c.area += b - a + 1;
        let (fa, fb, fy) = (a as f32, (b + 1) as f32, y as f32);
        if fa + fy < c.keys[0] {
            c.keys[0] = fa + fy;
            c.tl = pos2(fa, fy);
        }
        if fb - fy > c.keys[1] {
            c.keys[1] = fb - fy;
            c.tr = pos2(fb, fy);
        }
        if fb + fy + 1.0 > c.keys[2] {
            c.keys[2] = fb + fy + 1.0;
            c.br = pos2(fb, fy + 1.0);
        }
        if fa - fy - 1.0 < c.keys[3] {
            c.keys[3] = fa - fy - 1.0;
            c.bl = pos2(fa, fy + 1.0);
        }
    }
    out
}

/// fiducial ids matched to squares, and the map they give
struct Hyp {
    members: Vec<(usize, usize)>,
    h: Homography,
    err: f32,
}

fn fit(members: &[(usize, usize)], squares: &[Square]) -> Option<(Homography, f32)> {
    let pairs: Vec<(Pos2, Pos2)> = members
        .iter()
        .flat_map(|&(id, s)| FIDS[id].rect.corners().into_iter().zip(squares[s].corners))
        .collect();
    let h = match members.len() {
        0 => return None,
        1 => Homography::similarity(&pairs)?,
        // the mini is seen at an angle, and its mode need not be 16:9, so
        // two markers already get their own x and y scales
        2 | 3 => Homography::affine(&pairs)?,
        _ => Homography::projective(&pairs)?,
    };
    let centre = h.apply(pos2(960.0, 540.0));
    let right = h.apply(pos2(1060.0, 540.0)) - centre;
    let down = h.apply(pos2(960.0, 640.0)) - centre;
    // no mirror image, and no more than 30 degrees of roll
    if right.x * down.y - right.y * down.x <= 0.0 || right.y.abs() > 0.58 * right.x {
        return None;
    }
    Some((h, h.residual(&pairs)))
}

/// the stamp cells that never change, white (the start cells and the
/// white level) and black (the black level and the quiet border)
fn fixed_cells() -> ([Rect; 4], [Rect; 4]) {
    let cell = |l, c| marks::stamp_cell(l, c).inner(0.25);
    let (q, s) = (marks::STAMP_QUIET, STAMPS);
    let strip = |x0, y0, x1, y1| Rect { x0, y0, x1, y1 }.inner(0.1);
    (
        [cell(0, 0), cell(2, 0), cell(4, 0), cell(4, 9)],
        [
            cell(5, 9),
            strip(s.x0 - q, s.y0 - q, s.x1 + q, s.y0 - 2.0),
            strip(s.x0 - q, s.y1 + 2.0, s.x1 + q, s.y1 + q),
            strip(s.x1 + 2.0, s.y0, s.x1 + q, s.y1),
        ],
    )
}

/// `h` with its x scale and an x perspective term about the left
/// markers chosen for the widest gap between the fixed white and black
/// stamp cells. two markers stacked at the left edge leave both nearly
/// free; stamps.py's refine() does the same search
fn refine_x(full: &Luma, h: &Homography) -> Option<Homography> {
    let (white, black) = fixed_cells();
    let x0 = f64::from(FIDS[0].rect.x0);
    let mut best: Option<(f32, Homography)> = None;
    for i in 0..28 {
        let sx = 0.85 + 0.55 * f64::from(i) / 27.0;
        for j in 0..9 {
            let p = -4e-4 + 8e-4 * f64::from(j) / 8.0;
            let t = Homography([
                sx,
                0.0,
                x0 * (1.0 - sx),
                0.0,
                1.0,
                0.0,
                p,
                0.0,
                1.0 - p * x0,
            ]);
            let h2 = h.then(&t);
            let lo = white
                .iter()
                .map(|r| level(full, &h2, *r))
                .try_fold(f32::MAX, |m, v| v.map(|v| m.min(v)));
            let hi = black
                .iter()
                .map(|r| level(full, &h2, *r))
                .try_fold(f32::MIN, |m, v| v.map(|v| m.max(v)));
            if let (Some(lo), Some(hi)) = (lo, hi) {
                if best.is_none_or(|b| lo - hi > b.0) {
                    best = Some((lo - hi, h2));
                }
            }
        }
    }
    best.filter(|b| b.0 > 0.0).map(|b| b.1)
}

fn fid_centre(id: usize) -> Pos2 {
    let r = FIDS[id].rect;
    pos2((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0)
}

/// a hypothesis from one square as fiducial `id`, grown by the nearest
/// square that lands where the map puts another fiducial
fn grow(start: (usize, usize), squares: &[Square]) -> Option<Hyp> {
    let mut members = vec![start];
    let (mut h, mut err) = fit(&members, squares)?;
    let mut tried = vec![start.0];
    loop {
        let mut best: Option<(f32, usize, usize)> = None;
        for (id, fid) in FIDS.iter().enumerate() {
            if tried.contains(&id) {
                continue;
            }
            let at = h.apply(fid_centre(id));
            let side = h.scale_at(fid_centre(id)) * fid.rect.width();
            for (s, sq) in squares.iter().enumerate() {
                if members.iter().any(|m| m.1 == s) {
                    continue;
                }
                let d = sq.centre().distance(at);
                let ratio = sq.side / side;
                if d < 0.3 * side && (0.7..1.4).contains(&ratio) && best.is_none_or(|b| d < b.0) {
                    best = Some((d, id, s));
                }
            }
        }
        let Some((_, id, s)) = best else {
            break;
        };
        tried.push(id);
        members.push((id, s));
        let mean_side =
            members.iter().map(|m| squares[m.1].side).sum::<f32>() / members.len() as f32;
        match fit(&members, squares) {
            Some((h2, e2)) if e2 < 0.2 * mean_side => (h, err) = (h2, e2),
            _ => {
                members.pop();
            }
        }
    }
    Some(Hyp { members, h, err })
}

/// every visualiser picture in view, the projector first. `zoom` is the
/// camera's digital zoom factor (1 at zoom_absolute 100), which tells a
/// lone mini from a lone projector by its size
pub fn analyse(full: &Luma, zoom: f32) -> Vec<Screen> {
    let squares = squares(full);
    let (w, ht) = (full.width as f32, full.height as f32);
    let mut hyps: Vec<(f32, Hyp)> = Vec::new();
    for s in 0..squares.len() {
        for id in 0..FIDS.len() {
            let Some(mut hyp) = grow((id, s), &squares) else {
                continue;
            };
            let left_only = hyp.members.iter().all(|m| m.0 == 0 || m.0 == 3);
            if left_only && read_stamps(full, &hyp.h).is_some() {
                if let Some(h) = refine_x(full, &hyp.h) {
                    hyp.h = h;
                }
            }
            let n = hyp.members.len();
            let stamps = read_stamps(full, &hyp.h);
            let decoded = stamps.map_or(0, |s| s.decoded());
            let contrast = stamps.is_some_and(|s| s.white - s.black > 30.0);
            let ok = match n {
                1 => decoded >= 2,
                2 => stamps.is_none() || contrast,
                _ => true,
            };
            if !ok {
                continue;
            }
            let side = squares[s].side.max(1.0);
            hyps.push((n as f32 * 10.0 + decoded as f32 * 3.0 - hyp.err / side, hyp));
        }
    }
    hyps.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut chosen: Vec<Hyp> = Vec::new();
    for (_, hyp) in hyps {
        if chosen.len() == 2 {
            break;
        }
        let used = chosen.iter().any(|c| {
            c.members
                .iter()
                .any(|m| hyp.members.iter().any(|n| n.1 == m.1))
        });
        let centre = hyp.h.apply(pos2(586.0, 280.0));
        let overlaps = chosen.iter().any(|c| {
            inside(&c.h.apply_all(FRAME.corners()), centre)
                || inside(
                    &hyp.h.apply_all(FRAME.corners()),
                    c.h.apply(pos2(586.0, 280.0)),
                )
        });
        if !used && !overlaps {
            chosen.push(hyp);
        }
    }

    let stamp_centre = pos2((STAMPS.x0 + STAMPS.x1) / 2.0, (STAMPS.y0 + STAMPS.y1) / 2.0);
    let scales: Vec<f32> = chosen.iter().map(|c| c.h.scale_at(stamp_centre)).collect();
    let names: Vec<Name> = match chosen.len() {
        2 if scales[0] >= scales[1] => vec![Name::Projector, Name::Mini],
        2 => vec![Name::Mini, Name::Projector],
        1 => {
            // the mini shows at about 0.15 camera px per frame px with
            // the c920 at 1920 wide, the projector at about 0.5
            let base = scales[0] * 1920.0 / w / zoom.max(1.0);
            vec![if base > 0.3 {
                Name::Projector
            } else {
                Name::Mini
            }]
        }
        _ => Vec::new(),
    };

    let mut out: Vec<Screen> = chosen
        .into_iter()
        .zip(names)
        .zip(scales)
        .map(|((hyp, name), scale)| {
            let outline = hyp.h.apply_all(FRAME.corners());
            let stamp_block = hyp.h.apply_all(STAMPS.corners());
            let clipped = Edges {
                top: outline.iter().any(|p| p.y < -0.5),
                bottom: outline.iter().any(|p| p.y > ht + 0.5),
                left: outline.iter().any(|p| p.x < -0.5),
                right: outline.iter().any(|p| p.x > w + 0.5),
            };
            let fids = FIDS
                .iter()
                .enumerate()
                .map(|(i, f)| FidView {
                    id: f.id,
                    quad: hyp.h.apply_all(f.rect.corners()),
                    seen: hyp.members.iter().any(|m| m.0 == i),
                })
                .collect();
            let module_px = hyp
                .members
                .iter()
                .map(|m| squares[m.1].side / 8.0)
                .sum::<f32>()
                / hyp.members.len() as f32;
            let (focus, saturated) = block_stats(full, &stamp_block);
            Screen {
                name,
                h: hyp.h,
                outline,
                stamp_block,
                fids,
                clipped,
                scale,
                module_px,
                fit_err: hyp.err,
                stamps: read_stamps(full, &hyp.h),
                focus,
                saturated,
            }
        })
        .collect();
    out.sort_by_key(|s| s.name);
    out
}

fn inside(quad: &[Pos2; 4], p: Pos2) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross != 0.0 {
            if sign != 0.0 && cross.signum() != sign {
                return false;
            }
            sign = cross.signum();
        }
    }
    true
}

/// focus (laplacian variance over grey variance, the measure the focus
/// sweep used) and the clipped share over the stamp block's bounds
fn block_stats(full: &Luma, block: &[Pos2; 4]) -> (Option<f32>, f32) {
    let Some(b) = super::geom::bounds(*block) else {
        return (None, 0.0);
    };
    let x0 = b.min.x.max(1.0) as usize;
    let y0 = b.min.y.max(1.0) as usize;
    let x1 = (b.max.x as usize).min(full.width.saturating_sub(2));
    let y1 = (b.max.y as usize).min(full.height.saturating_sub(2));
    if x1 <= x0 + 2 || y1 <= y0 + 2 {
        return (None, 0.0);
    }
    let (mut n, mut sum, mut sq, mut lsum, mut lsq, mut hot) = (0f64, 0f64, 0f64, 0f64, 0f64, 0);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let g = f64::from(full.grey(x, y));
            let lap = f64::from(full.grey(x - 1, y))
                + f64::from(full.grey(x + 1, y))
                + f64::from(full.grey(x, y - 1))
                + f64::from(full.grey(x, y + 1))
                - 4.0 * g;
            n += 1.0;
            sum += g;
            sq += g * g;
            lsum += lap;
            lsq += lap * lap;
            hot += usize::from(g >= 250.0);
        }
    }
    let var = sq / n - (sum / n).powi(2);
    let lvar = lsq / n - (lsum / n).powi(2);
    let focus = (var > 1.0).then(|| (lvar / var) as f32);
    (focus, hot as f32 / n as f32)
}

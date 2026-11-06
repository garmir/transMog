use egui::{pos2, vec2, Pos2, Vec2};

use crate::aim::geom::Homography;
use crate::aim::marks::FRAME;

/// radial lens distortion about the picture centre: a point the pinhole
/// model puts at `p` is recorded at c + (p - c)(1 + k1 r^2 + k2 r^4),
/// r = |p - c| / norm, norm half the picture width
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lens {
    pub k1: f64,
    pub k2: f64,
    pub centre: Pos2,
    pub norm: f64,
}

impl Lens {
    pub fn none(size: (u32, u32)) -> Lens {
        Lens {
            k1: 0.0,
            k2: 0.0,
            centre: pos2(size.0 as f32 / 2.0, size.1 as f32 / 2.0),
            norm: f64::from(size.0) / 2.0,
        }
    }

    pub fn is_none(&self) -> bool {
        self.k1 == 0.0 && self.k2 == 0.0
    }

    fn factor(&self, r2: f64) -> f64 {
        1.0 + self.k1 * r2 + self.k2 * r2 * r2
    }

    pub fn distort(&self, p: Pos2) -> Pos2 {
        if self.is_none() {
            return p;
        }
        let (dx, dy) = (
            f64::from(p.x - self.centre.x),
            f64::from(p.y - self.centre.y),
        );
        let f = self.factor((dx * dx + dy * dy) / (self.norm * self.norm));
        pos2(
            self.centre.x + (dx * f) as f32,
            self.centre.y + (dy * f) as f32,
        )
    }

    /// the inverse of `distort` by fixed-point steps; a few are enough
    /// for the c920's mild barrel
    pub fn undistort(&self, p: Pos2) -> Pos2 {
        if self.is_none() {
            return p;
        }
        let (dx, dy) = (
            f64::from(p.x - self.centre.x),
            f64::from(p.y - self.centre.y),
        );
        let (mut ux, mut uy) = (dx, dy);
        for _ in 0..8 {
            let f = self.factor((ux * ux + uy * uy) / (self.norm * self.norm));
            if f.abs() < 1e-6 {
                break;
            }
            (ux, uy) = (dx / f, dy / f);
        }
        pos2(self.centre.x + ux as f32, self.centre.y + uy as f32)
    }
}

/// frame pixels of one screen to camera pixels as recorded: the plane's
/// homography, then the lens
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenMap {
    pub h: Homography,
    pub lens: Lens,
}

impl ScreenMap {
    pub fn apply(&self, p: Pos2) -> Pos2 {
        self.lens.distort(self.h.apply(p))
    }

    /// camera pixels per frame pixel along x and y around `p`
    pub fn scales_at(&self, p: Pos2) -> (f32, f32) {
        let d = 4.0;
        let a = self.apply(p);
        (
            self.apply(p + vec2(d, 0.0)).distance(a) / d,
            self.apply(p + vec2(0.0, d)).distance(a) / d,
        )
    }
}

/// how the camera sees one screen, from its homography at the frame's
/// centre. angles in degrees, camera y pointing down
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Geometry {
    /// camera pixels per frame pixel, by area, along x and along y
    pub scale: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    /// the frame's x axis against the camera's, positive clockwise
    pub rotation: f32,
    /// the angle between the frame's axes less 90
    pub skew: f32,
    /// right edge over left edge and bottom over top, less 1, in percent
    pub keystone_h: f32,
    pub keystone_v: f32,
    /// between the screen's normal and the ray to its centre, from a
    /// pinhole of `focal` camera pixels
    pub view: f32,
}

/// the c920 at 1920x1080 and zoom 100: 78 degrees across the diagonal
pub const FOCAL_1080: f64 = 1360.0;

impl Geometry {
    pub fn of(h: &Homography, size: (u32, u32), zoom: f32) -> Geometry {
        let c = FRAME.centre();
        let d = 8.0;
        let o = h.apply(c);
        let ex: Vec2 = (h.apply(c + vec2(d, 0.0)) - o) / d;
        let ey: Vec2 = (h.apply(c + vec2(0.0, d)) - o) / d;
        let area = (ex.x * ey.y - ex.y * ey.x).abs();
        let angle = |v: Vec2| v.y.atan2(v.x).to_degrees();
        let edge = |a: Pos2, b: Pos2| h.apply(a).distance(h.apply(b));
        let [tl, tr, br, bl] = FRAME.corners();
        let keystone = |near: f32, far: f32| {
            if near > 0.0 {
                (far / near - 1.0) * 100.0
            } else {
                0.0
            }
        };
        Geometry {
            scale: area.sqrt(),
            scale_x: ex.length(),
            scale_y: ey.length(),
            rotation: angle(ex),
            skew: angle(ey) - angle(ex) - 90.0,
            keystone_h: keystone(edge(tl, bl), edge(tr, br)),
            keystone_v: keystone(edge(tl, tr), edge(bl, br)),
            view: view_angle(h, size, zoom),
        }
    }
}

/// the angle at the screen's centre between its normal and the line of
/// sight: K^-1 H's first two columns span the screen plane
fn view_angle(h: &Homography, size: (u32, u32), zoom: f32) -> f32 {
    let f = FOCAL_1080 * f64::from(size.0) / 1920.0 * f64::from(zoom.max(0.01));
    let (cx, cy) = (f64::from(size.0) / 2.0, f64::from(size.1) / 2.0);
    let m = h.rows();
    let col = |j: usize| {
        [
            (m[0][j] - cx * m[2][j]) / f,
            (m[1][j] - cy * m[2][j]) / f,
            m[2][j],
        ]
    };
    let (r1, r2, t) = (col(0), col(1), col(2));
    // the ray to the frame's centre, in the same camera space
    let (fx, fy) = (f64::from(FRAME.centre().x), f64::from(FRAME.centre().y));
    let ray = [
        r1[0] * fx + r2[0] * fy + t[0],
        r1[1] * fx + r2[1] * fy + t[1],
        r1[2] * fx + r2[2] * fy + t[2],
    ];
    let n = [
        r1[1] * r2[2] - r1[2] * r2[1],
        r1[2] * r2[0] - r1[0] * r2[2],
        r1[0] * r2[1] - r1[1] * r2[0],
    ];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let den = (dot(n, n) * dot(ray, ray)).sqrt();
    if den < 1e-12 {
        return 0.0;
    }
    (dot(n, ray).abs() / den)
        .clamp(0.0, 1.0)
        .acos()
        .to_degrees() as f32
}

/// the map one screen's correspondences give, by how many markers they
/// come from: one fixes a rotation and scale, two (stacked, as the mini
/// shows them) an affine map, three or more the whole homography
pub fn fit_plane(pairs: &[(Pos2, Pos2)], markers: usize) -> Option<Homography> {
    match markers {
        0 => None,
        1 => Homography::similarity(pairs),
        2 => Homography::affine(pairs),
        _ => Homography::projective(pairs),
    }
}

/// one screen's corner correspondences: frame pixels against camera
/// pixels as recorded
#[derive(Clone, Debug, Default)]
pub struct Points {
    pub pairs: Vec<(Pos2, Pos2)>,
    pub markers: usize,
}

/// rms and largest distance, camera pixels as recorded, of the corners
/// from where the map puts them
pub fn residual(map: &ScreenMap, pairs: &[(Pos2, Pos2)]) -> (f32, f32) {
    if pairs.is_empty() {
        return (0.0, 0.0);
    }
    let (mut sum, mut worst) = (0.0f32, 0.0f32);
    for (f, c) in pairs {
        let e = map.apply(*f).distance(*c);
        sum += e * e;
        worst = worst.max(e);
    }
    ((sum / pairs.len() as f32).sqrt(), worst)
}

/// each screen's plane through `lens`, fitted to its undistorted corners
pub fn fit_maps(screens: &[Points], lens: Lens) -> Vec<Option<ScreenMap>> {
    screens
        .iter()
        .map(|s| {
            let undist: Vec<(Pos2, Pos2)> = s
                .pairs
                .iter()
                .map(|(f, c)| (*f, lens.undistort(*c)))
                .collect();
            fit_plane(&undist, s.markers).map(|h| ScreenMap { h, lens })
        })
        .collect()
}

/// rms over every screen's corners after each screen's own fit
fn total_rms(screens: &[Points], lens: Lens) -> f64 {
    let (mut sum, mut n) = (0.0f64, 0usize);
    for (s, map) in screens.iter().zip(fit_maps(screens, lens)) {
        let Some(map) = map else {
            return f64::MAX;
        };
        for (f, c) in &s.pairs {
            sum += f64::from(map.apply(*f).distance(*c)).powi(2);
            n += 1;
        }
    }
    if n == 0 {
        return f64::MAX;
    }
    (sum / n as f64).sqrt()
}

/// distortion terms beyond which the fit is taken to have run away
const K_LIMIT: f64 = 0.5;

/// k1 (and k2 with two screens of three markers or more) fitted across
/// every screen at once, each screen keeping its own plane. a screen
/// with fewer than three markers has an affine map that bends with any
/// k, so it cannot help and is left out. gives the lens and the rms
/// before and after, or none when no screen can tell
pub fn fit_lens(screens: &[Points], size: (u32, u32)) -> Option<(Lens, f64, f64)> {
    let usable: Vec<Points> = screens.iter().filter(|s| s.markers >= 3).cloned().collect();
    if usable.is_empty() {
        return None;
    }
    let base = Lens::none(size);
    let before = total_rms(&usable, base);
    let both = usable.len() >= 2;
    let cost = |k: &[f64]| {
        if k.iter().any(|v| v.abs() > K_LIMIT) {
            return f64::MAX;
        }
        let lens = Lens {
            k1: k[0],
            k2: if both { k[1] } else { 0.0 },
            ..base
        };
        total_rms(&usable, lens)
    };
    let start: Vec<f64> = if both { vec![0.0, 0.0] } else { vec![0.0] };
    let (k, after) = nelder_mead(&cost, &start, 0.02, 80);
    let lens = Lens {
        k1: k[0],
        k2: if both { k[1] } else { 0.0 },
        ..base
    };
    Some((lens, before, after))
}

/// a plain downhill simplex; two unknowns at most here
fn nelder_mead(
    f: &dyn Fn(&[f64]) -> f64,
    start: &[f64],
    step: f64,
    iters: usize,
) -> (Vec<f64>, f64) {
    let n = start.len();
    let mut simplex: Vec<(Vec<f64>, f64)> = (0..=n)
        .map(|i| {
            let mut p = start.to_vec();
            if i > 0 {
                p[i - 1] += step;
            }
            let v = f(&p);
            (p, v)
        })
        .collect();
    for _ in 0..iters {
        simplex.sort_by(|a, b| a.1.total_cmp(&b.1));
        let worst = simplex[n].clone();
        let centroid: Vec<f64> = (0..n)
            .map(|j| simplex[..n].iter().map(|p| p.0[j]).sum::<f64>() / n as f64)
            .collect();
        let towards = |t: f64| -> Vec<f64> {
            (0..n)
                .map(|j| centroid[j] + t * (worst.0[j] - centroid[j]))
                .collect()
        };
        let r = towards(-1.0);
        let fr = f(&r);
        if fr < simplex[0].1 {
            let e = towards(-2.0);
            let fe = f(&e);
            simplex[n] = if fe < fr { (e, fe) } else { (r, fr) };
        } else if fr < simplex[n - 1].1 {
            simplex[n] = (r, fr);
        } else {
            let c = towards(0.5);
            let fc = f(&c);
            if fc < worst.1 {
                simplex[n] = (c, fc);
            } else {
                let best = simplex[0].0.clone();
                for p in simplex.iter_mut().skip(1) {
                    p.0 = (0..n).map(|j| best[j] + 0.5 * (p.0[j] - best[j])).collect();
                    p.1 = f(&p.0);
                }
            }
        }
    }
    simplex.sort_by(|a, b| a.1.total_cmp(&b.1));
    simplex.swap_remove(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undistort_inverts_distort() {
        let lens = Lens {
            k1: -0.12,
            k2: 0.03,
            ..Lens::none((1920, 1080))
        };
        for p in [pos2(10.0, 10.0), pos2(1900.0, 1000.0), pos2(960.0, 540.0)] {
            let back = lens.undistort(lens.distort(p));
            assert!(back.distance(p) < 0.01, "{p:?} -> {back:?}");
        }
    }

    #[test]
    fn geometry_of_a_plain_scale_is_square() {
        let h = Homography([0.5, 0.0, 100.0, 0.0, 0.5, 50.0, 0.0, 0.0, 1.0]);
        let g = Geometry::of(&h, (1920, 1080), 1.0);
        assert!((g.scale - 0.5).abs() < 1e-4, "{g:?}");
        assert!(g.rotation.abs() < 1e-3 && g.skew.abs() < 1e-3, "{g:?}");
        assert!(
            g.keystone_h.abs() < 1e-3 && g.keystone_v.abs() < 1e-3,
            "{g:?}"
        );
    }

    #[test]
    fn keystone_shows_the_longer_edge() {
        // the right edge drawn nearer the camera, so longer
        let h = Homography([0.5, 0.0, 100.0, 0.0, 0.5, 50.0, -1e-4, 0.0, 1.0]);
        let g = Geometry::of(&h, (1920, 1080), 1.0);
        assert!(g.keystone_h > 5.0, "{g:?}");
        assert!(g.view > 5.0, "{g:?}");
    }

    #[test]
    fn simplex_finds_a_bowl_bottom() {
        let f = |k: &[f64]| (k[0] - 0.1).powi(2) + (k[1] + 0.05).powi(2);
        let (k, v) = nelder_mead(&f, &[0.0, 0.0], 0.02, 200);
        assert!(v < 1e-8 && (k[0] - 0.1).abs() < 1e-3, "{k:?}");
    }
}

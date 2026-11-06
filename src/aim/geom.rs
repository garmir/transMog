use egui::{pos2, Pos2};

/// a 3x3 projective map, row major, from visualiser frame pixels to
/// camera pixels
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Homography(pub [f64; 9]);

impl Homography {
    pub const IDENTITY: Homography = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);

    pub fn apply(&self, p: Pos2) -> Pos2 {
        let h = &self.0;
        let (x, y) = (f64::from(p.x), f64::from(p.y));
        let w = h[6] * x + h[7] * y + h[8];
        pos2(
            ((h[0] * x + h[1] * y + h[2]) / w) as f32,
            ((h[3] * x + h[4] * y + h[5]) / w) as f32,
        )
    }

    pub fn apply_all<const N: usize>(&self, pts: [Pos2; N]) -> [Pos2; N] {
        pts.map(|p| self.apply(p))
    }

    /// the same map scaled so the bottom right entry is 1
    pub fn normalised(&self) -> Homography {
        let k = self.0[8];
        if k.abs() < 1e-12 {
            return *self;
        }
        Homography(self.0.map(|v| v / k))
    }

    pub fn rows(&self) -> [[f64; 3]; 3] {
        let h = self.normalised().0;
        [[h[0], h[1], h[2]], [h[3], h[4], h[5]], [h[6], h[7], h[8]]]
    }

    /// camera pixels per frame pixel around `p`, from the area the map
    /// gives a small square there
    pub fn scale_at(&self, p: Pos2) -> f32 {
        let d = 4.0;
        let a = self.apply(p);
        let b = self.apply(p + egui::vec2(d, 0.0));
        let c = self.apply(p + egui::vec2(0.0, d));
        let (u, v) = (b - a, c - a);
        (u.x * v.y - u.y * v.x).abs().sqrt() / d
    }

    /// a rotation, one scale and a shift, least squares over the pairs
    pub fn similarity(pairs: &[(Pos2, Pos2)]) -> Option<Homography> {
        if pairs.len() < 2 {
            return None;
        }
        // x' = a x - b y + tx, y' = b x + a y + ty
        let mut rows = Vec::with_capacity(pairs.len() * 2);
        for (s, d) in pairs {
            let (x, y) = (f64::from(s.x), f64::from(s.y));
            rows.push((vec![x, -y, 1.0, 0.0], f64::from(d.x)));
            rows.push((vec![y, x, 0.0, 1.0], f64::from(d.y)));
        }
        let p = least_squares(&rows, 4)?;
        Some(Homography([
            p[0], -p[1], p[2], p[1], p[0], p[3], 0.0, 0.0, 1.0,
        ]))
    }

    /// six free entries, least squares over at least three pairs
    pub fn affine(pairs: &[(Pos2, Pos2)]) -> Option<Homography> {
        if pairs.len() < 3 {
            return None;
        }
        let mut rows = Vec::with_capacity(pairs.len() * 2);
        for (s, d) in pairs {
            let (x, y) = (f64::from(s.x), f64::from(s.y));
            rows.push((vec![x, y, 1.0, 0.0, 0.0, 0.0], f64::from(d.x)));
            rows.push((vec![0.0, 0.0, 0.0, x, y, 1.0], f64::from(d.y)));
        }
        let p = least_squares(&rows, 6)?;
        Some(Homography([
            p[0], p[1], p[2], p[3], p[4], p[5], 0.0, 0.0, 1.0,
        ]))
    }

    /// the full projective map, least squares over at least four pairs.
    /// both point sets are centred and scaled first, since raw pixel
    /// coordinates square to 1e13 in the normal equations
    pub fn projective(pairs: &[(Pos2, Pos2)]) -> Option<Homography> {
        if pairs.len() < 4 {
            return None;
        }
        let (ts, td) = (
            conditioner(pairs.iter().map(|p| p.0)),
            conditioner(pairs.iter().map(|p| p.1)),
        );
        let mut rows = Vec::with_capacity(pairs.len() * 2);
        for (s, d) in pairs {
            let s = ts.apply(*s);
            let d = td.apply(*d);
            let (x, y, u, v) = (
                f64::from(s.x),
                f64::from(s.y),
                f64::from(d.x),
                f64::from(d.y),
            );
            rows.push((vec![x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u));
            rows.push((vec![0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v));
        }
        let p = least_squares(&rows, 8)?;
        let h = Homography([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7], 1.0]);
        Some(td.inverse()?.then(&h.then(&ts)).normalised())
    }

    /// `self` after `first`: x -> self(first(x))
    pub fn then(&self, first: &Homography) -> Homography {
        let (a, b) = (&self.0, &first.0);
        let mut out = [0.0; 9];
        for r in 0..3 {
            for c in 0..3 {
                out[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
            }
        }
        Homography(out)
    }

    pub fn inverse(&self) -> Option<Homography> {
        let m = &self.0;
        let c = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let det = m[0] * c[0] + m[1] * c[3] + m[2] * c[6];
        if det.abs() < 1e-12 {
            return None;
        }
        Some(Homography(c.map(|v| v / det)))
    }

    /// the largest distance between a mapped source point and its pair
    pub fn residual(&self, pairs: &[(Pos2, Pos2)]) -> f32 {
        pairs
            .iter()
            .map(|(s, d)| self.apply(*s).distance(*d))
            .fold(0.0, f32::max)
    }
}

/// moves a point set to its centroid and scales it to a mean distance
/// of sqrt 2
fn conditioner(points: impl Iterator<Item = Pos2> + Clone) -> Homography {
    let n = points.clone().count().max(1) as f64;
    let (sx, sy) = points.clone().fold((0.0, 0.0), |(x, y), p| {
        (x + f64::from(p.x), y + f64::from(p.y))
    });
    let (cx, cy) = (sx / n, sy / n);
    let mean = points
        .map(|p| ((f64::from(p.x) - cx).powi(2) + (f64::from(p.y) - cy).powi(2)).sqrt())
        .sum::<f64>()
        / n;
    let k = if mean > 1e-9 { 2f64.sqrt() / mean } else { 1.0 };
    Homography([k, 0.0, -k * cx, 0.0, k, -k * cy, 0.0, 0.0, 1.0])
}

/// solves the normal equations of `rows` (coefficients, right-hand
/// side) for `n` unknowns by gaussian elimination with partial pivoting
fn least_squares(rows: &[(Vec<f64>, f64)], n: usize) -> Option<Vec<f64>> {
    let mut a = vec![vec![0.0; n + 1]; n];
    for (coef, rhs) in rows {
        for i in 0..n {
            for j in 0..n {
                a[i][j] += coef[i] * coef[j];
            }
            a[i][n] += coef[i] * rhs;
        }
    }
    for col in 0..n {
        let pivot = (col..n).max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let pivot_row = a[col].clone();
        for (r, row) in a.iter_mut().enumerate() {
            if r == col {
                continue;
            }
            let f = row[col] / pivot_row[col];
            if f != 0.0 {
                for (v, p) in row.iter_mut().zip(&pivot_row).skip(col) {
                    *v -= f * p;
                }
            }
        }
    }
    Some((0..n).map(|i| a[i][n] / a[i][i]).collect())
}

/// angle of the line from `a` to `b` in degrees, positive when `b` sits
/// lower in the picture (camera y points down)
pub fn slope_deg(a: Pos2, b: Pos2) -> f32 {
    (b.y - a.y).atan2(b.x - a.x).to_degrees()
}

/// axis-aligned bounds of a point set
pub fn bounds(points: impl IntoIterator<Item = Pos2>) -> Option<egui::Rect> {
    let mut it = points.into_iter();
    let first = it.next()?;
    Some(it.fold(egui::Rect::from_min_max(first, first), |r, p| {
        r.union(egui::Rect::from_min_max(p, p))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> [Pos2; 4] {
        [
            pos2(0.0, 0.0),
            pos2(100.0, 0.0),
            pos2(100.0, 100.0),
            pos2(0.0, 100.0),
        ]
    }

    #[test]
    fn projective_recovers_a_known_map() {
        let h = Homography([0.5, 0.05, 300.0, -0.02, 0.48, 120.0, 1e-5, -2e-5, 1.0]);
        let src = [
            pos2(0.0, 0.0),
            pos2(1920.0, 0.0),
            pos2(1920.0, 1080.0),
            pos2(0.0, 1080.0),
            pos2(400.0, 300.0),
        ];
        let pairs: Vec<_> = src.iter().map(|&p| (p, h.apply(p))).collect();
        let fit = Homography::projective(&pairs).unwrap();
        assert!(fit.residual(&pairs) < 0.01, "{fit:?}");
    }

    #[test]
    fn similarity_reads_rotation_and_scale() {
        let a = 10f32.to_radians();
        let pairs: Vec<_> = square()
            .iter()
            .map(|&p| {
                let (s, c) = (0.5 * a.sin(), 0.5 * a.cos());
                (p, pos2(c * p.x - s * p.y + 40.0, s * p.x + c * p.y + 7.0))
            })
            .collect();
        let h = Homography::similarity(&pairs).unwrap();
        assert!(h.residual(&pairs) < 1e-3);
        assert!((h.scale_at(pos2(50.0, 50.0)) - 0.5).abs() < 1e-3);
        let top = h.apply_all([square()[0], square()[1]]);
        assert!((slope_deg(top[0], top[1]) - 10.0).abs() < 1e-3);
    }

    #[test]
    fn inverse_undoes() {
        let h = Homography([0.5, 0.05, 300.0, -0.02, 0.48, 120.0, 1e-5, -2e-5, 1.0]);
        let p = pos2(123.0, 456.0);
        let back = h.inverse().unwrap().apply(h.apply(p));
        assert!(back.distance(p) < 1e-3);
    }
}

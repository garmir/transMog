use egui::{pos2, vec2, Pos2, Vec2};

use crate::aim::luma::Sample;

/// a fiducial's white frame to sub-pixel precision: the outer corners,
/// where the white meets what surrounds the marker, and the inner ones,
/// where it meets the black border a module in. both top left, top
/// right, bottom right, bottom left, in camera pixels
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Corners {
    pub outer: [Pos2; 4],
    pub inner: [Pos2; 4],
    /// rms distance of the edge points from their fitted lines
    pub rms: f32,
}

/// step along a profile, camera pixels
const STEP: f32 = 0.25;
/// an edge needs this much grey between its two sides
const MIN_STEP: f32 = 12.0;

/// one straight edge: a point on it and its unit direction
#[derive(Clone, Copy, Debug)]
struct Line {
    at: Pos2,
    dir: Vec2,
}

impl Line {
    fn distance(&self, p: Pos2) -> f32 {
        let d = p - self.at;
        (d.x * self.dir.y - d.y * self.dir.x).abs()
    }

    fn meet(&self, other: &Line) -> Option<Pos2> {
        let den = self.dir.x * other.dir.y - self.dir.y * other.dir.x;
        if den.abs() < 1e-3 {
            return None;
        }
        let d = other.at - self.at;
        let t = (d.x * other.dir.y - d.y * other.dir.x) / den;
        Some(self.at + self.dir * t)
    }
}

/// total least squares through the points, then again without the ones
/// further than a pixel (or three times the median) from the first fit;
/// a stray bright pixel beside the marker moves only its own profile
fn fit_line(points: &[Pos2]) -> Option<(Line, Vec<f32>)> {
    let first = tls(points)?;
    let mut d: Vec<f32> = points.iter().map(|p| first.distance(*p)).collect();
    let mut sorted = d.clone();
    sorted.sort_by(f32::total_cmp);
    let cut = (3.0 * sorted[sorted.len() / 2]).max(1.0);
    let kept: Vec<Pos2> = points
        .iter()
        .zip(&d)
        .filter(|(_, &e)| e <= cut)
        .map(|(p, _)| *p)
        .collect();
    let line = if kept.len() >= 3 && kept.len() < points.len() {
        tls(&kept)?
    } else {
        first
    };
    d = kept.iter().map(|p| line.distance(*p)).collect();
    Some((line, d))
}

fn tls(points: &[Pos2]) -> Option<Line> {
    if points.len() < 3 {
        return None;
    }
    let n = points.len() as f32;
    let c = points.iter().fold(Vec2::ZERO, |a, p| a + p.to_vec2()) / n;
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for p in points {
        let d = p.to_vec2() - c;
        sxx += d.x * d.x;
        sxy += d.x * d.y;
        syy += d.y * d.y;
    }
    // the principal axis of the scatter
    let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    Some(Line {
        at: c.to_pos2(),
        dir: vec2(angle.cos(), angle.sin()),
    })
}

/// where the profile from `from` along `dir` first falls through the
/// level half way between its value at `s_hi` (the brighter side, max
/// over `hi`) and at the far end (min over `lo`); `s` counts pixels
/// along `dir`
fn crossing<S: Sample>(
    s: &S,
    from: Pos2,
    dir: Vec2,
    hi: (f32, f32),
    lo: (f32, f32),
) -> Option<f32> {
    let at = |t: f32| {
        let p = from + dir * t;
        s.at(p.x, p.y)
    };
    let steps = |(a, b): (f32, f32)| {
        let n = ((b - a) / STEP).ceil().max(1.0) as usize;
        (0..=n).map(move |i| a + (b - a) * i as f32 / n as f32)
    };
    let mut peak = (f32::MIN, hi.0);
    for t in steps(hi) {
        let g = at(t)?;
        if g > peak.0 {
            peak = (g, t);
        }
    }
    let mut floor = f32::MAX;
    for t in steps(lo) {
        floor = floor.min(at(t)?);
    }
    if peak.0 - floor < MIN_STEP {
        return None;
    }
    let thr = (peak.0 + floor) / 2.0;
    let end = lo.1;
    let sign = (end - peak.1).signum();
    let mut prev = (peak.1, peak.0);
    let mut t = peak.1;
    while (end - t) * sign > 0.0 {
        t += STEP * sign;
        let g = at(t)?;
        if g < thr {
            let f = (prev.1 - thr) / (prev.1 - g).max(1e-6);
            return Some(prev.0 + (t - prev.0) * f);
        }
        prev = (t, g);
    }
    None
}

/// the white frame of a marker refined from `coarse`, its outer corners
/// to within a pixel or so (as the flood fill gives them). each side is
/// cut by profiles across it, the grey crossings are fitted with lines
/// and the lines meet in the corners. none when a side has too few
/// clean crossings or the result moved more than half a module
pub fn refine<S: Sample>(s: &S, coarse: [Pos2; 4]) -> Option<Corners> {
    let side = (0..4)
        .map(|i| coarse[i].distance(coarse[(i + 1) % 4]))
        .sum::<f32>()
        / 4.0;
    let u = side / 8.0;
    if u < 1.5 {
        return None;
    }
    // the corners go clockwise on screen, so the outward normal of the
    // side from a to b is its direction turned a quarter anticlockwise
    let count = ((side / 1.5) as usize).clamp(6, 48);
    let reach = (1.2 * u).max(3.0);
    let mut outer_lines = Vec::with_capacity(4);
    let mut inner_lines = Vec::with_capacity(4);
    let mut residuals = Vec::new();
    for i in 0..4 {
        let (a, b) = (coarse[i], coarse[(i + 1) % 4]);
        let along = (b - a).normalized();
        let out = vec2(along.y, -along.x);
        let mut outer_pts = Vec::with_capacity(count);
        let mut inner_pts = Vec::with_capacity(count);
        for k in 0..count {
            // keep clear of the corners, where the next side's edge
            // crosses the profile
            let t = 0.2 + 0.6 * (k as f32 + 0.5) / count as f32;
            let base = a + (b - a) * t;
            // the white strip is the first module inside the outline
            let white = (-0.9 * u, -0.1 * u);
            if let Some(d) = crossing(s, base, out, white, (0.4 * u, reach)) {
                outer_pts.push(base + out * d);
            }
            if let Some(d) = crossing(s, base, -out, (0.1 * u, 0.9 * u), (1.3 * u, 1.8 * u)) {
                inner_pts.push(base - out * d);
            }
        }
        if outer_pts.len() < count / 2 || inner_pts.len() < count / 2 {
            return None;
        }
        let (lo, ro) = fit_line(&outer_pts)?;
        let (li, ri) = fit_line(&inner_pts)?;
        residuals.extend(ro);
        residuals.extend(ri);
        outer_lines.push(lo);
        inner_lines.push(li);
    }
    let meet = |lines: &[Line]| -> Option<[Pos2; 4]> {
        let mut out = [Pos2::ZERO; 4];
        for (i, c) in out.iter_mut().enumerate() {
            *c = lines[(i + 3) % 4].meet(&lines[i])?;
        }
        Some(out)
    };
    let outer = meet(&outer_lines)?;
    let inner = meet(&inner_lines)?;
    let moved = outer
        .iter()
        .zip(&coarse)
        .map(|(p, q)| p.distance(*q))
        .fold(0.0, f32::max);
    if moved > 0.5 * u + 1.5 {
        return None;
    }
    let rms = (residuals.iter().map(|e| e * e).sum::<f32>() / residuals.len().max(1) as f32).sqrt();
    Some(Corners { outer, inner, rms })
}

/// the inner corners of the white frame in frame pixels, one module in
/// from `outer` (a square of 8 modules)
pub fn inset(outer: [Pos2; 4]) -> [Pos2; 4] {
    let u = (outer[1].x - outer[0].x) / 8.0;
    [
        pos2(outer[0].x + u, outer[0].y + u),
        pos2(outer[1].x - u, outer[1].y + u),
        pos2(outer[2].x - u, outer[2].y - u),
        pos2(outer[3].x + u, outer[3].y - u),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_meet_where_they_cross() {
        let a = Line {
            at: pos2(0.0, 5.0),
            dir: vec2(1.0, 0.0),
        };
        let b = Line {
            at: pos2(3.0, 0.0),
            dir: vec2(0.0, 1.0),
        };
        assert!(a.meet(&b).unwrap().distance(pos2(3.0, 5.0)) < 1e-5);
    }

    #[test]
    fn line_fit_drops_a_stray_point() {
        let mut pts: Vec<Pos2> = (0..10)
            .map(|i| pos2(i as f32, 2.0 + 0.1 * i as f32))
            .collect();
        pts.push(pos2(5.0, 9.0));
        let (line, _) = fit_line(&pts).unwrap();
        assert!(line.distance(pos2(20.0, 4.0)) < 1e-3, "{line:?}");
    }
}

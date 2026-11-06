use egui::Pos2;

use super::edges::{self, Corners};
use super::map::{self, Points};
use crate::aim::detect::Square;
use crate::aim::luma::Sample;
use crate::aim::marks::FIDS;
use crate::aim::{Name, Screen};

/// a corner that moves less than this between frames is noise and is
/// smoothed; a larger move is the camera or the framing and is taken
/// as it is
const JUMP: f32 = 1.0;
const ALPHA: f32 = 0.3;
/// frames a marker may go unrefined before it is dropped
const MISSES: u32 = 3;
/// how far from its predicted place, in marker sides, a missed marker
/// is looked for: the mini's two left markers, 20 % keystoned, put the
/// right-hand ones over two sides off
const REACH: f32 = 3.0;

#[derive(Clone, Debug)]
pub struct Marker {
    /// index into FIDS
    pub fid: usize,
    pub corners: Corners,
    pub misses: u32,
}

/// one screen's markers followed from frame to frame
#[derive(Clone, Debug)]
pub struct ScreenTrack {
    pub name: Name,
    pub markers: Vec<Marker>,
    /// markers the last detection found; fewer now means look again
    pub found: usize,
}

fn smooth(old: &mut [Pos2; 4], new: [Pos2; 4]) {
    let far = old.iter().zip(&new).any(|(a, b)| a.distance(*b) > JUMP);
    for (a, b) in old.iter_mut().zip(new) {
        *a = if far { b } else { *a + (b - *a) * ALPHA };
    }
}

impl ScreenTrack {
    /// the markers a detection saw on this screen, refined, then any
    /// other bright square that sits where the refined corners put a
    /// marker the detection missed; none when no marker refines.
    /// `squares` is what the detection searched. two markers stacked at
    /// the left fix only an affine map, which puts the right-hand ones
    /// tens of pixels off on a keystoned screen, too far for the
    /// detection's own match
    pub fn start<S: Sample>(s: &S, screen: &Screen, squares: &[Square]) -> Option<ScreenTrack> {
        let mut markers: Vec<Marker> = screen
            .fids
            .iter()
            .filter(|f| f.seen)
            .filter_map(|f| {
                let fid = FIDS.iter().position(|d| d.id == f.id)?;
                // the square itself when one is there: the detection's
                // fit can sit a pixel or two off a small marker's corners
                let at = centre(&f.quad);
                let seed = squares
                    .iter()
                    .filter(|q| q.centre().distance(at) < 0.3 * q.side)
                    .min_by(|a, b| a.centre().distance(at).total_cmp(&b.centre().distance(at)))
                    .map_or(f.quad, |q| q.corners);
                let corners = edges::refine(s, seed)?;
                Some(Marker {
                    fid,
                    corners,
                    misses: 0,
                })
            })
            .collect();
        if markers.is_empty() {
            return None;
        }
        let mut tried = Vec::new();
        while let Some(m) = next_marker(s, &markers, &mut tried, squares) {
            markers.push(m);
        }
        Some(ScreenTrack {
            name: screen.name,
            found: markers.len(),
            markers,
        })
    }

    /// refines every marker from where it was; false once a marker has
    /// been dropped and the screen should be looked for again
    pub fn update<S: Sample>(&mut self, s: &S) -> bool {
        for m in &mut self.markers {
            match edges::refine(s, m.corners.outer) {
                Some(c) => {
                    smooth(&mut m.corners.outer, c.outer);
                    smooth(&mut m.corners.inner, c.inner);
                    m.corners.rms = c.rms;
                    m.misses = 0;
                }
                None => m.misses += 1,
            }
        }
        self.markers.retain(|m| m.misses < MISSES);
        !self.markers.is_empty() && self.markers.len() >= self.found
    }

    pub fn ids(&self) -> Vec<u8> {
        let mut ids: Vec<u8> = self.markers.iter().map(|m| FIDS[m.fid].id).collect();
        ids.sort_unstable();
        ids
    }

    /// each marker's outer and inner corners against their frame pixels;
    /// a marker that missed this frame keeps its last corners
    pub fn points(&self) -> Points {
        points_of(&self.markers)
    }
}

/// the unmatched square nearest to where the markers so far put one of
/// the others, refined and kept when the fit with it stays tight.
/// `tried` holds the (marker, square) pairs already turned down
fn next_marker<S: Sample>(
    s: &S,
    markers: &[Marker],
    tried: &mut Vec<(usize, usize)>,
    squares: &[Square],
) -> Option<Marker> {
    loop {
        let pts = points_of(markers);
        let h = map::fit_plane(&pts.pairs, pts.markers)?;
        let taken = |sq: &Square| {
            markers
                .iter()
                .any(|m| sq.centre().distance(centre(&m.corners.outer)) < 0.5 * sq.side)
        };
        let mut best: Option<(f32, usize, usize)> = None;
        for fid in (0..FIDS.len()).filter(|f| markers.iter().all(|m| m.fid != *f)) {
            let quad = h.apply_all(FIDS[fid].rect.corners());
            let side = (0..4)
                .map(|i| quad[i].distance(quad[(i + 1) % 4]))
                .sum::<f32>()
                / 4.0;
            let at = centre(&quad);
            for (k, sq) in squares.iter().enumerate() {
                if tried.contains(&(fid, k)) || taken(sq) {
                    continue;
                }
                let d = sq.centre().distance(at);
                let ratio = sq.side / side.max(1.0);
                let near = d / side.max(1.0);
                if near < REACH && (0.5..2.0).contains(&ratio) && best.is_none_or(|b| near < b.0) {
                    best = Some((near, fid, k));
                }
            }
        }
        let (_, fid, k) = best?;
        tried.push((fid, k));
        let Some(corners) = edges::refine(s, squares[k].corners) else {
            continue;
        };
        let m = Marker {
            fid,
            corners,
            misses: 0,
        };
        let mut with: Vec<Marker> = markers.to_vec();
        with.push(m.clone());
        let pts = points_of(&with);
        let Some(h) = map::fit_plane(&pts.pairs, pts.markers) else {
            continue;
        };
        // a square in the wrong place, or under the wrong label, cannot
        // be fitted with the others without leaving corners pixels off
        let module = squares[k].side / 8.0;
        if h.residual(&pts.pairs) < (0.5 * module).max(1.5) {
            return Some(m);
        }
    }
}

fn centre(q: &[Pos2; 4]) -> Pos2 {
    (q.iter().fold(egui::Vec2::ZERO, |a, p| a + p.to_vec2()) / 4.0).to_pos2()
}

fn points_of(markers: &[Marker]) -> Points {
    let mut pairs = Vec::with_capacity(markers.len() * 8);
    for m in markers {
        let outer = FIDS[m.fid].rect.corners();
        pairs.extend(outer.into_iter().zip(m.corners.outer));
        pairs.extend(edges::inset(outer).into_iter().zip(m.corners.inner));
    }
    Points {
        pairs,
        markers: markers.len(),
    }
}

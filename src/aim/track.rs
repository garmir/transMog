use std::collections::VecDeque;

use super::detect::Name;

/// how long both screens must stay in view and decode before aim is ok
pub const HOLD: f64 = 3.0;
pub const RATE_OK: f32 = 0.95;
const KEEP: f64 = 5.0;

/// per screen, the stamp reads of the frames seen and since when it has
/// been wholly in view. times are seconds on any one clock
#[derive(Debug, Default)]
pub struct Track {
    reads: [VecDeque<(f64, bool)>; 2],
    in_view_since: [Option<f64>; 2],
}

impl Track {
    pub fn view(&mut self, t: f64, name: Name, in_view: bool) {
        let since = &mut self.in_view_since[name.index()];
        match (in_view, *since) {
            (true, None) => *since = Some(t),
            (false, _) => *since = None,
            _ => {}
        }
    }

    pub fn read(&mut self, t: f64, name: Name, ok: bool) {
        let reads = &mut self.reads[name.index()];
        reads.push_back((t, ok));
        while reads.front().is_some_and(|r| r.0 < t - KEEP) {
            reads.pop_front();
        }
    }

    /// decoded share and count of the reads in the last `window` seconds
    pub fn rate(&self, t: f64, name: Name, window: f64) -> Option<(f32, usize)> {
        let reads: Vec<bool> = self.reads[name.index()]
            .iter()
            .filter(|r| r.0 > t - window && r.0 <= t)
            .map(|r| r.1)
            .collect();
        if reads.is_empty() {
            return None;
        }
        let ok = reads.iter().filter(|&&r| r).count();
        Some((ok as f32 / reads.len() as f32, reads.len()))
    }

    /// both screens in view for HOLD seconds, with reads across the whole
    /// of that time and RATE_OK of them decoded
    pub fn aim_ok(&self, t: f64) -> bool {
        Name::ALL.iter().all(|&n| {
            let held = self.in_view_since[n.index()].is_some_and(|s| t - s >= HOLD);
            let covered = self.reads[n.index()]
                .iter()
                .find(|r| r.0 > t - HOLD - 0.5)
                .is_some_and(|r| r.0 <= t - HOLD + 0.5);
            let rate = self
                .rate(t, n, HOLD)
                .is_some_and(|(r, count)| r >= RATE_OK && count >= 10);
            held && covered && rate
        })
    }

    /// what aim_ok still waits for, for the panel
    pub fn waiting_for(&self, t: f64) -> Option<String> {
        if self.aim_ok(t) {
            return None;
        }
        for n in Name::ALL {
            match self.in_view_since[n.index()] {
                None => return Some(format!("{} not wholly in view", n.label())),
                Some(s) if t - s < HOLD => {
                    return Some(format!(
                        "{} in view for {:.1} of {HOLD:.0} s",
                        n.label(),
                        t - s
                    ))
                }
                _ => {}
            }
            if let Some((r, _)) = self.rate(t, n, HOLD).filter(|(r, _)| *r < RATE_OK) {
                return Some(format!(
                    "{} stamps decode {:.0}% over {HOLD:.0} s, need {:.0}%",
                    n.label(),
                    r * 100.0,
                    RATE_OK * 100.0
                ));
            }
        }
        Some("collecting stamp reads".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_three_seconds_of_good_reads() {
        let mut t = Track::default();
        for i in 0..=60 {
            let at = f64::from(i) / 30.0;
            for n in Name::ALL {
                t.view(at, n, true);
                t.read(at, n, true);
            }
        }
        assert!(!t.aim_ok(2.0));
        for i in 61..=200 {
            let at = f64::from(i) / 30.0;
            for n in Name::ALL {
                t.read(at, n, i % 10 != 0);
            }
        }
        // one read in ten failed: 90%
        assert!(!t.aim_ok(200.0 / 30.0));
        let mut ok = Track::default();
        for i in 0..=120 {
            let at = f64::from(i) / 30.0;
            for n in Name::ALL {
                ok.view(at, n, true);
                ok.read(at, n, true);
            }
        }
        assert!(ok.aim_ok(4.0));
        ok.view(4.1, Name::Mini, false);
        assert!(!ok.aim_ok(4.1));
    }
}

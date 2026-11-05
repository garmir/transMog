use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tracing::warn;

use super::detect::{self, Name, Screen};
use super::geom::bounds;
use super::hint::{self, Plan};
use super::luma::{FrameView, Luma};
use super::track::Track;
use crate::capture::FrameSource;

/// one full analysis every 250 ms; the frames between only have their
/// stamps read
const PERIOD: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_millis(8);
const JOIN_TIMEOUT: Duration = Duration::from_secs(1);
const LEVELS: usize = 8;

#[derive(Clone, Debug)]
pub struct ScreenReport {
    pub screen: Screen,
    /// decoded share and count of the frames read over the last second
    pub rate: Option<(f32, usize)>,
    pub exposure: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub size: (u32, u32),
    pub screens: Vec<ScreenReport>,
    pub plan: Plan,
    pub aim_ok: bool,
    /// what aim ok still waits for
    pub waiting: Option<String>,
    pub analysis_ms: f32,
    pub analysed: Instant,
    /// the zoom_absolute the analysis assumed
    pub zoom: i64,
    /// frames whose stamps were read since the worker started
    pub frames_read: u64,
}

impl Report {
    pub fn screen(&self, name: Name) -> Option<&ScreenReport> {
        self.screens.iter().find(|s| s.screen.name == name)
    }

    /// bounds of every screen found, in camera pixels
    pub fn union(&self) -> Option<egui::Rect> {
        bounds(self.screens.iter().flat_map(|s| s.screen.outline))
    }
}

type Wake = Box<dyn Fn() + Send + Sync>;

struct Shared {
    source: Mutex<Option<FrameSource>>,
    zoom: AtomicI64,
    stop: AtomicBool,
    report: Mutex<Option<Arc<Report>>>,
    wake: Mutex<Option<Wake>>,
}

/// the analysis thread; it stops on drop
pub struct Aim {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Aim {
    pub fn start(source: Option<FrameSource>) -> Aim {
        let shared = Arc::new(Shared {
            source: Mutex::new(source),
            zoom: AtomicI64::new(100),
            stop: AtomicBool::new(false),
            report: Mutex::new(None),
            wake: Mutex::new(None),
        });
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("aim".into())
            .spawn(move || run(&worker))
            .map_err(|e| warn!("cannot start the aim thread: {e}"))
            .ok();
        Aim { shared, thread }
    }

    /// the frames to read; follows capture restarts
    pub fn set_source(&self, source: Option<FrameSource>) {
        *self.shared.source.lock() = source;
    }

    /// zoom_absolute as read back, so a lone screen is named by its size
    /// without the zoom
    pub fn set_zoom(&self, zoom: i64) {
        self.shared.zoom.store(zoom, Ordering::Relaxed);
    }

    pub fn report(&self) -> Option<Arc<Report>> {
        self.shared.report.lock().clone()
    }

    /// called from the aim thread after every new report
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.shared.wake.lock() = Some(Box::new(wake));
    }
}

impl Drop for Aim {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + JOIN_TIMEOUT;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                warn!("aim thread did not stop in time, leaving it behind");
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = thread.join();
    }
}

fn run(shared: &Shared) {
    let base = Instant::now();
    let mut track = Track::default();
    let mut screens: Vec<Screen> = Vec::new();
    let mut last: Option<(u64, Instant)> = None;
    let mut next = Instant::now();
    let mut frames_read = 0u64;
    let mut levels: VecDeque<f32> = VecDeque::new();
    let mut level_from = None;
    while !shared.stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL);
        let Some(frame) = shared.source.lock().as_ref().and_then(FrameSource::latest) else {
            continue;
        };
        if last == Some((frame.seq, frame.received)) {
            continue;
        }
        last = Some((frame.seq, frame.received));
        let t = frame.received.duration_since(base).as_secs_f64();

        if Instant::now() < next {
            let Some(view) = FrameView::new(&frame) else {
                continue;
            };
            for name in Name::ALL {
                let ok = screens
                    .iter()
                    .find(|s| s.name == name)
                    .and_then(|s| detect::read_stamps(&view, &s.h))
                    .is_some_and(|s| s.ok());
                track.read(t, name, ok);
            }
            frames_read += 1;
            continue;
        }

        let started = Instant::now();
        next = started + PERIOD;
        let size = (frame.width, frame.height);
        let Some(luma) = Luma::from_frame(&frame) else {
            continue;
        };
        // the capture thread can recycle the buffer from here on
        drop(frame);
        let zoom = shared.zoom.load(Ordering::Relaxed);
        screens = detect::analyse(&luma, zoom as f32 / 100.0);
        for name in Name::ALL {
            let found = screens.iter().find(|s| s.name == name);
            track.view(t, name, found.is_some_and(|s| !s.clipped.any()));
            track.read(
                t,
                name,
                found.and_then(|s| s.stamps).is_some_and(|s| s.ok()),
            );
        }
        frames_read += 1;
        let mut plan = hint::plan(
            &screens,
            (size.0 as f32, size.1 as f32),
            zoom as f32 / 100.0,
        );
        // one analysis's level moves by a few degrees when only the
        // mini's left markers show, so the median of the last few is used
        if plan.level_from != level_from {
            levels.clear();
            level_from = plan.level_from;
        }
        match plan.level_deg {
            Some(deg) => {
                levels.push_back(deg);
                if levels.len() > LEVELS {
                    levels.pop_front();
                }
                let mut sorted: Vec<f32> = levels.iter().copied().collect();
                sorted.sort_by(f32::total_cmp);
                hint::set_level(&mut plan, sorted[sorted.len() / 2]);
            }
            None => levels.clear(),
        }
        let report = Report {
            size,
            screens: screens
                .iter()
                .map(|s| ScreenReport {
                    screen: s.clone(),
                    rate: track.rate(t, s.name, 1.0),
                    exposure: hint::exposure_note(s),
                })
                .collect(),
            plan,
            aim_ok: track.aim_ok(t),
            waiting: track.waiting_for(t),
            analysis_ms: started.elapsed().as_secs_f32() * 1000.0,
            analysed: Instant::now(),
            zoom,
            frames_read,
        };
        *shared.report.lock() = Some(Arc::new(report));
        if let Some(wake) = shared.wake.lock().as_ref() {
            wake();
        }
    }
}

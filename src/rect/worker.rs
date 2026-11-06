use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use egui::pos2;
use parking_lot::Mutex;
use tracing::{info, warn};

use super::map::{self, Geometry, Lens, Points};
use super::out::{Record, Writer};
use super::track::ScreenTrack;
use super::warp::{self, Tile};
use crate::aim::detect;
use crate::aim::luma::{FrameView, Luma};
use crate::aim::marks::{self, Rect, FRAME, STAMPS};
use crate::aim::Name;
use crate::capture::FrameSource;

/// tile pixels a frame pixel: the projector's native 0.49 camera px a
/// frame px, and four times the mini's
pub const DEFAULT_SCALE: f32 = 0.5;
const POLL: Duration = Duration::from_millis(3);
const JOIN_TIMEOUT: Duration = Duration::from_secs(1);
/// a full search for new screens and markers, even while tracking
const SEARCH: Duration = Duration::from_secs(2);
/// the quickest a lost screen is searched for again
const RETRY: Duration = Duration::from_millis(250);
const LENS_EVERY: u64 = 30;
/// the lens fit is taken when it lowers the corner rms by this share
const LENS_GAIN: f64 = 0.1;
const LENS_ALPHA: f64 = 0.2;
const PREVIEW_SCALE: f32 = 0.25;
const PREVIEW_EVERY: Duration = Duration::from_millis(250);
const PUBLISH_EVERY: Duration = Duration::from_millis(100);
/// frames of decode history kept for the rates
pub const HISTORY: usize = 30;
const STATS_EVERY: Duration = Duration::from_secs(30);

/// one screen in the newest frame
#[derive(Clone, Debug)]
pub struct ScreenView {
    /// without its tiles
    pub record: Record,
    /// the whole screen at a quarter scale, while a preview is asked for
    pub preview: Option<Arc<Tile>>,
    /// share of the last HISTORY frames with N, A and M decoded
    pub rates: [f32; 3],
}

#[derive(Clone, Debug)]
pub struct Report {
    pub screens: Vec<ScreenView>,
    pub lens: Lens,
    /// corner rms over the screens that fit the lens, before and after
    pub lens_fit: Option<(f64, f64)>,
    /// thread cpu per frame, mean over the last second, ms
    pub cpu_ms: f32,
    pub frames: u64,
    /// source frames that went by while the worker was busy
    pub skipped: u64,
    pub written: u64,
    pub write_errors: u64,
}

impl Report {
    pub fn screen(&self, name: Name) -> Option<&ScreenView> {
        self.screens.iter().find(|s| s.record.name == name)
    }
}

type Wake = Box<dyn Fn() + Send + Sync>;

struct Shared {
    source: Mutex<Option<FrameSource>>,
    /// where rect-<screen> goes; none writes nothing
    out: Mutex<Option<PathBuf>>,
    scale: f32,
    zoom: AtomicI64,
    preview: AtomicBool,
    fit_lens: AtomicBool,
    stop: AtomicBool,
    report: Mutex<Option<Arc<Report>>>,
    wake: Mutex<Option<Wake>>,
}

/// the rectification thread; it stops on drop
pub struct Rectify {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Rectify {
    /// `out` is the tap directory the rect-<screen> directories go in
    pub fn start(source: Option<FrameSource>, out: Option<PathBuf>, scale: f32) -> Rectify {
        let shared = Arc::new(Shared {
            source: Mutex::new(source),
            out: Mutex::new(out),
            scale: scale.clamp(0.05, 2.0),
            zoom: AtomicI64::new(100),
            preview: AtomicBool::new(false),
            fit_lens: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            report: Mutex::new(None),
            wake: Mutex::new(None),
        });
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("rectify".into())
            .spawn(move || run(&worker))
            .map_err(|e| warn!("cannot start the rectify thread: {e}"))
            .ok();
        Rectify { shared, thread }
    }

    pub fn set_source(&self, source: Option<FrameSource>) {
        *self.shared.source.lock() = source;
    }

    pub fn set_out(&self, out: Option<PathBuf>) {
        *self.shared.out.lock() = out;
    }

    pub fn writing(&self) -> bool {
        self.shared.out.lock().is_some()
    }

    pub fn set_zoom(&self, zoom: i64) {
        self.shared.zoom.store(zoom, Ordering::Relaxed);
    }

    /// whole-screen previews for the window, at 4 a second
    pub fn set_preview(&self, on: bool) {
        self.shared.preview.store(on, Ordering::Relaxed);
    }

    pub fn set_fit_lens(&self, on: bool) {
        self.shared.fit_lens.store(on, Ordering::Relaxed);
    }

    pub fn fit_lens(&self) -> bool {
        self.shared.fit_lens.load(Ordering::Relaxed)
    }

    pub fn report(&self) -> Option<Arc<Report>> {
        self.shared.report.lock().clone()
    }

    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.shared.wake.lock() = Some(Box::new(wake));
    }
}

impl Drop for Rectify {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + JOIN_TIMEOUT;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                warn!("rectify thread did not stop in time, leaving it behind");
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = thread.join();
    }
}

fn thread_cpu_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // cannot fail for this clock with a valid pointer
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    crate::capture::tap::timeval_ns(ts.tv_sec, 0) + ts.tv_nsec
}

/// the camera rows the rolling shutter is read from: each value's two
/// lines meet at its centre row, the toggles at their middles
fn mark_points() -> Vec<(&'static str, egui::Pos2)> {
    let x = STAMPS.centre().x;
    let line = |l: usize| marks::stamp_cell(l, 0).y1;
    vec![
        ("N", pos2(x, line(0))),
        ("A", pos2(x, line(2))),
        ("M", pos2(x, line(4))),
        (
            "top",
            marks::stamp_cell(0, 9).centre() + egui::vec2(0.0, 20.0),
        ),
        ("key", marks::KEY.0.centre()),
        ("mid", marks::MID.0.centre()),
        ("bottom", marks::BOTTOM.0.centre()),
        ("ruler", marks::RULER.centre()),
    ]
}

#[derive(Default)]
struct Stats {
    frames: u64,
    skipped: u64,
    cpu: VecDeque<(Instant, i64)>,
    since: Option<Instant>,
    logged_frames: u64,
    logged_cpu: i64,
}

fn run(shared: &Shared) {
    let mut tracks: Vec<ScreenTrack> = Vec::new();
    let mut last_seq: Option<u64> = None;
    let mut searched: Option<Instant> = None;
    let mut lost = true;
    let mut lens: Option<Lens> = None;
    let mut lens_fit = None;
    let mut writer: Option<(PathBuf, Writer)> = None;
    let mut history: [VecDeque<[bool; 3]>; 2] = [VecDeque::new(), VecDeque::new()];
    let mut previews: [Option<Arc<Tile>>; 2] = [None, None];
    let mut previewed: Option<Instant> = None;
    let mut published: Option<Instant> = None;
    let mut stats = Stats::default();
    let points = mark_points();
    while !shared.stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL);
        let Some((frame, meta)) = shared
            .source
            .lock()
            .as_ref()
            .and_then(FrameSource::latest_with_meta)
        else {
            continue;
        };
        if last_seq == Some(frame.seq) {
            continue;
        }
        if let Some(prev) = last_seq.filter(|&p| frame.seq > p + 1) {
            stats.skipped += frame.seq - prev - 1;
        }
        last_seq = Some(frame.seq);
        let cpu0 = thread_cpu_ns();
        let size = (frame.width, frame.height);
        let Some(view) = FrameView::new(&frame) else {
            continue;
        };
        let zoom = shared.zoom.load(Ordering::Relaxed) as f32 / 100.0;

        // follow the markers, then look again when one went or a while
        // has passed
        if !lost {
            for t in &mut tracks {
                lost |= !t.update(&view);
            }
            tracks.retain(|t| !t.markers.is_empty());
        }
        let due = searched.is_none_or(|s| {
            let age = s.elapsed();
            age >= SEARCH || (lost || tracks.is_empty()) && age >= RETRY
        });
        let mut tracked = true;
        if due {
            searched = Some(Instant::now());
            if let Some(luma) = Luma::from_frame(&frame) {
                let squares = detect::squares(&luma);
                for screen in detect::analyse(&luma, zoom) {
                    let Some(new) = ScreenTrack::start(&luma, &screen, &squares) else {
                        continue;
                    };
                    match tracks.iter_mut().find(|t| t.name == new.name) {
                        Some(old) if old.markers.len() > new.markers.len() && !lost => {}
                        Some(old) => *old = new,
                        None => tracks.push(new),
                    }
                }
                tracks.sort_by_key(|t| t.name);
                lost = false;
                tracked = false;
            }
        }

        let pts: Vec<Points> = tracks.iter().map(ScreenTrack::points).collect();
        if shared.fit_lens.load(Ordering::Relaxed) {
            if stats.frames.is_multiple_of(LENS_EVERY) {
                if let Some((fit, before, after)) = map::fit_lens(&pts, size) {
                    lens_fit = Some((before, after));
                    if after < (1.0 - LENS_GAIN) * before {
                        lens = Some(match lens {
                            Some(l) => Lens {
                                k1: l.k1 + LENS_ALPHA * (fit.k1 - l.k1),
                                k2: l.k2 + LENS_ALPHA * (fit.k2 - l.k2),
                                ..fit
                            },
                            None => fit,
                        });
                    }
                }
            }
        } else {
            lens = None;
            lens_fit = None;
        }
        let used = lens.filter(|l| l.centre == Lens::none(size).centre);
        let maps = map::fit_maps(&pts, used.unwrap_or_else(|| Lens::none(size)));

        let out_dir = shared.out.lock().clone();
        if writer.as_ref().map(|w| &w.0) != out_dir.as_ref() {
            writer = out_dir.map(|d| (d.clone(), Writer::new(&d)));
        }
        let want_preview = shared.preview.load(Ordering::Relaxed);
        let preview_now = want_preview && previewed.is_none_or(|p| p.elapsed() >= PREVIEW_EVERY);
        if preview_now {
            previewed = Some(Instant::now());
        }
        let mut views = Vec::with_capacity(tracks.len());
        for ((t, p), m) in tracks.iter().zip(&pts).zip(maps) {
            let Some(m) = m else {
                continue;
            };
            let tiles: Vec<Tile> = warp::regions()
                .into_iter()
                .map(|(name, r)| warp::warp(&view, &m, name, r, shared.scale))
                .collect();
            let stamps = marks::decode(|r: Rect| warp::level(&tiles, r).unwrap_or(0.0));
            let (rms, worst) = map::residual(&m, &p.pairs);
            let record = Record {
                name: t.name,
                seq: meta.tap_seq.unwrap_or(frame.seq),
                tap_seq: meta.tap_seq,
                frame_seq: frame.seq,
                kernel_ns: meta.kernel_ns,
                camera: size,
                map: m,
                geometry: Geometry::of(&m.h, size, zoom),
                rms,
                worst,
                ids: t.ids(),
                tracked,
                lens_fitted: used.is_some(),
                stamps: Some(stamps),
                rows: points.iter().map(|(k, f)| (*k, m.apply(*f).y)).collect(),
                tiles,
            };
            if let Some((_, w)) = writer.as_mut() {
                w.write(&record);
            }
            let hist = &mut history[t.name.index()];
            hist.push_back(stamps.values.map(|v| v.is_some()));
            if hist.len() > HISTORY {
                hist.pop_front();
            }
            let n = hist.len().max(1) as f32;
            let rates = [0, 1, 2].map(|i| hist.iter().filter(|h| h[i]).count() as f32 / n);
            if preview_now {
                previews[t.name.index()] = Some(Arc::new(warp::warp(
                    &view,
                    &m,
                    "screen",
                    FRAME,
                    PREVIEW_SCALE,
                )));
            }
            views.push(ScreenView {
                record: Record {
                    tiles: Vec::new(),
                    ..record
                },
                preview: if want_preview {
                    previews[t.name.index()].clone()
                } else {
                    None
                },
                rates,
            });
        }
        for name in Name::ALL {
            if !tracks.iter().any(|t| t.name == name) {
                history[name.index()].clear();
                previews[name.index()] = None;
            }
        }
        drop(frame);

        stats.frames += 1;
        let now = Instant::now();
        let spent = thread_cpu_ns() - cpu0;
        stats.cpu.push_back((now, spent));
        while stats
            .cpu
            .front()
            .is_some_and(|c| now.duration_since(c.0) > Duration::from_secs(1))
        {
            stats.cpu.pop_front();
        }
        stats.logged_frames += 1;
        stats.logged_cpu += spent;
        let since = *stats.since.get_or_insert(now);
        if now.duration_since(since) >= STATS_EVERY {
            info!(
                frames = stats.logged_frames,
                skipped = stats.skipped,
                cpu_ms_per_frame = stats.logged_cpu as f64 / 1e6 / stats.logged_frames.max(1) as f64,
                written = writer.as_ref().map_or(0, |w| w.1.written),
                screens = ?tracks.iter().map(|t| (t.name.key(), t.ids())).collect::<Vec<_>>(),
                k1 = used.map_or(0.0, |l| l.k1),
                k2 = used.map_or(0.0, |l| l.k2),
                "rectify"
            );
            stats.since = Some(now);
            stats.logged_frames = 0;
            stats.logged_cpu = 0;
        }
        if published.is_some_and(|p| p.elapsed() < PUBLISH_EVERY) {
            continue;
        }
        published = Some(now);
        let cpu_ms =
            stats.cpu.iter().map(|c| c.1).sum::<i64>() as f32 / 1e6 / stats.cpu.len().max(1) as f32;
        let report = Report {
            screens: views,
            lens: used.unwrap_or_else(|| Lens::none(size)),
            lens_fit,
            cpu_ms,
            frames: stats.frames,
            skipped: stats.skipped,
            written: writer.as_ref().map_or(0, |w| w.1.written),
            write_errors: writer.as_ref().map_or(0, |w| w.1.errors),
        };
        *shared.report.lock() = Some(Arc::new(report));
        if let Some(wake) = shared.wake.lock().as_ref() {
            wake();
        }
    }
}
